//! Opt-in, one-way transfer of recorded text and images over the local network.
//! The receive code contains a random key; possession of that code authorizes sending.
//!
//! Text stays on the original `CKL1` frame (`clipkeep-lan-text-v1`), so a peer from before
//! image support still accepts it. Images use `CKL2` (`clipkeep-lan-image-v1`). A text-only
//! peer rejects that magic before reading the body, so an image is not saved as text and
//! cannot make it allocate the payload. Windows and macOS share this module.

use crate::{Core, changed, clipboard};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::{
    ChaCha20Poly1305, Key, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

const PORT: u16 = 45788;
const MAGIC: &[u8; 4] = b"CKL1";
const IMAGE_MAGIC: &[u8; 4] = b"CKL2";
const AAD: &[u8] = b"clipkeep-lan-text-v1";
const IMAGE_AAD: &[u8] = b"clipkeep-lan-image-v1";
const ACK_AAD: &[u8] = b"clipkeep-lan-ack-v1";
const MAX_CIPHER_BYTES: usize = clipboard::MAX_TEXT_BYTES + 16;
/// Largest PNG payload. The ciphertext cap is this plus the 16-byte Poly1305 tag.
/// A screenshot PNG fits well under it; anything larger is refused before it is sent.
pub const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_IMAGE_CIPHER_BYTES: usize = MAX_IMAGE_BYTES + 16;
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
const TEXT_IO_TIMEOUT: Duration = Duration::from_secs(4);
const IMAGE_IO_TIMEOUT: Duration = Duration::from_secs(30);
/// Image bodies are read in short slices so turning reception off does not wait out the full transfer budget.
const IMAGE_READ_SLICE: Duration = Duration::from_millis(200);
const CODE_PREFIX: &str = "cklan1://";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Peer {
    name: String,
    address: String,
    key: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    receive_enabled: bool,
    receive_key: String,
    peers: Vec<Peer>,
}

impl Config {
    fn fresh() -> Self {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).expect("Windows secure random source unavailable");
        Self {
            receive_enabled: false,
            receive_key: URL_SAFE_NO_PAD.encode(key),
            peers: Vec::new(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerView {
    pub name: String,
    pub address: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalCode {
    pub address: String,
    pub code: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanState {
    pub receive_enabled: bool,
    pub local_codes: Vec<LocalCode>,
    pub peers: Vec<PeerView>,
    /// PNG payload cap advertised to the UI, matching [`MAX_IMAGE_BYTES`].
    pub max_image_bytes: usize,
}

struct Worker {
    stop: Arc<AtomicBool>,
    join: JoinHandle<()>,
}

pub struct LanService {
    path: PathBuf,
    config: Mutex<Config>,
    operation: Mutex<()>,
    worker: Mutex<Option<Worker>>,
}

fn decode_key(encoded: &str) -> Result<[u8; 32], String> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| "配对码无效")?;
    bytes.try_into().map_err(|_| "配对码无效".into())
}

fn allowed_ip(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_link_local() || ip.is_loopback()
}

fn parse_code(code: &str) -> Result<(SocketAddrV4, String), String> {
    let body = code
        .trim()
        .strip_prefix(CODE_PREFIX)
        .ok_or("配对码格式无效")?;
    let (address, key) = body.split_once('/').ok_or("配对码格式无效")?;
    let address: SocketAddrV4 = address.parse().map_err(|_| "配对地址无效")?;
    if !allowed_ip(*address.ip()) || address.port() != PORT {
        return Err("仅支持本地网络 IPv4 地址和默认端口".into());
    }
    decode_key(key)?;
    Ok((address, key.to_owned()))
}

fn save(path: &Path, config: &Config) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    crate::atomic_file::save(path, &json).map_err(|e| format!("保存局域网设置失败：{e}"))
}

impl LanService {
    pub fn new(data_dir: &Path) -> Self {
        let path = data_dir.join("lan.json");
        let config = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Config>(&bytes).ok())
            .filter(|c| decode_key(&c.receive_key).is_ok())
            .unwrap_or_else(|| {
                if path.exists() {
                    let _ = std::fs::copy(&path, path.with_extension("json.bad"));
                }
                let c = Config::fresh();
                if let Err(e) = save(&path, &c) {
                    eprintln!("{e}");
                }
                c
            });
        Self {
            path,
            config: Mutex::new(config),
            operation: Mutex::new(()),
            worker: Mutex::new(None),
        }
    }

    pub fn state(&self) -> Result<LanState, String> {
        let config = self.config.lock().map_err(|e| e.to_string())?.clone();
        let mut ips: Vec<Ipv4Addr> = local_ip_address::list_afinet_netifas()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, ip)| match ip {
                IpAddr::V4(ip) if allowed_ip(ip) && !ip.is_loopback() => Some(ip),
                _ => None,
            })
            .collect();
        ips.sort_unstable();
        ips.dedup();
        Ok(LanState {
            receive_enabled: config.receive_enabled,
            local_codes: ips
                .into_iter()
                .map(|ip| {
                    let address = SocketAddrV4::new(ip, PORT).to_string();
                    LocalCode {
                        code: format!("{CODE_PREFIX}{address}/{}", config.receive_key),
                        address,
                    }
                })
                .collect(),
            peers: config
                .peers
                .iter()
                .map(|p| PeerView {
                    name: p.name.clone(),
                    address: p.address.clone(),
                })
                .collect(),
            max_image_bytes: MAX_IMAGE_BYTES,
        })
    }

    pub fn restore(&self, app: &AppHandle) {
        let enabled = self.config.lock().is_ok_and(|c| c.receive_enabled);
        if enabled && let Err(e) = self.start(app) {
            eprintln!("LAN receiver could not start: {e}");
            if let Ok(mut c) = self.config.lock() {
                c.receive_enabled = false;
                let _ = save(&self.path, &c);
            }
        }
    }

    fn start(&self, app: &AppHandle) -> Result<(), String> {
        let key = decode_key(&self.config.lock().map_err(|e| e.to_string())?.receive_key)?;
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, PORT))
            .map_err(|e| format!("无法监听端口 {PORT}：{e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let app = app.clone();
        let join = thread::spawn(move || receive_loop(listener, key, app, thread_stop));
        *self.worker.lock().map_err(|e| e.to_string())? = Some(Worker { stop, join });
        Ok(())
    }

    fn stop(&self) -> Result<(), String> {
        if let Some(worker) = self.worker.lock().map_err(|e| e.to_string())?.take() {
            worker.stop.store(true, Ordering::SeqCst);
            let _ = worker.join.join();
        }
        Ok(())
    }

    pub fn set_receive(&self, app: &AppHandle, enabled: bool) -> Result<LanState, String> {
        let _guard = self.operation.lock().map_err(|e| e.to_string())?;
        let mut next = self.config.lock().map_err(|e| e.to_string())?.clone();
        if next.receive_enabled != enabled {
            if enabled {
                self.start(app)?;
            } else {
                self.stop()?;
            }
            next.receive_enabled = enabled;
            if let Err(e) = save(&self.path, &next) {
                if enabled {
                    let _ = self.stop();
                } else {
                    let _ = self.start(app);
                }
                return Err(e);
            }
            *self.config.lock().map_err(|e| e.to_string())? = next;
            let _ = app.emit("lan-state-changed", ());
        }
        self.state()
    }

    pub fn add_peer(&self, name: &str, code: &str) -> Result<LanState, String> {
        let _guard = self.operation.lock().map_err(|e| e.to_string())?;
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 40 {
            return Err("设备名称需为 1–40 个字符".into());
        }
        let (address, key) = parse_code(code)?;
        let mut config = self.config.lock().map_err(|e| e.to_string())?.clone();
        let address = address.to_string();
        if let Some(peer) = config.peers.iter_mut().find(|p| p.address == address) {
            peer.name = name.to_owned();
            peer.key = key;
        } else {
            config.peers.push(Peer {
                name: name.to_owned(),
                address,
                key,
            });
        }
        save(&self.path, &config)?;
        *self.config.lock().map_err(|e| e.to_string())? = config;
        self.state()
    }

    pub fn remove_peer(&self, address: &str) -> Result<LanState, String> {
        let _guard = self.operation.lock().map_err(|e| e.to_string())?;
        let mut config = self.config.lock().map_err(|e| e.to_string())?.clone();
        config.peers.retain(|p| p.address != address);
        save(&self.path, &config)?;
        *self.config.lock().map_err(|e| e.to_string())? = config;
        self.state()
    }

    pub fn rotate_code(&self, app: &AppHandle) -> Result<LanState, String> {
        let _guard = self.operation.lock().map_err(|e| e.to_string())?;
        let mut new_key = [0u8; 32];
        getrandom::fill(&mut new_key).map_err(|e| e.to_string())?;
        let enabled = self
            .config
            .lock()
            .map_err(|e| e.to_string())?
            .receive_enabled;
        if enabled {
            self.stop()?;
        }
        let mut config = self.config.lock().map_err(|e| e.to_string())?.clone();
        config.receive_key = URL_SAFE_NO_PAD.encode(new_key);
        if let Err(e) = save(&self.path, &config) {
            if enabled {
                let _ = self.start(app);
            }
            return Err(e);
        }
        *self.config.lock().map_err(|e| e.to_string())? = config;
        if enabled && let Err(e) = self.start(app) {
            if let Ok(mut current) = self.config.lock() {
                current.receive_enabled = false;
                let _ = save(&self.path, &current);
            }
            return Err(e);
        }
        self.state()
    }

    pub fn send_text(&self, address: &str, text: &str) -> Result<(), String> {
        if text.trim().is_empty() || text.len() > clipboard::MAX_TEXT_BYTES {
            return Err("只能传送不超过 2 MB 的非空文本".into());
        }
        let (target, key) = self.paired_target(address)?;
        let mut stream = connect(&target)?;
        send_stream(&mut stream, &key, text)
    }

    pub fn send_image(&self, address: &str, png: &[u8]) -> Result<(), String> {
        validate_png_for_send(png)?;
        let (target, key) = self.paired_target(address)?;
        let mut stream = connect(&target)?;
        send_image_stream(&mut stream, &key, png)
    }

    fn paired_target(&self, address: &str) -> Result<(SocketAddr, [u8; 32]), String> {
        let peer = self
            .config
            .lock()
            .map_err(|e| e.to_string())?
            .peers
            .iter()
            .find(|p| p.address == address)
            .cloned()
            .ok_or("设备未配对")?;
        let key = decode_key(&peer.key)?;
        let target: SocketAddr = peer.address.parse().map_err(|_| "设备地址无效")?;
        if !matches!(target, SocketAddr::V4(a) if allowed_ip(*a.ip()) && a.port() == PORT) {
            return Err("仅支持本地网络 IPv4 地址和默认端口".into());
        }
        Ok((target, key))
    }
}

