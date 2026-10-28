//! Update check, download and install.
//!
//! The check asks GitHub and jsDelivr at the same time (GitHub is often slow or unreachable from
//! mainland China) and takes the higher version. Downloads come from GitHub Releases and are
//! verified against the SHA-256 digest GitHub publishes for the asset; nothing is installed
//! without that match, and nothing is downloaded or installed without the user asking.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::image::Image;
use tauri::{AppHandle, Emitter};

const REPO: &str = "Fomovet/clipkeep";
pub const RELEASES_PAGE: &str = "https://github.com/Fomovet/clipkeep/releases/latest";
pub const EVENT: &str = "update-state";
const TRAY_TOOLTIP: &str = "clipkeep 剪贴板";
const CHECK_TIMEOUT: Duration = Duration::from_secs(8);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);
/// Delay before the start-up check, so it never competes with launching or a slow logon network.
pub const STARTUP_DELAY: Duration = Duration::from_secs(15);

/// Shared by every window through the `update-state` event.
#[derive(Clone, Serialize, PartialEq, Debug)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum State {
    Idle,
    Checking,
    UpToDate,
    /// `portable`: this copy wasn't installed by the installer, so it can only be replaced by hand.
    Available {
        version: String,
        size: u64,
        portable: bool,
    },
    Downloading {
        version: String,
        received: u64,
        total: u64,
    },
    Ready {
        version: String,
    },
    /// `version` is set when a newer release is known, so the download can be retried.
    Error {
        message: String,
        version: Option<String>,
    },
}

