//! Update check, download and install.
//!
//! The check asks GitHub and jsDelivr at the same time (GitHub is often slow or unreachable from
//! mainland China) and takes the higher version. Downloads come from GitHub Releases and are
//! verified against the SHA-256 digest GitHub publishes for the asset; nothing is installed
//! without that match, and nothing is downloaded or installed without the user asking.

use serde::Serialize;
use sha2::{Digest, Sha256};
#[cfg(windows)]
use std::fs::OpenOptions;
use std::fs::{self, File};
#[cfg(windows)]
use std::io::Read;
use std::io::Write;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
#[cfg(windows)]
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
#[cfg(windows)]
fn is_portable() -> bool {
    std::env::current_exe()
        .map(|exe| !exe.with_file_name("uninstall.exe").exists())
        .unwrap_or(true)
}

/// The macOS build is a plain .app: there is no elevated installer to run in place, so the
/// release page is the only update path. `download()` bails out just as it does for a portable
/// Windows copy, and the UI keeps offering the "open download page" button.
#[cfg(target_os = "macos")]
fn is_portable() -> bool {
    true
}

/// Release asset suffix for the running platform.
#[cfg(windows)]
const ASSET_SUFFIX: &str = "_x64-setup.exe";
#[cfg(target_os = "macos")]
const ASSET_SUFFIX: &str = "_aarch64.dmg";

#[cfg(windows)]
fn installer_file_name(version: &str) -> String {
    format!("clipkeep_{version}_x64-setup.exe")
}

#[cfg(target_os = "macos")]
fn installer_file_name(version: &str) -> String {
    format!("clipkeep_{version}_aarch64.dmg")
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

/// Uses the OS proxy settings (system-proxy) and the OS certificate store (Schannel on Windows,
/// Security.framework on macOS).
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
                .is_some_and(|n| n.ends_with(ASSET_SUFFIX))
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
    let path = dir.join(installer_file_name(&release.version));
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
#[cfg(windows)]
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

/// macOS has no in-place installer; `is_portable()` keeps `download()` from reaching Ready, so
/// this is only a guard against a stale UI event.
#[cfg(target_os = "macos")]
pub fn install() -> Result<(), String> {
    Err("请从发布页下载 .dmg 手动更新".into())
}

/// The app icon at the size the platform draws in the notification area (16 px at 100 %
/// scaling on Windows). The window icon is the 32 px one, which the tray shrinks; a badge
/// drawn on that comes out a few blurry pixels wide.
pub fn tray_icon(app: &AppHandle) -> Option<Image<'static>> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) };
        if size > 0
            && let Ok(icon) = Image::from_app_icon_resource(size as u32)
        {
            return Some(icon);
        }
    }
    app.default_window_icon().map(|icon| {
        #[cfg(target_os = "macos")]
        {
            // Keep the logo's alpha mask (including antialiasing), with white artwork for
            // the menu bar. Also used when refreshing the update badge, so it stays white.
            let mut rgba = icon.rgba().to_vec();
            for pixel in rgba.as_chunks_mut::<4>().0 {
                pixel[..3].fill(255);
            }
            Image::new_owned(rgba, icon.width(), icon.height())
        }
        #[cfg(not(target_os = "macos"))]
        icon.clone().to_owned()
    })
}

/// Paints a red dot on the tray icon while a newer version is known.
fn refresh_tray(app: &AppHandle, newer: Option<&str>) {
    let (Some(tray), Some(base)) = (app.tray_by_id("main"), tray_icon(app)) else {
        return;
    };
    let (icon, tooltip) = match newer {
        Some(version) => (
            badged(&base),
            format!("{TRAY_TOOLTIP} · 有新版本 v{version}"),
        ),
        None => (base, TRAY_TOOLTIP.to_owned()),
    };
    let _ = tray.set_icon(Some(icon));
    let _ = tray.set_tooltip(Some(tooltip));
}

const BADGE_RED: [f32; 3] = [235.0, 59.0, 64.0];

/// Share of the pixel at (x, y) inside the circle, from 4×4 samples, for smooth edges.
fn coverage(x: u32, y: u32, cx: f32, cy: f32, r: f32) -> f32 {
    const N: u32 = 4;
    let mut inside = 0;
    for sy in 0..N {
        for sx in 0..N {
            let px = x as f32 + (sx as f32 + 0.5) / N as f32;
            let py = y as f32 + (sy as f32 + 0.5) / N as f32;
            if (px - cx).powi(2) + (py - cy).powi(2) <= r * r {
                inside += 1;
            }
        }
    }
    inside as f32 / (N * N) as f32
}

/// A notification dot in the top-right corner, where the logo is empty. A transparent gap
/// separates it from the logo, which reads on light and dark taskbars alike.
fn badged(base: &Image<'_>) -> Image<'static> {
    let (w, h) = (base.width(), base.height());
    let mut px = base.rgba().to_vec();
    let size = w.min(h) as f32;
    let r = (size * 0.19).max(3.0);
    let gap = (size * 0.07).max(1.0);
    let (cx, cy) = (w as f32 - r, r);
    for y in 0..h {
        for x in 0..w {
            let around = coverage(x, y, cx, cy, r + gap);
            if around == 0.0 {
                continue;
            }
            let dot = coverage(x, y, cx, cy, r);
            let i = ((y * w + x) * 4) as usize;
            // The logo, cut back by the gap, with the dot composited over it.
            let logo = px[i + 3] as f32 / 255.0 * (1.0 - around);
            let alpha = dot + logo * (1.0 - dot);
            for c in 0..3 {
                let value = if alpha > 0.0 {
                    (BADGE_RED[c] * dot + px[i + c] as f32 * logo * (1.0 - dot)) / alpha
                } else {
                    0.0
                };
                px[i + c] = value.round().clamp(0.0, 255.0) as u8;
            }
            px[i + 3] = (alpha * 255.0).round() as u8;
        }
    }
    Image::new_owned(px, w, h)
}

#[cfg(test)]
mod tests {
    use super::{Image, State, badged, newer};

    #[test]
    fn badge_sits_in_the_corner_with_a_clear_gap() {
        // An opaque white 16 px icon, the size of the tray at 100 % scaling.
        let base = Image::new_owned(vec![255; 16 * 16 * 4], 16, 16);
        let out = badged(&base);
        let px = |x: u32, y: u32| {
            let i = ((y * 16 + x) * 4) as usize;
            out.rgba()[i..i + 4].to_vec()
        };
        // The dot's centre is solid red.
        assert_eq!(px(12, 3), vec![235, 59, 64, 255]);
        // Just outside it, the logo is cut away so the dot stands apart.
        assert_eq!(px(9, 3)[3], 0);
        // The rest of the icon is untouched.
        assert_eq!(px(2, 13), vec![255, 255, 255, 255]);
        assert_eq!(px(8, 8), vec![255, 255, 255, 255]);
    }

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