fn connect(target: &SocketAddr) -> Result<TcpStream, String> {
    TcpStream::connect_timeout(target, Duration::from_secs(3))
        .map_err(|e| format!("连接设备失败：{e}"))
}

fn set_io_timeout(stream: &mut TcpStream, timeout: Duration) -> Result<(), String> {
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())
}

fn send_stream(stream: &mut TcpStream, key: &[u8; 32], text: &str) -> Result<(), String> {
    set_io_timeout(stream, TEXT_IO_TIMEOUT)?;
    let frame = encrypt_frame(key, text)?;
    stream
        .write_all(&frame)
        .map_err(|e| format!("发送失败：{e}"))?;
    let mut ack = [0u8; 30];
    stream
        .read_exact(&mut ack)
        .map_err(|e| format!("设备未确认接收：{e}"))?;
    verify_ack(key, &ack)
}

fn send_image_stream(stream: &mut TcpStream, key: &[u8; 32], png: &[u8]) -> Result<(), String> {
    validate_png_for_send(png)?;
    set_io_timeout(stream, IMAGE_IO_TIMEOUT)?;
    let frame = encrypt_image_frame(key, png)?;
    stream
        .write_all(&frame)
        .map_err(|e| format!("发送图片失败：{e}。若对方是只支持文本的旧版本，它会直接拒绝图片"))?;
    let mut ack = [0u8; 30];
    stream.read_exact(&mut ack).map_err(|_| {
        "对方未确认接收图片。请确认对方已开启接收；只支持文本的旧版本会拒绝图片".to_string()
    })?;
    verify_ack(key, &ack)
}