impl State {
    fn newer_version(&self) -> Option<&str> {
        match self {
            State::Available { version, .. }
            | State::Downloading { version, .. }
            | State::Ready { version }
            | State::Error {
                version: Some(version),
                ..
            } => Some(version),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct Release {
    version: String,
    url: Option<String>,
    size: u64,
    sha256: Option<String>,
}

struct Inner {
    state: State,
    release: Option<Release>,
    file: Option<PathBuf>,
}

static INNER: Mutex<Inner> = Mutex::new(Inner {
    state: State::Idle,
    release: None,
    file: None,
});
static CANCEL: AtomicBool = AtomicBool::new(false);

pub fn state() -> State {
    INNER.lock().map(|g| g.state.clone()).unwrap_or(State::Idle)
}

fn set_state(app: &AppHandle, state: State) {
    let dot_changed = match INNER.lock() {
        Ok(mut g) => {
            let changed = g.state.newer_version() != state.newer_version();
            g.state = state.clone();
            changed
        }
        Err(_) => return,
    };
    if dot_changed {
        refresh_tray(app, state.newer_version());
    }
    let _ = app.emit(EVENT, state);
}

/// The installer leaves an uninstaller next to the exe; a single-file copy has none.
fn is_portable() -> bool {
    std::env::current_exe()
        .map(|exe| !exe.with_file_name("uninstall.exe").exists())
        .unwrap_or(true)
}

/// Compares dotted numeric versions ("0.10.0" > "0.9.3"); non-numeric parts count as 0.
pub fn newer(remote: &str, local: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split(['.', '-'])
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    let (a, b) = (parts(remote), parts(local));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    false
}

/// Uses the Windows proxy settings (system-proxy) and the Windows certificate store (Schannel).
fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("clipkeep/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())
}

async fn get_json(url: &str) -> Result<serde_json::Value, String> {
    let resp = client()?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .timeout(CHECK_TIMEOUT)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

async fn github_latest() -> Result<Release, String> {
    let v = get_json(&format!(
        "https://api.github.com/repos/{REPO}/releases/latest"
    ))
    .await?;
    let version = v["tag_name"]
        .as_str()
        .ok_or("no tag_name")?
        .trim_start_matches('v')
        .to_owned();
    let setup = v["assets"].as_array().and_then(|assets| {
        assets.iter().find(|a| {
            a["name"]
                .as_str()
                .is_some_and(|n| n.ends_with("_x64-setup.exe"))
        })
    });
    Ok(Release {
        version,
        url: setup
            .and_then(|a| a["browser_download_url"].as_str())
            .map(str::to_owned),
        size: setup.and_then(|a| a["size"].as_u64()).unwrap_or(0),
        sha256: setup
            .and_then(|a| a["digest"].as_str())
            .and_then(|d| d.strip_prefix("sha256:"))
            .map(str::to_ascii_lowercase),
    })
}

/// jsDelivr resolves the newest tag of the repository and is usually reachable from China.
async fn jsdelivr_latest() -> Result<String, String> {
    let v = get_json(&format!(
        "https://data.jsdelivr.com/v1/packages/gh/{REPO}/resolved?specifier=latest"
    ))
    .await?;
    v["version"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "no version".into())
}

/// `quiet` is the start-up check: it only ever announces a new version, never "up to date" or
/// a network error, and it doesn't interrupt a check or download already under way.
pub async fn check(app: &AppHandle, quiet: bool) {
    let busy = matches!(
        state(),
        State::Checking | State::Downloading { .. } | State::Ready { .. }
    );
    if busy {
        return;
    }
    if !quiet {
        set_state(app, State::Checking);
    }
    let local = app.package_info().version.to_string();
    let github = tauri::async_runtime::spawn(github_latest());
    let jsdelivr = tauri::async_runtime::spawn(jsdelivr_latest());
    let github = github.await.ok().and_then(Result::ok);
    let jsdelivr = jsdelivr.await.ok().and_then(Result::ok);

    let remote = match (github.as_ref().map(|r| r.version.clone()), jsdelivr) {
        (Some(a), Some(b)) => Some(if newer(&b, &a) { b } else { a }),
        (a, b) => a.or(b),
    };
    let next = match remote {
        Some(version) if newer(&version, &local) => {
            let release = github.filter(|r| r.version == version);
            let size = release.as_ref().map_or(0, |r| r.size);
            if let Ok(mut g) = INNER.lock() {
                g.release = release;
            }
            State::Available {
                version,
                size,
                portable: is_portable(),
            }
        }
        Some(_) if quiet => return,
        Some(_) => State::UpToDate,
        None if quiet => return,
        None => State::Error {
            message: "无法连接更新服务器，请检查网络后重试".into(),
            version: None,
        },
    };
    set_state(app, next);
}

pub fn cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}

/// Downloads the installer for the announced version and verifies it.
pub async fn download(app: AppHandle) {
    let (version, known) = match INNER.lock() {
        Ok(g) => match g.state.newer_version() {
            Some(v) if !matches!(g.state, State::Downloading { .. } | State::Ready { .. }) => {
                (v.to_owned(), g.release.clone())
            }
            _ => return,
        },
        Err(_) => return,
    };
    if is_portable() {
        return;
    }
    let usable = |r: &Release| r.version == version && r.url.is_some() && r.sha256.is_some();
    set_state(
        &app,
        State::Downloading {
            version: version.clone(),
            received: 0,
            total: known.as_ref().map_or(0, |r| r.size),
        },
    );
    CANCEL.store(false, Ordering::Relaxed);

    // The check may have been answered by jsDelivr alone; the download needs GitHub's digest.
    let release = match known.filter(usable) {
        Some(r) => r,
        None => match github_latest().await {
            Ok(r) if usable(&r) => r,
            _ => {
                return fail(
                    &app,
                    &version,
                    "无法从 GitHub 获取安装包信息，请稍后重试，或在浏览器中下载",
                );
            }
        },
    };
    match fetch(&app, &release).await {
        Ok(path) => {
            if let Ok(mut g) = INNER.lock() {
                g.file = Some(path);
                // install() re-verifies against this digest.
                g.release = Some(release.clone());
            }
            set_state(&app, State::Ready { version });
        }
        Err(Fetch::Cancelled) => {
            let size = release.size;
            set_state(
                &app,
                State::Available {
                    version,
                    size,
                    portable: false,
                },
            );
        }
        Err(Fetch::Failed(message)) => fail(&app, &version, &message),
    }
}

fn fail(app: &AppHandle, version: &str, message: &str) {
    set_state(
        app,
        State::Error {
            message: message.to_owned(),
            version: Some(version.to_owned()),
        },
    );
}

enum Fetch {
    Cancelled,
    Failed(String),
}

impl<E: std::fmt::Display> From<E> for Fetch {
    fn from(e: E) -> Self {
        Fetch::Failed(format!("下载失败：{e}"))
    }
}

fn download_dir() -> PathBuf {
    std::env::temp_dir().join("clipkeep-update")
}

async fn fetch(app: &AppHandle, release: &Release) -> Result<PathBuf, Fetch> {
    let (Some(url), Some(expected)) = (&release.url, &release.sha256) else {
        return Err(Fetch::Failed("缺少安装包地址或校验值".into()));
    };
    let dir = download_dir();
    // Old installers are never needed again.
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("clipkeep_{}_x64-setup.exe", release.version));
    let partial = path.with_extension("part");

    let mut resp = client()?.get(url).send().await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(release.size);
    let mut file = File::create(&partial)?;
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    let mut last = Instant::now();
    let result = async {
        while let Some(chunk) = resp.chunk().await? {
            if CANCEL.load(Ordering::Relaxed) {
                return Err(Fetch::Cancelled);
            }
            file.write_all(&chunk)?;
            hasher.update(&chunk);
            received += chunk.len() as u64;
            if last.elapsed() >= PROGRESS_INTERVAL {
                last = Instant::now();
                set_state(
                    app,
                    State::Downloading {
                        version: release.version.clone(),
                        received,
                        total,
                    },
                );
            }
        }
        file.sync_all()?;
        Ok(())
    }
    .await;
    drop(file);
    if let Err(e) = result {
        let _ = fs::remove_file(&partial);
        return Err(e);
    }
    if format!("{:x}", hasher.finalize()) != *expected {
        let _ = fs::remove_file(&partial);
        return Err(Fetch::Failed(
            "安装包校验失败（SHA-256 不一致），已删除，请重试".into(),
        ));
    }
    fs::rename(&partial, &path)?;
    Ok(path)
}

/// Starts the verified installer in passive mode; it closes clipkeep, upgrades in place and
/// restarts it. The caller then exits.
pub fn install() -> Result<(), String> {
    let (version, path, expected) = match INNER.lock() {
        Ok(g) => match (
            &g.state,
            &g.file,
            g.release.as_ref().and_then(|r| r.sha256.as_ref()),
        ) {
            (State::Ready { version }, Some(path), Some(sha256)) => {
                (version.clone(), path.clone(), sha256.clone())
            }
            _ => return Err("安装包尚未下载完成".into()),
        },
        Err(_) => return Err("更新状态不可用".into()),
    };
    // %TEMP% is writable by any process of this user, and we run elevated: hold the file open
    // without write/delete sharing while re-verifying and starting it, so it can't be swapped.
    const FILE_SHARE_READ: u32 = 1;
    let mut locked = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&path)
        .map_err(|e| format!("无法打开安装包：{e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = locked.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    if format!("{:x}", hasher.finalize()) != expected {
        return Err(format!("v{version} 安装包校验失败，请重新下载"));
    }
    // /P: progress bar only, closes the running app itself. /R: start clipkeep when done.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    Command::new(&path)
        .args(["/P", "/R"])
        .creation_flags(DETACHED_PROCESS)
        .spawn()
        .map_err(|e| format!("无法启动安装程序：{e}"))?;
    drop(locked);
    Ok(())
}

/// Paints a red dot on the tray icon while a newer version is known.
fn refresh_tray(app: &AppHandle, newer: Option<&str>) {
    let (Some(tray), Some(base)) = (app.tray_by_id("main"), app.default_window_icon()) else {
        return;
    };
    let (icon, tooltip) = match newer {
        Some(version) => (
            badged(base),
            format!("{TRAY_TOOLTIP} · 有新版本 v{version}"),
        ),
        None => (base.clone(), TRAY_TOOLTIP.to_owned()),
    };
    let _ = tray.set_icon(Some(icon));
    let _ = tray.set_tooltip(Some(tooltip));
}

fn badged(base: &Image<'_>) -> Image<'static> {
    let (w, h) = (base.width(), base.height());
    let mut px = base.rgba().to_vec();
    let size = w.min(h) as f32;
    // A notification-style dot in the top-right corner, small enough not to hide the logo.
    let r = size * 0.12;
    let ring = size * 0.035;
    let (cx, cy) = (w as f32 - r - ring, r + ring);
    for y in 0..h {
        for x in 0..w {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            // Antialiased white ring around a red dot, so it reads on dark and light taskbars.
            let (color, cover) = if d <= r + 0.5 {
                ([229u8, 72, 77], (r + 0.5 - d).clamp(0.0, 1.0))
            } else {
                ([255u8, 255, 255], (r + ring + 0.5 - d).clamp(0.0, 1.0))
            };
            if cover <= 0.0 {
                continue;
            }
            let i = ((y * w + x) * 4) as usize;
            for c in 0..3 {
                px[i + c] = (px[i + c] as f32 * (1.0 - cover) + color[c] as f32 * cover) as u8;
            }
            px[i + 3] = px[i + 3].max((cover * 255.0) as u8);
        }
    }
    Image::new_owned(px, w, h)
}

#[cfg(test)]
mod tests {
    use super::{State, newer};

    #[test]
    fn compares_versions_numerically() {
        assert!(newer("0.2.1", "0.2.0"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0", "0.99.99"));
        assert!(!newer("0.2.0", "0.2.0"));
        assert!(!newer("0.1.9", "0.2.0"));
        assert!(!newer("0.2", "0.2.0"));
    }

    #[test]
    fn state_serializes_for_the_frontend() {
        let json = serde_json::to_value(State::Downloading {
            version: "0.3.0".into(),
            received: 1,
            total: 2,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "downloading", "version": "0.3.0", "received": 1, "total": 2 })
        );
        assert_eq!(
            serde_json::to_value(State::UpToDate).unwrap()["kind"],
            "upToDate"
        );
    }
}