fn image_limit_error() -> String {
    format!(
        "只能传送不超过 {} MB 的图片",
        MAX_IMAGE_BYTES / (1024 * 1024)
    )
}

fn reject_image_size(len: usize) -> Result<(), String> {
    if len == 0 {
        Err("图片是空的".into())
    } else if len > MAX_IMAGE_BYTES {
        Err(image_limit_error())
    } else {
        Ok(())
    }
}

fn validate_png_for_send(png: &[u8]) -> Result<(), String> {
    reject_image_size(png.len())?;
    if !png.starts_with(PNG_MAGIC) {
        return Err("图片格式无效".into());
    }
    Ok(())
}

fn seal(magic: &[u8; 4], aad: &[u8], key: &[u8; 32], plain: &[u8]) -> Result<Vec<u8>, String> {
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
    let cipher = ChaCha20Poly1305::new(&Key::from(*key));
    let ciphertext = cipher
        .encrypt(&Nonce::from(nonce), Payload { msg: plain, aad })
        .map_err(|_| "加密失败")?;
    let mut frame = Vec::with_capacity(20 + ciphertext.len());
    frame.extend_from_slice(magic);
    frame.extend_from_slice(&nonce);
    frame.extend_from_slice(&(ciphertext.len() as u32).to_be_bytes());
    frame.extend_from_slice(&ciphertext);
    Ok(frame)
}

fn encrypt_frame(key: &[u8; 32], text: &str) -> Result<Vec<u8>, String> {
    seal(MAGIC, AAD, key, text.as_bytes())
}

fn encrypt_image_frame(key: &[u8; 32], png: &[u8]) -> Result<Vec<u8>, String> {
    validate_png_for_send(png)?;
    seal(IMAGE_MAGIC, IMAGE_AAD, key, png)
}

fn make_ack(key: &[u8; 32]) -> Result<[u8; 30], String> {
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
    let cipher = ChaCha20Poly1305::new(&Key::from(*key));
    let receipt = cipher
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: b"OK",
                aad: ACK_AAD,
            },
        )
        .map_err(|_| "确认加密失败")?;
    let mut ack = [0u8; 30];
    ack[..12].copy_from_slice(&nonce);
    ack[12..].copy_from_slice(&receipt);
    Ok(ack)
}

fn verify_ack(key: &[u8; 32], ack: &[u8; 30]) -> Result<(), String> {
    let cipher = ChaCha20Poly1305::new(&Key::from(*key));
    let nonce: [u8; 12] = ack[..12].try_into().unwrap();
    let receipt = cipher
        .decrypt(
            &Nonce::from(nonce),
            Payload {
                msg: &ack[12..],
                aad: ACK_AAD,
            },
        )
        .map_err(|_| "设备确认无效")?;
    if receipt != b"OK" {
        return Err("设备拒绝了内容".into());
    }
    Ok(())
}

fn receive_loop(listener: TcpListener, key: [u8; 32], app: AppHandle, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, addr)) => {
                let allowed = matches!(addr.ip(), IpAddr::V4(ip) if allowed_ip(ip));
                if !allowed {
                    continue;
                }
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                if let Err(e) = receive_one(&mut stream, &key, &app, &stop)
                    && !stop.load(Ordering::SeqCst)
                {
                    eprintln!("LAN receive rejected: {e}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(40))
            }
            Err(e) => {
                eprintln!("LAN listener error: {e}");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

#[derive(Debug)]
enum Incoming {
    Text(String),
    Image(Vec<u8>),
}

struct FrameHeader {
    magic: [u8; 4],
    nonce: [u8; 12],
    len: usize,
}

fn read_frame_header(reader: &mut impl Read) -> Result<FrameHeader, String> {
    let mut header = [0u8; 20];
    reader.read_exact(&mut header).map_err(|e| e.to_string())?;
    let magic: [u8; 4] = header[..4].try_into().unwrap();
    let nonce: [u8; 12] = header[4..16].try_into().unwrap();
    let len = u32::from_be_bytes(header[16..20].try_into().unwrap()) as usize;
    let max = if &magic == IMAGE_MAGIC {
        MAX_IMAGE_CIPHER_BYTES
    } else if &magic == MAGIC {
        MAX_CIPHER_BYTES
    } else {
        return Err("协议版本不匹配".into());
    };
    if !(16..=max).contains(&len) {
        return Err(if &magic == IMAGE_MAGIC {
            "图片大小超限".into()
        } else {
            "文本长度超限".into()
        });
    }
    Ok(FrameHeader { magic, nonce, len })
}

fn open_frame(header: &FrameHeader, ciphertext: &[u8], key: &[u8; 32]) -> Result<Vec<u8>, String> {
    let aad: &[u8] = if &header.magic == IMAGE_MAGIC {
        IMAGE_AAD
    } else {
        AAD
    };
    let cipher = ChaCha20Poly1305::new(&Key::from(*key));
    cipher
        .decrypt(
            &Nonce::from(header.nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| "配对码不匹配或数据损坏".into())
}

fn decode_plain(header: &FrameHeader, plain: Vec<u8>) -> Result<Incoming, String> {
    if &header.magic == IMAGE_MAGIC {
        if plain.len() > MAX_IMAGE_BYTES {
            return Err("图片大小超限".into());
        }
        if !plain.starts_with(PNG_MAGIC) {
            return Err("图片格式无效".into());
        }
        return Ok(Incoming::Image(plain));
    }
    let text = String::from_utf8(plain).map_err(|_| "文本编码无效")?;
    if text.trim().is_empty() {
        return Err("空文本".into());
    }
    Ok(Incoming::Text(text))
}

fn finish_frame(
    header: FrameHeader,
    reader: &mut impl Read,
    key: &[u8; 32],
) -> Result<Incoming, String> {
    let mut ciphertext = vec![0u8; header.len];
    reader
        .read_exact(&mut ciphertext)
        .map_err(|e| e.to_string())?;
    let plain = open_frame(&header, &ciphertext, key)?;
    decode_plain(&header, plain)
}

#[cfg(test)]
fn read_incoming(reader: &mut impl Read, key: &[u8; 32]) -> Result<Incoming, String> {
    let header = read_frame_header(reader)?;
    finish_frame(header, reader, key)
}

fn store_png(
    images: &crate::images::ImageStore,
    png: &[u8],
) -> Result<crate::images::Saved, String> {
    let rgba = crate::images::decode_png(png)?;
    let hash = crate::images::hash(&rgba);
    images.write(&rgba, &hash)
}

#[cfg(test)]
fn read_text_frame(reader: &mut impl Read, key: &[u8; 32]) -> Result<String, String> {
    match read_incoming(reader, key)? {
        Incoming::Text(text) => Ok(text),
        Incoming::Image(_) => Err("协议版本不匹配".into()),
    }
}

fn receive_one(
    stream: &mut TcpStream,
    key: &[u8; 32],
    app: &AppHandle,
    stop: &AtomicBool,
) -> Result<(), String> {
    receive_incoming(stream, key, Some(stop), |incoming| {
        let core = app.state::<Core>();
        match incoming {
            Incoming::Text(text) => core.save_text(&text, Some("局域网"))?,
            Incoming::Image(png) => {
                let saved = store_png(&core.images, &png)?;
                core.save_image(&saved, Some("局域网"))?;
            }
        }
        // History only. Nothing here writes the receiver's clipboard.
        changed(app);
        Ok(())
    })
}

/// Reads an image body without sitting in one 30s `read`. `stop` is checked between slices, so
/// `LanService::stop` — used when reception is turned off or the pairing code rotates — can join
/// the worker instead of waiting out the whole transfer budget.
fn read_image_body(
    stream: &mut TcpStream,
    len: usize,
    stop: Option<&AtomicBool>,
) -> Result<Vec<u8>, String> {
    stream
        .set_read_timeout(Some(IMAGE_READ_SLICE))
        .map_err(|e| e.to_string())?;
    let mut body = vec![0u8; len];
    let mut filled = 0;
    let deadline = std::time::Instant::now() + IMAGE_IO_TIMEOUT;
    while filled < len {
        if stop.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            return Err("接收已关闭".into());
        }
        if std::time::Instant::now() >= deadline {
            return Err("读取图片超时".into());
        }
        match stream.read(&mut body[filled..]) {
            Ok(0) => return Err("连接已关闭".into()),
            Ok(n) => filled += n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(body)
}

fn receive_incoming(
    stream: &mut TcpStream,
    key: &[u8; 32],
    stop: Option<&AtomicBool>,
    save: impl FnOnce(Incoming) -> Result<(), String>,
) -> Result<(), String> {
    let header = read_frame_header(stream)?;
    let incoming = if &header.magic == IMAGE_MAGIC {
        let ciphertext = read_image_body(stream, header.len, stop)?;
        let plain = open_frame(&header, &ciphertext, key)?;
        decode_plain(&header, plain)?
    } else {
        finish_frame(header, stream, key)?
    };
    save(incoming)?;
    stream
        .write_all(&make_ack(key)?)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
fn receive_stream(
    stream: &mut TcpStream,
    key: &[u8; 32],
    save_text: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    receive_incoming(stream, key, None, |incoming| match incoming {
        Incoming::Text(text) => save_text(&text),
        Incoming::Image(_) => Err("收到了图片帧".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_and_encryption_roundtrip() {
        let c = Config::fresh();
        let key = decode_key(&c.receive_key).unwrap();
        let code = format!("{CODE_PREFIX}192.168.1.2:{PORT}/{}", c.receive_key);
        assert!(parse_code(&code).is_ok());
        assert!(parse_code(&code.replace("192.168.1.2", "8.8.8.8")).is_err());
        let frame = encrypt_frame(&key, "你好\nlong text").unwrap();
        assert_eq!(
            read_text_frame(&mut frame.as_slice(), &key).unwrap(),
            "你好\nlong text"
        );
        let mut tampered = frame.clone();
        tampered[20] ^= 1;
        assert!(read_text_frame(&mut tampered.as_slice(), &key).is_err());
        let mut wrong_key = key;
        wrong_key[0] ^= 1;
        assert!(read_text_frame(&mut frame.as_slice(), &wrong_key).is_err());
        let mut oversized = frame.clone();
        oversized[16..20].copy_from_slice(&((MAX_CIPHER_BYTES + 1) as u32).to_be_bytes());
        assert!(
            read_text_frame(&mut oversized.as_slice(), &key)
                .unwrap_err()
                .contains("超限")
        );
        let ack = make_ack(&key).unwrap();
        verify_ack(&key, &ack).unwrap();
        let mut forged = ack;
        forged[29] ^= 1;
        assert!(verify_ack(&key, &forged).is_err());
    }

    #[test]
    fn pairing_persists_with_receiver_off_by_default() {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let dir =
            std::env::temp_dir().join(format!("clipkeep-lan-test-{}", u64::from_ne_bytes(random)));
        std::fs::create_dir(&dir).unwrap();
        let service = LanService::new(&dir);
        assert!(!service.state().unwrap().receive_enabled);
        let key = service.config.lock().unwrap().receive_key.clone();
        let code = format!("{CODE_PREFIX}127.0.0.1:{PORT}/{key}");
        service.add_peer("测试设备", &code).unwrap();
        drop(service);
        let reloaded = LanService::new(&dir);
        let state = reloaded.state().unwrap();
        assert!(!state.receive_enabled);
        assert_eq!(state.peers.len(), 1);
        assert_eq!(state.peers[0].name, "测试设备");
        drop(reloaded);
        std::fs::remove_file(dir.join("lan.json")).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn socket_send_requires_receiver_confirmation() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let key = decode_key(&Config::fresh().receive_key).unwrap();
        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let conn = crate::db::open(std::path::Path::new(":memory:")).unwrap();
            receive_stream(&mut stream, &key, |text| {
                let clip = crate::db::NewClip {
                    kind: crate::db::KIND_TEXT,
                    content: text,
                    hash: &crate::db::text_hash(text),
                    width: None,
                    height: None,
                    size: text.len() as i64,
                    source: Some("局域网"),
                };
                crate::db::upsert(&conn, &clip, 0).map_err(|e| e.to_string())?;
                Ok(())
            })
            .unwrap();
            let clips = crate::db::list(&conn, crate::db::Scope::All, "").unwrap();
            assert_eq!(clips.len(), 1);
            let (_, text) = crate::db::get(&conn, clips[0].id).unwrap().unwrap();
            (text, clips[0].source.clone())
        });
        let mut sender = TcpStream::connect(address).unwrap();
        send_stream(&mut sender, &key, "本机回环测试\n第二行").unwrap();
        assert_eq!(
            receiver.join().unwrap(),
            (
                "本机回环测试\n第二行".to_string(),
                Some("局域网".to_string())
            )
        );
    }

    #[test]
    fn socket_send_rejects_forged_confirmation() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let key = decode_key(&Config::fresh().receive_key).unwrap();
        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_text_frame(&mut stream, &key).unwrap();
            let mut ack = make_ack(&key).unwrap();
            ack[29] ^= 1;
            stream.write_all(&ack).unwrap();
        });
        let mut sender = TcpStream::connect(address).unwrap();
        assert!(send_stream(&mut sender, &key, "必须确认").is_err());
        receiver.join().unwrap();
    }

    #[test]
    fn socket_send_fails_when_receiver_cannot_save() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let key = decode_key(&Config::fresh().receive_key).unwrap();
        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            receive_stream(&mut stream, &key, |_| Err("存储失败".into())).unwrap_err()
        });
        let mut sender = TcpStream::connect(address).unwrap();
        assert!(send_stream(&mut sender, &key, "不能误报成功").is_err());
        assert_eq!(receiver.join().unwrap(), "存储失败");
    }

    fn sample_png() -> Vec<u8> {
        use image::ImageEncoder as _;
        let pixels = [9u8, 17, 25, 255];
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&pixels, 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        png
    }

    /// The text-only reader this crate shipped before image frames. Kept here, not called from
    /// the new parser, so a `CKL2` image is proven to fail closed on an old peer.
    fn text_only_client_read(reader: &mut impl Read, key: &[u8; 32]) -> Result<String, String> {
        use chacha20poly1305::{
            ChaCha20Poly1305, Key, KeyInit, Nonce,
            aead::{Aead, Payload},
        };
        let mut header = [0u8; 20];
        reader.read_exact(&mut header).map_err(|e| e.to_string())?;
        if &header[..4] != MAGIC {
            return Err("协议版本不匹配".into());
        }
        let len = u32::from_be_bytes(header[16..20].try_into().unwrap()) as usize;
        if !(16..=MAX_CIPHER_BYTES).contains(&len) {
            return Err("文本长度超限".into());
        }
        let mut ciphertext = vec![0u8; len];
        reader
            .read_exact(&mut ciphertext)
            .map_err(|e| e.to_string())?;
        let cipher = ChaCha20Poly1305::new(&Key::from(*key));
        let nonce: [u8; 12] = header[4..16].try_into().unwrap();
        let plain = cipher
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &ciphertext,
                    aad: AAD,
                },
            )
            .map_err(|_| "配对码不匹配或数据损坏")?;
        let text = String::from_utf8(plain).map_err(|_| "文本编码无效")?;
        if text.trim().is_empty() {
            return Err("空文本".into());
        }
        Ok(text)
    }

    fn header_only(magic: &[u8; 4], len: u32) -> [u8; 20] {
        let mut header = [0u8; 20];
        header[..4].copy_from_slice(magic);
        header[16..20].copy_from_slice(&len.to_be_bytes());
        header
    }

    #[test]
    fn image_frames_roundtrip_and_keep_text_frames() {
        let key = decode_key(&Config::fresh().receive_key).unwrap();
        let png = sample_png();
        let frame = encrypt_image_frame(&key, &png).unwrap();
        match read_incoming(&mut frame.as_slice(), &key).unwrap() {
            Incoming::Image(bytes) => assert_eq!(bytes, png),
            Incoming::Text(_) => panic!("image frame decoded as text"),
        }
        let mut tampered = frame.clone();
        tampered[20] ^= 1;
        assert!(read_incoming(&mut tampered.as_slice(), &key).is_err());
        let mut wrong_key = key;
        wrong_key[0] ^= 1;
        assert!(read_incoming(&mut frame.as_slice(), &wrong_key).is_err());
        let mut as_text = frame.clone();
        as_text[..4].copy_from_slice(MAGIC);
        assert!(read_incoming(&mut as_text.as_slice(), &key).is_err());

        let text = encrypt_frame(&key, "仍是文本").unwrap();
        match read_incoming(&mut text.as_slice(), &key).unwrap() {
            Incoming::Text(value) => assert_eq!(value, "仍是文本"),
            Incoming::Image(_) => panic!("text frame decoded as image"),
        }

        let garbage = seal(IMAGE_MAGIC, IMAGE_AAD, &key, b"not-a-png-payload").unwrap();
        assert!(
            read_incoming(&mut garbage.as_slice(), &key)
                .unwrap_err()
                .contains("图片格式无效")
        );
        let huge = header_only(IMAGE_MAGIC, u32::MAX);
        assert!(
            read_incoming(&mut &huge[..], &key)
                .unwrap_err()
                .contains("图片大小超限"),
            "an oversized image length must be refused before the body is read"
        );
        assert_eq!(validate_png_for_send(&[]).unwrap_err(), "图片是空的");
        assert!(validate_png_for_send(b"nope").unwrap_err().contains("格式"));
        assert!(reject_image_size(MAX_IMAGE_BYTES).is_ok());
        assert_eq!(
            reject_image_size(MAX_IMAGE_BYTES + 1).unwrap_err(),
            image_limit_error()
        );
    }

    #[test]
    fn text_only_client_rejects_image_frames_without_saving_them() {
        let key = decode_key(&Config::fresh().receive_key).unwrap();
        let png = sample_png();
        assert!(png.len() + 16 <= MAX_CIPHER_BYTES);
        let image = encrypt_image_frame(&key, &png).unwrap();
        let err = text_only_client_read(&mut image.as_slice(), &key).unwrap_err();
        assert!(
            err.contains("协议版本不匹配"),
            "old peers must reject the image magic, got {err}"
        );
        let text = encrypt_frame(&key, "旧端仍能收文本").unwrap();
        assert_eq!(
            text_only_client_read(&mut text.as_slice(), &key).unwrap(),
            "旧端仍能收文本"
        );
        let huge = header_only(IMAGE_MAGIC, u32::MAX);
        assert!(
            text_only_client_read(&mut &huge[..], &key)
                .unwrap_err()
                .contains("协议版本不匹配")
        );
    }

    #[test]
    fn send_image_checks_the_payload_before_connecting() {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let dir = std::env::temp_dir().join(format!(
            "clipkeep-lan-image-guard-{}",
            u64::from_ne_bytes(random)
        ));
        std::fs::create_dir(&dir).unwrap();
        let service = LanService::new(&dir);
        let address = format!("127.0.0.1:{PORT}");
        let err = service.send_image(&address, b"nope").unwrap_err();
        assert!(err.contains("图片格式无效"), "{err}");
        assert!(!err.contains("连接"), "{err}");
        drop(service);
        std::fs::remove_file(dir.join("lan.json")).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn socket_image_is_stored_as_history() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let key = decode_key(&Config::fresh().receive_key).unwrap();
        let png = sample_png();
        let payload = png.clone();
        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut random = [0u8; 8];
            getrandom::fill(&mut random).unwrap();
            let dir = std::env::temp_dir().join(format!(
                "clipkeep-lan-image-recv-{}",
                u64::from_ne_bytes(random)
            ));
            std::fs::create_dir(&dir).unwrap();
            let store = crate::images::ImageStore::new(&dir).unwrap();
            let conn = crate::db::open(std::path::Path::new(":memory:")).unwrap();
            receive_incoming(&mut stream, &key, None, |incoming| {
                let Incoming::Image(bytes) = incoming else {
                    return Err("图片被当成了文本".into());
                };
                let saved = store_png(&store, &bytes)?;
                let clip = crate::db::NewClip {
                    kind: crate::db::KIND_IMAGE,
                    content: &saved.file,
                    hash: &saved.hash,
                    width: Some(saved.width),
                    height: Some(saved.height),
                    size: saved.bytes as i64,
                    source: Some("局域网"),
                };
                crate::db::upsert(&conn, &clip, 0).map_err(|e| e.to_string())?;
                Ok(())
            })
            .unwrap();
            let clips = crate::db::list(&conn, crate::db::Scope::All, "").unwrap();
            assert_eq!(clips.len(), 1);
            let clip = &clips[0];
            assert_eq!(clip.kind, crate::db::KIND_IMAGE);
            assert_eq!(clip.width, Some(1));
            assert_eq!(clip.height, Some(1));
            assert_eq!(clip.source.as_deref(), Some("局域网"));
            assert!(clip.preview.is_empty());
            let file = clip.file.clone().unwrap();
            let loaded = store.load(&file).unwrap();
            let expected = crate::images::decode_png(&png).unwrap();
            assert_eq!(loaded.pixels, expected.pixels);
            let thumb = store.thumb_path(&file).unwrap();
            assert!(thumb.is_file());
            assert_eq!(
                crate::images::decode_png(&std::fs::read(&thumb).unwrap())
                    .unwrap()
                    .pixels,
                expected.pixels
            );
            drop(store);
            drop(conn);
            std::fs::remove_dir_all(&dir).unwrap();
        });
        let mut sender = TcpStream::connect(address).unwrap();
        send_image_stream(&mut sender, &key, &payload).unwrap();
        receiver.join().unwrap();
    }

    #[test]
    fn socket_image_is_rejected_by_text_only_peer() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let key = decode_key(&Config::fresh().receive_key).unwrap();
        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let err = text_only_client_read(&mut stream, &key).unwrap_err();
            assert!(
                err.contains("协议版本不匹配"),
                "old peer should reject the frame, got {err}"
            );
        });
        let mut sender = TcpStream::connect(address).unwrap();
        let err = send_image_stream(&mut sender, &key, &sample_png()).unwrap_err();
        assert!(
            err.contains("旧版本") || err.contains("未确认") || err.contains("发送图片失败"),
            "{err}"
        );
        receiver.join().unwrap();
    }

    #[test]
    fn stopping_reception_interrupts_a_stalled_image_read() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let key = decode_key(&Config::fresh().receive_key).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let receiver = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let started = std::time::Instant::now();
            let err = receive_incoming(&mut stream, &key, Some(&thread_stop), |_| {
                Err("停掉的传输不应写入历史".into())
            })
            .unwrap_err();
            (err, started.elapsed())
        });
        let mut sender = TcpStream::connect(address).unwrap();
        sender
            .write_all(&header_only(IMAGE_MAGIC, 64 * 1024))
            .unwrap();
        thread::sleep(Duration::from_millis(50));
        stop.store(true, Ordering::SeqCst);
        let (err, elapsed) = receiver.join().unwrap();
        assert!(
            elapsed < Duration::from_secs(2),
            "stop waited {elapsed:?}: {err}"
        );
        assert!(err.contains("接收已关闭"), "{err}");
    }
}
