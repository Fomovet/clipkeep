mod atomic_file;
mod clipboard;
mod db;
mod dib;
mod images;
mod instances;
mod lan;
mod settings;
mod update;
mod win;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use clipboard::Captured;
use db::{Group, NewClip, RawClip, Scope};
use images::{ImageStore, Rgba};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use settings::{Settings, SettingsFile};
use std::collections::HashMap;
use std::io::{self, BufWriter, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, RwLock};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, State, WebviewWindow, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_dialog::DialogExt;
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const AUTOSTART_ARG: &str = "--minimized";
const AUTOSTART_TASK: &str = "\\clipkeep-clipboard";
/// Written next to the exe by the installer on a first install (see installer-hooks.nsh).
const AUTOSTART_MARKER: &str = "autostart.pending";
const BUNDLE_VERSION: u32 = 1;
const MAX_IMPORT_FILE: u64 = 2 * 1024 * 1024 * 1024;
/// Window won't auto-hide on blur until this epoch-ms (briefly after show or while dragging).
static SUPPRESS_HIDE_UNTIL: AtomicI64 = AtomicI64::new(0);
const SHOW_BLUR_GRACE_MS: i64 = 80;
/// Only an actual title-bar drag may extend the blur grace on Moved events.
/// Showing the window also moves it near the cursor and emits Moved.
static DRAG_HIDE_UNTIL: AtomicI64 = AtomicI64::new(0);
/// Invalidates a pending blur check whenever the window is shown, hidden, or focused again.
static FOCUS_EPOCH: AtomicU64 = AtomicU64::new(0);
/// Our own record of whether the window is shown. `WebviewWindow::is_visible()` proved unreliable
/// here (stale `true` after a hide), which made the toggle hotkey need two presses; every show/hide
/// goes through show_main/hide_main, so this flag is the authoritative toggle state.
static WINDOW_VISIBLE: AtomicBool = AtomicBool::new(false);
/// Ignore a transient blur while WebView2 is being shown, then check real focus.
static TRAY_MENU_VISIBLE: AtomicBool = AtomicBool::new(false);
static TRAY_MENU_EPOCH: AtomicU64 = AtomicU64::new(0);
static TRAY_MENU_BLUR_GRACE_UNTIL: AtomicI64 = AtomicI64::new(0);
/// The tray menu window is being built, so a second right-click doesn't build another.
static TRAY_MENU_CREATING: AtomicBool = AtomicBool::new(false);
/// The tray menu page has loaded its state and can be shown.
static TRAY_MENU_READY: AtomicBool = AtomicBool::new(false);
/// Where the menu was requested; it may only be shown once its page has loaded.
static TRAY_MENU_POINTER: Mutex<Option<PhysicalPosition<f64>>> = Mutex::new(None);

struct Core {
    db: Mutex<Connection>,
    images: ImageStore,
    lan: lan::LanService,
    settings: RwLock<Settings>,
    settings_file: SettingsFile,
    position_dirty: AtomicBool,
    data_dir: PathBuf,
}

type CmdResult<T> = Result<T, String>;

fn to_err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Core {
    fn conn(&self) -> CmdResult<MutexGuard<'_, Connection>> {
        self.db.lock().map_err(to_err)
    }

    fn settings(&self) -> Settings {
        self.settings.read().map(|s| s.clone()).unwrap_or_default()
    }

    fn max_bytes(&self) -> i64 {
        self.settings().max_storage_bytes()
    }

    /// Inserts under the lock and deletes any trimmed image files while still holding it, so a
    /// concurrent save of the same image can't re-add a row pointing at a file we then remove.
    fn upsert(&self, clip: NewClip) -> CmdResult<()> {
        let conn = self.conn()?;
        let trimmed = db::upsert(&conn, &clip, self.max_bytes()).map_err(to_err)?;
        for file in &trimmed {
            if !db::is_file_referenced(&conn, file).unwrap_or(true) {
                self.images.remove(file);
            }
        }
        Ok(())
    }

    fn save_text(&self, text: &str, source: Option<&str>) -> CmdResult<()> {
        let hash = db::text_hash(text);
        self.upsert(NewClip {
            kind: db::KIND_TEXT,
            content: text,
            hash: &hash,
            width: None,
            height: None,
            size: text.len() as i64,
            source,
        })
    }

    fn save_image(&self, saved: &images::Saved, source: Option<&str>) -> CmdResult<()> {
        self.upsert(NewClip {
            kind: db::KIND_IMAGE,
            content: &saved.file,
            hash: &saved.hash,
            width: Some(saved.width),
            height: Some(saved.height),
            size: saved.bytes as i64,
            source,
        })
    }

    fn edit_text(&self, id: i64, text: &str) -> CmdResult<()> {
        let conn = self.conn()?;
        let trimmed = db::edit_text(&conn, id, text, self.max_bytes())?;
        for file in &trimmed {
            if !db::is_file_referenced(&conn, file).unwrap_or(true) {
                self.images.remove(file);
            }
        }
        Ok(())
    }

    /// Removes image files for rows just deleted, but only those no surviving row still points at.
    fn drop_files(&self, files: &[String]) {
        if files.is_empty() {
            return;
        }
        let Ok(conn) = self.conn() else { return };
        for file in files {
            if !db::is_file_referenced(&conn, file).unwrap_or(true) {
                self.images.remove(file);
            }
        }
    }

    fn persist_position(&self) {
        if !self.position_dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let result = self
            .settings
            .read()
            .map_err(to_err)
            .and_then(|settings| self.settings_file.save(&settings).map_err(to_err));
        if let Err(error) = result {
            self.position_dirty.store(true, Ordering::Relaxed);
            eprintln!("saving window position failed: {error}");
        }
    }
}

fn changed(app: &AppHandle) {
    let _ = app.emit("clips-changed", ());
}

fn on_local_copy(app: &AppHandle, captured: Captured) {
    if clipboard::capture_paused() {
        return;
    }
    let core = app.state::<Core>();
    let result = match captured {
        Captured::Text(text) => core.save_text(&text, None),
        Captured::Image(img) => {
            let hash = images::hash(&img);
            core.images
                .write(&img, &hash)
                .and_then(|saved| core.save_image(&saved, None))
        }
        Captured::ImageFiles(paths) => {
            // A file selection can contain several large JPEGs. Keep decoding and PNG
            // thumbnail generation off the clipboard listener so later copies aren't missed.
            let app = app.clone();
            std::thread::spawn(move || {
                let core = app.state::<Core>();
                let mut saved_any = false;
                for path in paths {
                    if clipboard::capture_paused() {
                        break;
                    }
                    let result = images::decode_file(&path).and_then(|img| {
                        let hash = images::hash(&img);
                        let source = path.file_name().map(|name| name.to_string_lossy());
                        core.images
                            .write(&img, &hash)
                            .and_then(|saved| core.save_image(&saved, source.as_deref()))
                    });
                    match result {
                        Ok(()) => saved_any = true,
                        Err(e) => {
                            eprintln!("save copied image file {} failed: {e}", path.display())
                        }
                    }
                }
                if saved_any {
                    changed(&app);
                }
            });
            return;
        }
    };
    match result {
        Ok(()) => changed(app),
        Err(e) => eprintln!("save clip failed: {e}"),
    }
}

fn scope_of(scope: &str, group_id: Option<i64>) -> Scope {
    match scope {
        "fav" => Scope::Favorites,
        "group" => Scope::Group(group_id.unwrap_or(-1)),
        _ => Scope::All,
    }
}

#[tauri::command]
fn list_clips(
    core: State<Core>,
    scope: String,
    group_id: Option<i64>,
    query: String,
) -> CmdResult<Vec<db::Clip>> {
    let mut clips =
        db::list(&*core.conn()?, scope_of(&scope, group_id), query.trim()).map_err(to_err)?;
    for clip in &mut clips {
        if let Some(file) = &clip.file {
            clip.thumb = core
                .images
                .thumb_path(file)
                .map(|path| path.to_string_lossy().into_owned());
        }
    }
    Ok(clips)
}

#[tauri::command]
fn get_clip_text(core: State<Core>, id: i64) -> CmdResult<String> {
    let (kind, text) = db::get(&*core.conn()?, id)
        .map_err(to_err)?
        .ok_or("条目不存在")?;
    if kind != db::KIND_TEXT {
        return Err("只能预览文本条目".into());
    }
    Ok(text)
}

#[tauri::command]
fn update_clip_text(app: AppHandle, core: State<Core>, id: i64, text: String) -> CmdResult<()> {
    if text.trim().is_empty() {
        return Err("文本不能为空".into());
    }
    if text.len() > clipboard::MAX_TEXT_BYTES {
        return Err("文本不能超过 2 MB".into());
    }
    core.edit_text(id, &text)?;
    changed(&app);
    Ok(())
}

#[tauri::command]
fn get_capture_paused() -> bool {
    clipboard::capture_paused()
}

fn set_capture_paused(app: &AppHandle, paused: bool) {
    clipboard::set_capture_paused(paused);
    let _ = app.emit("capture-paused-changed", paused);
}

#[tauri::command]
fn toggle_capture_paused(app: AppHandle) -> bool {
    let paused = !clipboard::capture_paused();
    set_capture_paused(&app, paused);
    paused
}

#[tauri::command]
fn get_lan_state(core: State<Core>) -> CmdResult<lan::LanState> {
    core.lan.state()
}

#[tauri::command]
fn set_lan_receive(app: AppHandle, core: State<Core>, enabled: bool) -> CmdResult<lan::LanState> {
    core.lan.set_receive(&app, enabled)
}

#[tauri::command]
fn add_lan_peer(core: State<Core>, name: String, code: String) -> CmdResult<lan::LanState> {
    core.lan.add_peer(&name, &code)
}

#[tauri::command]
fn remove_lan_peer(core: State<Core>, address: String) -> CmdResult<lan::LanState> {
    core.lan.remove_peer(&address)
}

#[tauri::command]
fn rotate_lan_code(app: AppHandle, core: State<Core>) -> CmdResult<lan::LanState> {
    core.lan.rotate_code(&app)
}

#[tauri::command]
fn copy_lan_code(core: State<Core>, address: String) -> CmdResult<()> {
    let code = core
        .lan
        .state()?
        .local_codes
        .into_iter()
        .find(|c| c.address == address)
        .ok_or("本机地址不存在")?
        .code;
    clipboard::write_text(&code).map_err(to_err)
}

#[tauri::command]
async fn send_clip_to_peer(app: AppHandle, id: i64, address: String) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Core>();
        let (kind, text) = db::get(&*core.conn()?, id)
            .map_err(to_err)?
            .ok_or("条目不存在")?;
        if kind != db::KIND_TEXT {
            return Err("仅支持传送文本".into());
        }
        core.lan.send_text(&address, &text)
    })
    .await
    .map_err(to_err)?
}

#[tauri::command]
fn toggle_favorite(app: AppHandle, core: State<Core>, id: i64) -> CmdResult<bool> {
    let on = db::toggle_favorite(&*core.conn()?, id).map_err(to_err)?;
    changed(&app);
    Ok(on)
}

#[tauri::command]
fn toggle_pinned(app: AppHandle, core: State<Core>, id: i64) -> CmdResult<bool> {
    let on = db::toggle_pinned(&*core.conn()?, id).map_err(to_err)?;
    changed(&app);
    Ok(on)
}

#[tauri::command]
fn move_clip(app: AppHandle, core: State<Core>, id: i64, group_id: Option<i64>) -> CmdResult<()> {
    db::set_group(&*core.conn()?, id, group_id).map_err(to_err)?;
    changed(&app);
    Ok(())
}

#[tauri::command]
fn delete_clip(app: AppHandle, core: State<Core>, id: i64) -> CmdResult<()> {
    let files = db::delete(&*core.conn()?, id).map_err(to_err)?;
    core.drop_files(&files);
    changed(&app);
    Ok(())
}

#[tauri::command]
fn clear_data(
    app: AppHandle,
    core: State<Core>,
    include_pinned: bool,
    include_grouped: bool,
    include_all: bool,
) -> CmdResult<()> {
    let files = db::clear_data(&*core.conn()?, include_pinned, include_grouped, include_all)
        .map_err(to_err)?;
    core.drop_files(&files);
    changed(&app);
    Ok(())
}

#[tauri::command]
fn list_groups(core: State<Core>) -> CmdResult<Vec<Group>> {
    db::list_groups(&*core.conn()?).map_err(to_err)
}

#[tauri::command]
fn create_group(app: AppHandle, core: State<Core>, name: String) -> CmdResult<Group> {
    let name = name.trim();
    if name.is_empty() {
        return Err("分组名不能为空".into());
    }
    let group = db::create_group(&*core.conn()?, name).map_err(to_err)?;
    let _ = app.emit("groups-changed", ());
    Ok(group)
}

#[tauri::command]
fn rename_group(app: AppHandle, core: State<Core>, id: i64, name: String) -> CmdResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err("分组名不能为空".into());
    }
    db::rename_group(&*core.conn()?, id, name).map_err(to_err)?;
    let _ = app.emit("groups-changed", ());
    Ok(())
}

#[tauri::command]
fn delete_group(app: AppHandle, core: State<Core>, id: i64, delete_items: bool) -> CmdResult<()> {
    let files = db::delete_group(&*core.conn()?, id, delete_items).map_err(to_err)?;
    core.drop_files(&files);
    let _ = app.emit("groups-changed", ());
    changed(&app);
    Ok(())
}

#[tauri::command]
fn reorder_groups(app: AppHandle, core: State<Core>, ids: Vec<i64>) -> CmdResult<()> {
    db::reorder_groups(&*core.conn()?, &ids).map_err(to_err)?;
    let _ = app.emit("groups-changed", ());
    Ok(())
}

/// Reads the stored item and puts it on the clipboard. Image decoding happens before the lock is
/// taken so a large image never blocks other DB users.
fn put_on_clipboard(core: &Core, id: i64) -> CmdResult<()> {
    let (kind, content) = db::get(&*core.conn()?, id)
        .map_err(to_err)?
        .ok_or("条目不存在")?;
    if kind == db::KIND_IMAGE {
        let img = core.images.load(&content)?;
        let file = if core.settings().paste_image_as_file {
            let source = db::source(&*core.conn()?, id).ok().flatten();
            core.images
                .png_bytes(&content)
                .ok()
                .map(|png| (png, paste_file_name(source.as_deref())))
        } else {
            None
        };
        clipboard::write_image(
            &img,
            file.as_ref()
                .map(|(png, name)| (png.as_slice(), name.as_str())),
        )
        .map_err(to_err)?;
    } else {
        clipboard::write_text(&content).map_err(to_err)?;
    }
    db::touch(&*core.conn()?, id).map_err(to_err)?;
    Ok(())
}

/// File name offered for a pasted picture: the original name of a copied image file, otherwise
/// the time of the paste, so consecutive pastes into an upload form don't clash.
fn paste_file_name(source: Option<&str>) -> String {
    const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    let stem = source
        .and_then(|s| Path::new(s).file_stem())
        .map(|stem| {
            stem.to_string_lossy()
                .chars()
                .filter(|c| !c.is_control() && !r#"<>:"/\|?*"#.contains(*c))
                .take(80)
                .collect::<String>()
                .trim()
                .trim_end_matches('.')
                .to_owned()
        })
        .filter(|stem| {
            let upper = stem.to_ascii_uppercase();
            !stem.is_empty()
                && !RESERVED.contains(&upper.as_str())
                && !((upper.starts_with("COM") || upper.starts_with("LPT")) && upper.len() == 4)
        });
    let stem = stem.unwrap_or_else(|| {
        let mut t = windows_sys::Win32::Foundation::SYSTEMTIME::default();
        unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
        format!(
            "clipkeep_{:04}{:02}{:02}_{:02}{:02}{:02}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
        )
    });
    format!("{stem}.png")
}

#[tauri::command]
fn paste_clip(app: AppHandle, id: i64) {
    hide_main(&app);
    // Off the UI thread: decoding/encoding a large image would otherwise freeze the window.
    std::thread::spawn(move || {
        let core = app.state::<Core>();
        match put_on_clipboard(&core, id) {
            Ok(()) => {
                changed(&app);
                win::paste_to_previous_window();
            }
            Err(e) => eprintln!("paste failed: {e}"),
        }
    });
}

#[tauri::command]
fn copy_clip(app: AppHandle, id: i64) {
    hide_main(&app);
    std::thread::spawn(move || {
        let core = app.state::<Core>();
        match put_on_clipboard(&core, id) {
            Ok(()) => changed(&app),
            Err(e) => eprintln!("copy failed: {e}"),
        }
    });
}

#[tauri::command]
fn hide_window(app: AppHandle) {
    hide_main(&app);
}

/// Opens the download page in the user's browser. A link inside the WebView would navigate the
/// app's own window away; going through Explorer also keeps the browser out of our elevation.
#[tauri::command]
fn open_release_page() -> CmdResult<()> {
    Command::new("explorer.exe")
        .arg(update::RELEASES_PAGE)
        .spawn()
        .map(drop)
        .map_err(to_err)
}

#[tauri::command]
fn get_update_state() -> update::State {
    update::state()
}

#[tauri::command]
async fn check_update(app: AppHandle) {
    update::check(&app, false).await;
}

#[tauri::command]
fn download_update(app: AppHandle) {
    tauri::async_runtime::spawn(update::download(app));
}

#[tauri::command]
fn cancel_update() {
    update::cancel();
}

/// The installer closes clipkeep itself; exiting right away just makes that immediate.
#[tauri::command]
fn install_update(app: AppHandle) -> CmdResult<()> {
    update::install()?;
    if let Some(core) = app.try_state::<Core>() {
        core.persist_position();
    }
    app.exit(0);
    Ok(())
}

#[tauri::command]
fn start_drag(window: WebviewWindow) -> CmdResult<()> {
    // Dragging blurs the webview; hold off the blur-hide (extended by each Moved event).
    let until = db::now_ms() + 1500;
    DRAG_HIDE_UNTIL.store(until, Ordering::Relaxed);
    SUPPRESS_HIDE_UNTIL.fetch_max(until, Ordering::Relaxed);
    window.start_dragging().map_err(to_err)
}

#[tauri::command]
fn toggle_keep_on_blur(app: AppHandle, core: State<Core>) -> CmdResult<bool> {
    let mut current = core.settings.write().map_err(to_err)?;
    let mut s = current.clone();
    s.keep_on_blur = !s.keep_on_blur;
    core.settings_file.save(&s).map_err(to_err)?;
    *current = s.clone();
    drop(current);
    let _ = app.emit("settings-changed", ());
    Ok(s.keep_on_blur)
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> CmdResult<bool> {
    Ok(autostart_task_exists() || app.autolaunch().is_enabled().unwrap_or(false))
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> CmdResult<bool> {
    apply_autostart(&app, enabled)
}

fn run_schtasks(args: &[&str]) -> CmdResult<Output> {
    Command::new("schtasks.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(to_err)
}

fn autostart_task_exists() -> bool {
    run_schtasks(&["/Query", "/TN", AUTOSTART_TASK]).is_ok_and(|output| output.status.success())
}

fn schtasks_error(output: &Output) -> String {
    win::oem_to_string(&output.stderr).trim().to_owned()
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Stored in the task's description; bump it when the definition changes so older tasks get
/// rewritten on startup.
const AUTOSTART_TASK_VERSION: &str = "clipkeep-autostart-v2";

/// `schtasks /SC ONLOGON` gets several defaults wrong for a resident app: it fires on any user's
/// logon, won't start on battery, stops when unplugged, kills the process after 72 hours and runs
/// it at below-normal priority. Spell every one of them out.
fn autostart_task_xml(executable: &str, user: &str) -> String {
    let (exe, user) = (xml_escape(executable), xml_escape(user));
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>{AUTOSTART_TASK_VERSION}</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>{AUTOSTART_ARG}</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

fn create_autostart_task() -> CmdResult<()> {
    if cfg!(debug_assertions) {
        return Err("请使用发布版设置开机自启".into());
    }
    let executable = std::env::current_exe().map_err(to_err)?;
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(domain), Ok(name)) => format!("{domain}\\{name}"),
        (_, Ok(name)) => name,
        _ => return Err("创建开机自启任务失败：无法确定当前用户".into()),
    };
    let xml = autostart_task_xml(&executable.display().to_string(), &user);
    // schtasks reads the definition from a file, as UTF-16 to match the declaration.
    let path = std::env::temp_dir().join(format!("clipkeep-autostart-{}.xml", std::process::id()));
    let bytes: Vec<u8> = [0xFEFFu16]
        .into_iter()
        .chain(xml.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect();
    std::fs::write(&path, bytes).map_err(to_err)?;
    let output = run_schtasks(&[
        "/Create",
        "/TN",
        AUTOSTART_TASK,
        "/XML",
        &path.display().to_string(),
        "/F",
    ]);
    let _ = std::fs::remove_file(&path);
    let output = output?;
    if !output.status.success() {
        return Err(format!("创建开机自启任务失败：{}", schtasks_error(&output)));
    }
    Ok(())
}

/// Keeps the logon task current: rewrites one an older version of this exe created, and takes
/// one over from an older copy elsewhere, e.g. the single-file build an installer upgrade left
/// behind. A task for a newer copy is left alone.
fn upgrade_autostart_task() {
    let Ok(output) = run_schtasks(&["/Query", "/TN", AUTOSTART_TASK, "/XML"]) else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let task = win::oem_to_string(&output.stdout);
    let Some(target) = task_command(&task) else {
        return;
    };
    let stale = if instances::is_current_exe(&target) {
        !task.contains(AUTOSTART_TASK_VERSION)
    } else {
        instances::supersedes(&target)
    };
    if stale && let Err(e) = create_autostart_task() {
        eprintln!("upgrading autostart task failed: {e}");
    }
}

/// The program a scheduled task starts, from its XML definition.
fn task_command(xml: &str) -> Option<PathBuf> {
    let start = xml.find("<Command>")? + "<Command>".len();
    let end = start + xml[start..].find("</Command>")?;
    let command = xml[start..end]
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    Some(PathBuf::from(command.trim().trim_matches('"')))
}

fn delete_autostart_task() -> CmdResult<()> {
    if autostart_task_exists() {
        let output = run_schtasks(&["/Delete", "/TN", AUTOSTART_TASK, "/F"])?;
        if !output.status.success() {
            return Err(format!("删除开机自启任务失败：{}", schtasks_error(&output)));
        }
    }
    Ok(())
}

fn apply_autostart(app: &AppHandle, enabled: bool) -> CmdResult<bool> {
    let launcher = app.autolaunch();
    if enabled {
        create_autostart_task()?;
    } else {
        delete_autostart_task()?;
    }
    // Old versions used HKCU Run. Remove it so only the elevated task starts at logon.
    if launcher.is_enabled().map_err(to_err)? {
        launcher.disable().map_err(to_err)?;
    }
    let now = autostart_task_exists();
    let _ = app.emit("autostart-changed", now);
    Ok(now)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HotkeyOption {
    id: &'static str,
    label: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsView {
    settings: Settings,
    hotkeys: Vec<HotkeyOption>,
    data_dir: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StorageUsageView {
    ordinary_bytes: i64,
    protected_bytes: i64,
    database_bytes: u64,
}

#[tauri::command]
fn get_settings(core: State<Core>) -> SettingsView {
    SettingsView {
        settings: core.settings(),
        hotkeys: win::HOTKEYS
            .iter()
            .map(|&(id, _, _, label)| HotkeyOption { id, label })
            .collect(),
        data_dir: core.data_dir.to_string_lossy().into_owned(),
    }
}

#[tauri::command]
fn get_storage_usage(core: State<Core>) -> CmdResult<StorageUsageView> {
    let usage = {
        let conn = core.conn()?;
        db::storage_usage(&conn).map_err(to_err)?
    };
    let file_len = |name: &str| -> CmdResult<u64> {
        match std::fs::metadata(core.data_dir.join(name)) {
            Ok(meta) => Ok(meta.len()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(format!("读取数据库大小失败：{error}")),
        }
    };
    // WAL contains writes not yet checkpointed into the main SQLite file.
    let database_bytes = file_len("clipkeep.db")?.saturating_add(file_len("clipkeep.db-wal")?);
    Ok(StorageUsageView {
        ordinary_bytes: usage.ordinary_bytes,
        protected_bytes: usage.protected_bytes,
        database_bytes,
    })
}

#[tauri::command]
fn update_settings(app: AppHandle, core: State<Core>, settings: Settings) -> CmdResult<Settings> {
    let mut current = core.settings.write().map_err(to_err)?;
    let old = current.clone();
    let mut new = settings;
    new.window_x = old.window_x;
    new.window_y = old.window_y;
    if win::hotkey_label(&new.hotkey).is_none() {
        return Err("未知的快捷键".into());
    }
    core.settings_file.save(&new).map_err(to_err)?;
    *current = new.clone();
    drop(current);
    win::set_hotkey(new.hotkey_enabled, &new.hotkey);
    clipboard::set_capture_images(new.capture_images);

    if new.max_storage_mb != old.max_storage_mb {
        let files = db::enforce_storage(&*core.conn()?, new.max_storage_bytes()).map_err(to_err)?;
        core.drop_files(&files);
        changed(&app);
    }
    let _ = app.emit("settings-changed", ());
    Ok(new)
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BundleGroup {
    id: i64,
    name: String,
    sort_order: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BundleClip {
    kind: String,
    text: Option<String>,
    image_b64: Option<String>,
    favorite: bool,
    pinned: bool,
    group_id: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

#[tauri::command]
async fn export_data(app: AppHandle) -> CmdResult<Option<String>> {
    let default_name = format!("clipkeep-backup-{}.ckbak", chrono_stamp());
    let Some(path) = app
        .dialog()
        .file()
        .add_filter("clipkeep 备份", &["ckbak"])
        .set_file_name(&default_name)
        .blocking_save_file()
    else {
        return Ok(None);
    };
    let path = path.into_path().map_err(to_err)?;

    // Snapshot ids and groups under a brief lock, then fetch and encode each row lock-free and
    // stream it to disk, so neither the DB nor memory is tied up for the whole export.
    let (ids, groups) = {
        let core = app.state::<Core>();
        let conn = core.conn()?;
        (
            db::clip_ids(&conn).map_err(to_err)?,
            db::list_groups(&conn).map_err(to_err)?,
        )
    };

    let groups: Vec<BundleGroup> = groups
        .into_iter()
        .map(|g| BundleGroup {
            id: g.id,
            name: g.name,
            sort_order: g.sort_order,
        })
        .collect();
    let mut count = 0usize;
    atomic_file::save_stream(&path, |file| {
        let mut w = BufWriter::new(file);
        let header = serde_json::json!({ "app": "clipkeep", "version": BUNDLE_VERSION, "exportedAt": db::now_ms() });
        write!(w, "{}", header.to_string().trim_end_matches('}'))?;
        write!(w, ",\"groups\":")?;
        serde_json::to_writer(&mut w, &groups).map_err(io::Error::other)?;
        write!(w, ",\"clips\":[")?;

        for id in ids {
            let (raw, png) = {
                let core = app.state::<Core>();
                let conn = core.conn().map_err(io::Error::other)?;
                let Some(raw) = db::export_one(&conn, id).map_err(io::Error::other)? else {
                    continue;
                };
                let png = if raw.kind == db::KIND_IMAGE {
                    Some(core.images.png_bytes(&raw.content).map_err(|error| {
                        io::Error::other(format!("图片条目 {id} 无法导出：{error}"))
                    })?)
                } else {
                    None
                };
                (raw, png)
            };
            let clip = BundleClip {
                image_b64: png.map(|bytes| STANDARD.encode(bytes)),
                text: (raw.kind == db::KIND_TEXT).then_some(raw.content),
                kind: raw.kind,
                favorite: raw.favorite,
                pinned: raw.pinned,
                group_id: raw.group_id,
                created_at: raw.created_at,
                updated_at: raw.updated_at,
            };
            if count > 0 {
                write!(w, ",")?;
            }
            serde_json::to_writer(&mut w, &clip).map_err(io::Error::other)?;
            count += 1;
        }
        write!(w, "]}}")?;
        w.flush()
    })
    .map_err(to_err)?;
    Ok(Some(format!("已导出 {count} 条到 {}", path.display())))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bundle {
    app: String,
    groups: Vec<BundleGroup>,
    clips: Vec<BundleClip>,
}

#[tauri::command]
async fn import_data(app: AppHandle) -> CmdResult<Option<String>> {
    let Some(path) = app
        .dialog()
        .file()
        .add_filter("clipkeep 备份", &["ckbak"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let path = path.into_path().map_err(to_err)?;
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > MAX_IMPORT_FILE {
        return Err("备份文件过大".into());
    }
    let data = std::fs::read(&path).map_err(to_err)?;
    let bundle: Bundle = serde_json::from_slice(&data).map_err(|_| "文件格式不正确".to_string())?;
    drop(data);
    if bundle.app != "clipkeep" {
        return Err("不是 clipkeep 备份文件".into());
    }

    // Decode every image up front (outside the lock), so the whole import is one short
    // transaction that either fully applies or rolls back — no half-imports, no orphan files.
    enum Pending {
        Text(String),
        Image(Rgba),
    }
    let mut pending = Vec::with_capacity(bundle.clips.len());
    for c in bundle.clips {
        match (c.kind.as_str(), c.text, c.image_b64) {
            (db::KIND_TEXT, Some(text), _) => pending.push((
                Pending::Text(text),
                c.favorite,
                c.pinned,
                c.group_id,
                c.created_at,
                c.updated_at,
            )),
            (db::KIND_IMAGE, _, Some(b64)) => {
                let png = STANDARD
                    .decode(b64.as_bytes())
                    .map_err(|_| "图片数据损坏".to_string())?;
                let rgba = images::decode_png(&png)?;
                pending.push((
                    Pending::Image(rgba),
                    c.favorite,
                    c.pinned,
                    c.group_id,
                    c.created_at,
                    c.updated_at,
                ));
            }
            _ => continue,
        }
    }

    let imported = {
        let core = app.state::<Core>();
        let mut conn = core.conn()?;
        let tx = conn.transaction().map_err(to_err)?;
        let existing: HashMap<String, i64> = db::list_groups(&tx)
            .map_err(to_err)?
            .into_iter()
            .map(|g| (g.name, g.id))
            .collect();
        let mut id_map: HashMap<i64, i64> = HashMap::new();
        for g in &bundle.groups {
            let new_id = match existing.get(&g.name) {
                Some(&id) => id,
                None => db::create_group(&tx, &g.name).map_err(to_err)?.id,
            };
            id_map.insert(g.id, new_id);
        }
        let mut count = 0usize;
        for (payload, favorite, pinned, group_id, created_at, updated_at) in pending {
            let group_id = group_id.and_then(|old| id_map.get(&old).copied());
            let raw = match payload {
                Pending::Text(text) => RawClip {
                    hash: db::text_hash(&text),
                    size: text.len() as i64,
                    kind: db::KIND_TEXT.into(),
                    content: text,
                    width: None,
                    height: None,
                    favorite,
                    pinned,
                    group_id,
                    created_at,
                    updated_at,
                },
                Pending::Image(rgba) => {
                    let hash = images::hash(&rgba);
                    let saved = core.images.write(&rgba, &hash)?;
                    RawClip {
                        kind: db::KIND_IMAGE.into(),
                        content: saved.file,
                        hash: saved.hash,
                        width: Some(saved.width as i64),
                        height: Some(saved.height as i64),
                        size: saved.bytes as i64,
                        favorite,
                        pinned,
                        group_id,
                        created_at,
                        updated_at,
                    }
                }
            };
            db::import_clip(&tx, &raw).map_err(to_err)?;
            count += 1;
        }
        tx.commit().map_err(to_err)?;
        let files = db::enforce_storage(&conn, core.max_bytes()).map_err(to_err)?;
        drop(conn);
        core.drop_files(&files);
        count
    };

    let _ = app.emit("groups-changed", ());
    changed(&app);
    Ok(Some(format!("已导入 {imported} 条")))
}

/// Local time as YYYYMMDD-HHMMSS, for default export file names.
fn chrono_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86400) as i64);
    let (hh, mm, ss) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    format!("{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}")
}

/// Howard Hinnant's days-to-civil-date algorithm (UTC); good enough for a file name.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn main_window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window("main")
}

fn tray_menu_window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window("tray_menu")
}

/// The tray menu only exists while it is open: its WebView keeps a ~25MB renderer alive, and the
/// menu is opened rarely. A closed menu is destroyed rather than hidden.
fn hide_tray_menu_window(app: &AppHandle) {
    if !TRAY_MENU_VISIBLE.swap(false, Ordering::Relaxed) {
        return;
    }
    TRAY_MENU_EPOCH.fetch_add(1, Ordering::Relaxed);
    // A menu still loading is destroyed by tray_menu_ready once it notices it isn't wanted.
    if TRAY_MENU_READY.load(Ordering::Relaxed)
        && let Some(window) = tray_menu_window(app)
    {
        destroy_tray_menu(&window);
    }
}

fn destroy_tray_menu(window: &WebviewWindow) {
    TRAY_MENU_READY.store(false, Ordering::Relaxed);
    if let Err(e) = window.destroy() {
        win::log_diagnostic(&format!("destroy-tray-menu-failed {e}"));
    }
}

/// The page has rendered its state: show the menu if it is still wanted.
#[tauri::command]
fn tray_menu_ready(app: AppHandle) {
    TRAY_MENU_READY.store(true, Ordering::Relaxed);
    let Some(window) = tray_menu_window(&app) else {
        return;
    };
    if TRAY_MENU_VISIBLE.load(Ordering::Relaxed) {
        reveal_tray_menu(&app, &window);
    } else {
        destroy_tray_menu(&window);
    }
}

fn popup_axis(pointer: i32, extent: i32, start: i32, end: i32) -> i32 {
    let gap = 6;
    let after = pointer + gap;
    let before = pointer - extent - gap;
    let room_after = end - after;
    let room_before = pointer - start - gap;
    let desired = if room_after >= extent || room_after >= room_before {
        after
    } else {
        before
    };
    desired.clamp(start, (end - extent).max(start))
}

fn show_tray_menu(app: &AppHandle, cursor: PhysicalPosition<f64>) {
    let pointer = app.cursor_position().unwrap_or(cursor);
    if let Ok(mut slot) = TRAY_MENU_POINTER.lock() {
        *slot = Some(pointer);
    }
    TRAY_MENU_EPOCH.fetch_add(1, Ordering::Relaxed);
    TRAY_MENU_VISIBLE.store(true, Ordering::Relaxed);
    if let Some(window) = tray_menu_window(app) {
        // Still loading: tray_menu_ready reveals it at the latest pointer.
        if TRAY_MENU_READY.load(Ordering::Relaxed) {
            reveal_tray_menu(app, &window);
        }
        return;
    }
    if TRAY_MENU_CREATING.swap(true, Ordering::Relaxed) {
        return;
    }
    // Building a WebView on the event-loop thread can deadlock on Windows while it waits for
    // WebView2, so build elsewhere and let the builder hand the work to the main thread.
    let app = app.clone();
    std::thread::spawn(move || {
        let built = app
            .config()
            .app
            .windows
            .iter()
            .find(|config| config.label == "tray_menu")
            .ok_or_else(|| "tray menu window is not configured".to_string())
            .and_then(|config| {
                tauri::WebviewWindowBuilder::from_config(&app, config)
                    .and_then(|builder| builder.build())
                    .map_err(to_err)
            });
        TRAY_MENU_CREATING.store(false, Ordering::Relaxed);
        match built {
            Ok(window) => {
                if let Ok(hwnd) = window.hwnd() {
                    win::disable_transitions(hwnd.0 as _);
                }
            }
            Err(e) => {
                TRAY_MENU_VISIBLE.store(false, Ordering::Relaxed);
                win::log_diagnostic(&format!("create-tray-menu-failed {e}"));
            }
        }
    });
}

fn reveal_tray_menu(app: &AppHandle, window: &WebviewWindow) {
    TRAY_MENU_BLUR_GRACE_UNTIL.store(db::now_ms() + 120, Ordering::Relaxed);
    let Ok(size) = window.outer_size() else {
        return;
    };
    let Some(pointer) = TRAY_MENU_POINTER.lock().ok().and_then(|slot| *slot) else {
        return;
    };
    let mut x = pointer.x.round() as i32 + 6;
    let mut y = pointer.y.round() as i32 + 6;
    if let Ok(Some(monitor)) = app.monitor_from_point(pointer.x, pointer.y) {
        let area = monitor.work_area();
        let left = area.position.x;
        let top = area.position.y;
        let right = left + area.size.width as i32;
        let bottom = top + area.size.height as i32;
        x = popup_axis(pointer.x.round() as i32, size.width as i32, left, right);
        y = popup_axis(pointer.y.round() as i32, size.height as i32, top, bottom);
    }
    let _ = window.set_position(PhysicalPosition::new(x, y));
    // Windows can restore the native caption on this secondary window. Enforce the borderless
    // style immediately before every show.
    if let Err(e) = window.set_decorations(false) {
        win::log_diagnostic(&format!("style-tray-menu-failed {e}"));
    }
    if let Err(e) = window.show() {
        TRAY_MENU_VISIBLE.store(false, Ordering::Relaxed);
        win::log_diagnostic(&format!("show-tray-menu-failed {e}"));
        return;
    }
    let _ = window.set_focus();
    let _ = window.emit("tray-menu-shown", ());
}

#[cfg(test)]
mod autostart_task_tests {
    use super::{AUTOSTART_TASK_VERSION, autostart_task_xml, paste_file_name, task_command};
    use std::path::PathBuf;

    #[test]
    fn reads_the_command_of_old_and_new_tasks() {
        let old = r#"<Exec><Command>"C:\Tools\clipkeep.exe"</Command><Arguments>--minimized</Arguments></Exec>"#;
        assert_eq!(
            task_command(old),
            Some(PathBuf::from(r"C:\Tools\clipkeep.exe"))
        );
        let new = autostart_task_xml(r"C:\Users\张三\A&B\clipkeep.exe", r"PC\张三");
        assert_eq!(
            task_command(&new),
            Some(PathBuf::from(r"C:\Users\张三\A&B\clipkeep.exe"))
        );
        assert_eq!(task_command("<Task/>"), None);
    }

    #[test]
    fn names_pasted_pictures_after_their_file_or_the_time() {
        assert_eq!(paste_file_name(Some("设计稿 v2.jpg")), "设计稿 v2.png");
        assert_eq!(paste_file_name(Some(r"a<b>:c?.png")), "abc.png");
        let timed = paste_file_name(None);
        assert!(timed.starts_with("clipkeep_") && timed.ends_with(".png"));
        assert_eq!(timed.len(), "clipkeep_20261028_120301.png".len());
        assert!(paste_file_name(Some("CON.png")).starts_with("clipkeep_"));
        assert!(paste_file_name(Some("...")).starts_with("clipkeep_"));
    }

    #[test]
    fn task_runs_on_battery_without_time_limit_for_this_user() {
        let xml = autostart_task_xml(r"C:\Users\张三\A&B\clipkeep.exe", r"PC\张三");
        for setting in [
            "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
            "<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>",
            "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
            "<Priority>4</Priority>",
            "<RunLevel>HighestAvailable</RunLevel>",
            AUTOSTART_TASK_VERSION,
        ] {
            assert!(xml.contains(setting), "missing {setting}");
        }
        // The logon trigger and the principal both name the user.
        assert_eq!(xml.matches(r"<UserId>PC\张三</UserId>").count(), 2);
        assert!(xml.contains(r"<Command>C:\Users\张三\A&amp;B\clipkeep.exe</Command>"));
    }
}

#[cfg(test)]
mod popup_position_tests {
    use super::popup_axis;

    #[test]
    fn menu_stays_near_pointer_on_both_screen_edges() {
        assert_eq!(popup_axis(5, 30, 0, 100), 11);
        assert_eq!(popup_axis(95, 30, 0, 100), 59);
        assert_eq!(popup_axis(50, 30, 0, 100), 56);
        assert_eq!(popup_axis(10, 120, 0, 100), 0);
    }
}

fn on_tray_menu_focus_changed(app: &AppHandle, focused: bool) {
    // A freshly built window reports a blur while its page is still loading, before it was ever
    // shown; that must not cancel the menu the user just asked for.
    if focused
        || !TRAY_MENU_VISIBLE.load(Ordering::Relaxed)
        || !TRAY_MENU_READY.load(Ordering::Relaxed)
    {
        return;
    }
    let epoch = TRAY_MENU_EPOCH.load(Ordering::Relaxed);
    let remaining = TRAY_MENU_BLUR_GRACE_UNTIL.load(Ordering::Relaxed) - db::now_ms();
    if remaining <= 0 {
        hide_tray_menu_window(app);
        return;
    }
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(remaining as u64));
        let target = handle.clone();
        let _ = handle.run_on_main_thread(move || {
            if TRAY_MENU_EPOCH.load(Ordering::Relaxed) == epoch
                && tray_menu_window(&target).is_some_and(|w| !w.is_focused().unwrap_or(false))
            {
                hide_tray_menu_window(&target);
            }
        });
    });
}

#[tauri::command]
fn hide_tray_menu(app: AppHandle) {
    hide_tray_menu_window(&app);
}

#[tauri::command]
fn tray_menu_action(app: AppHandle, action: String) -> CmdResult<()> {
    match action.as_str() {
        "show" => {
            hide_tray_menu_window(&app);
            show_main(&app);
        }
        "pause" => {
            set_capture_paused(&app, !clipboard::capture_paused());
        }
        "autostart" => {
            let enabled = autostart_task_exists() || app.autolaunch().is_enabled().unwrap_or(false);
            apply_autostart(&app, !enabled)?;
        }
        "quit" => {
            if let Some(core) = app.try_state::<Core>() {
                core.persist_position();
            }
            app.exit(0);
        }
        // A newer version is known: downloading and installing are confirmed in Settings.
        "update" => {
            hide_tray_menu_window(&app);
            show_main(&app);
            if let Some(w) = main_window(&app) {
                let _ = w.emit("open-about", ());
            }
        }
        _ => return Err("未知的托盘操作".into()),
    }
    Ok(())
}

/// The panel is hidden most of the time; a low target lets WebView2 trim its renderer meanwhile.
fn set_low_memory(window: &WebviewWindow, low: bool) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
        ICoreWebView2_19,
    };
    use windows_core::Interface;

    let level = if low {
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
    } else {
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
    };
    let _ = window.with_webview(move |webview| unsafe {
        // ICoreWebView2_19 needs WebView2 Runtime 114+; older runtimes simply keep the default.
        if let Ok(core) = webview.controller().CoreWebView2()
            && let Ok(core) = core.cast::<ICoreWebView2_19>()
            && let Err(e) = core.SetMemoryUsageTargetLevel(level)
        {
            win::log_diagnostic(&format!("set-memory-level-failed {e}"));
        }
    });
}

fn hide_main(app: &AppHandle) {
    win::log_diagnostic("hide-main");
    FOCUS_EPOCH.fetch_add(1, Ordering::Relaxed);
    DRAG_HIDE_UNTIL.store(0, Ordering::Relaxed);
    WINDOW_VISIBLE.store(false, Ordering::Relaxed);
    if let Some(w) = main_window(app) {
        let _ = w.emit("window-hidden", ());
        if let Err(e) = w.hide() {
            win::log_diagnostic(&format!("hide-main-failed {e}"));
        }
        set_low_memory(&w, true);
    }
    if let Some(core) = app.try_state::<Core>() {
        core.persist_position();
    }
}

fn show_main(app: &AppHandle) {
    win::log_diagnostic("show-main-begin");
    hide_tray_menu_window(app);
    FOCUS_EPOCH.fetch_add(1, Ordering::Relaxed);
    DRAG_HIDE_UNTIL.store(0, Ordering::Relaxed);
    let Some(w) = main_window(app) else {
        win::log_diagnostic("show-main-no-window");
        return;
    };
    win::remember_foreground();
    set_low_memory(&w, false);
    place_window(app, &w);
    let _ = w.unminimize();
    // Showing and focusing the WebView can emit a transient blur event.
    SUPPRESS_HIDE_UNTIL.store(db::now_ms() + SHOW_BLUR_GRACE_MS, Ordering::Relaxed);
    if let Err(e) = w.show() {
        win::log_diagnostic(&format!("show-main-failed {e}"));
        eprintln!("show main window failed: {e}");
        return;
    }
    // Call force_foreground directly — show_main already runs on the main thread
    // (dispatched by the hook dispatcher thread via run_on_main_thread).
    // The previous nested run_on_main_thread here was redundant and caused
    // re-entrance / blocking that made run_on_main_thread take >300 ms,
    // causing Windows to silently remove the low-level keyboard hook.
    if let Ok(hwnd) = w.hwnd() {
        win::force_foreground(hwnd.0 as _);
    }
    WINDOW_VISIBLE.store(true, Ordering::Relaxed);
    win::log_diagnostic("show-main-complete");
    let _ = w.emit("window-shown", ());
}

fn toggle_main(app: &AppHandle) {
    let visible = WINDOW_VISIBLE.load(Ordering::Relaxed);
    win::log_diagnostic(&format!("toggle-main visible={}", visible));
    if visible {
        hide_main(app);
    } else {
        show_main(app);
    }
}

fn place_window(app: &AppHandle, w: &WebviewWindow) {
    if let Some(core) = app.try_state::<Core>() {
        let s = core.settings();
        if let (true, Some(x), Some(y)) = (s.remember_position, s.window_x, s.window_y)
            && let Ok(Some(_)) = app.monitor_from_point(x as f64 + 40.0, y as f64 + 20.0)
        {
            let _ = w.set_position(PhysicalPosition::new(x, y));
            return;
        }
    }
    place_near_cursor(app, w);
}

fn place_near_cursor(app: &AppHandle, w: &WebviewWindow) {
    let (Ok(cursor), Ok(size)) = (app.cursor_position(), w.outer_size()) else {
        return;
    };
    let mut x = cursor.x as i32 + 4;
    let mut y = cursor.y as i32 + 4;
    if let Ok(Some(monitor)) = app.monitor_from_point(cursor.x, cursor.y) {
        let area = monitor.work_area();
        let (left, top) = (area.position.x, area.position.y);
        let right = left + area.size.width as i32;
        let bottom = top + area.size.height as i32;
        x = x.min(right - size.width as i32).max(left);
        y = y.min(bottom - size.height as i32).max(top);
    }
    let _ = w.set_position(PhysicalPosition::new(x, y));
}

fn on_window_moved(app: &AppHandle, pos: PhysicalPosition<i32>) {
    let now = db::now_ms();
    if now < DRAG_HIDE_UNTIL.load(Ordering::Relaxed) {
        DRAG_HIDE_UNTIL.store(now + 1500, Ordering::Relaxed);
        SUPPRESS_HIDE_UNTIL.fetch_max(now + 800, Ordering::Relaxed);
    }
    let Some(core) = app.try_state::<Core>() else {
        return;
    };
    let Ok(mut s) = core.settings.write() else {
        return;
    };
    if s.remember_position && (s.window_x, s.window_y) != (Some(pos.x), Some(pos.y)) {
        s.window_x = Some(pos.x);
        s.window_y = Some(pos.y);
        core.position_dirty.store(true, Ordering::Relaxed);
    }
}

/// Pinned (keep_on_blur) or mid-drag: don't auto-hide.
fn should_keep_open(app: &AppHandle) -> bool {
    if db::now_ms() < SUPPRESS_HIDE_UNTIL.load(Ordering::Relaxed) {
        return true;
    }
    app.try_state::<Core>()
        .map(|c| c.settings().keep_on_blur)
        .unwrap_or(false)
}

fn on_main_focus_changed(app: &AppHandle, focused: bool) {
    let epoch = FOCUS_EPOCH.fetch_add(1, Ordering::Relaxed) + 1;
    let remaining = SUPPRESS_HIDE_UNTIL.load(Ordering::Relaxed) - db::now_ms();
    win::log_diagnostic(&format!(
        "main-focus focused={focused} visible={} hide-delay-ms={remaining}",
        WINDOW_VISIBLE.load(Ordering::Relaxed)
    ));
    if focused || !WINDOW_VISIBLE.load(Ordering::Relaxed) {
        return;
    }
    if !should_keep_open(app) {
        hide_main(app);
        return;
    }
    // A real blur during the short show/drag grace period used to be discarded forever.
    // Check again when the grace expires, unless focus or visibility has changed meanwhile.
    let app = app.clone();
    std::thread::spawn(move || {
        loop {
            if FOCUS_EPOCH.load(Ordering::Relaxed) != epoch {
                return;
            }
            let remaining = SUPPRESS_HIDE_UNTIL.load(Ordering::Relaxed) - db::now_ms();
            if remaining <= 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(remaining.min(500) as u64));
        }
        let target = app.clone();
        let _ = app.run_on_main_thread(move || {
            if FOCUS_EPOCH.load(Ordering::Relaxed) == epoch
                && WINDOW_VISIBLE.load(Ordering::Relaxed)
                && !should_keep_open(&target)
                && main_window(&target).is_some_and(|window| !window.is_focused().unwrap_or(false))
            {
                win::log_diagnostic("deferred-blur-hide");
                hide_main(&target);
            }
        });
    });
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("clipkeep 剪贴板")
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button,
                button_state: MouseButtonState::Up,
                position,
                ..
            } = event
            {
                match button {
                    MouseButton::Left => toggle_main(tray.app_handle()),
                    MouseButton::Right => show_tray_menu(tray.app_handle(), position),
                    _ => {}
                }
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// Only confirmed corruption starts a fresh database. I/O, permission, and schema-version
/// errors must preserve the existing history for diagnosis or a newer app version.
fn open_db(data_dir: &std::path::Path) -> CmdResult<Connection> {
    let path = data_dir.join("clipkeep.db");
    match db::open(&path) {
        Ok(conn) => Ok(conn),
        Err(e)
            if matches!(
                &e,
                rusqlite::Error::SqliteFailure(error, _)
                    if matches!(error.code, rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
            ) =>
        {
            eprintln!("history DB unusable ({e}); starting a fresh one");
            let stamp = chrono_stamp();
            for suffix in ["", "-wal", "-shm"] {
                let from = data_dir.join(format!("clipkeep.db{suffix}"));
                if from.exists() {
                    std::fs::rename(
                        &from,
                        data_dir.join(format!("clipkeep-corrupt-{stamp}.db{suffix}")),
                    )
                    .map_err(|error| format!("无法保留损坏的历史数据库：{error}"))?;
                }
            }
            db::open(&path).map_err(to_err)
        }
        Err(error) => Err(format!("打开历史数据库失败，原文件已保留：{error}")),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if !cfg!(debug_assertions) {
        // Before the single-instance check, which would otherwise hand over to that copy.
        instances::retire_older_instance();
    }
    tauri::Builder::default()
        // With WebView2 focused, Tauri's default device filter can starve the host process's
        // WH_KEYBOARD_LL hook. Always keeps Win+V observable inside our own window too.
        .device_event_filter(tauri::DeviceEventFilter::Always)
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app)
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_ARG]),
        ))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();

            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let settings_file = SettingsFile::new(&data_dir);
            win::start_hotkey_diagnostics(&data_dir);
            let mut settings = settings_file.load();
            if !win::set_hotkey(settings.hotkey_enabled, &settings.hotkey) {
                settings.hotkey = Settings::default().hotkey;
                win::set_hotkey(settings.hotkey_enabled, &settings.hotkey);
            }
            clipboard::set_capture_images(settings.capture_images);

            if !cfg!(debug_assertions) && handle.autolaunch().is_enabled().unwrap_or(false) {
                if let Err(e) = create_autostart_task() {
                    eprintln!("migrating autostart to elevated task failed: {e}");
                } else if let Err(e) = handle.autolaunch().disable() {
                    eprintln!("removing legacy autostart entry failed: {e}");
                }
            }
            if let Ok(marker) =
                std::env::current_exe().map(|exe| exe.with_file_name(AUTOSTART_MARKER))
                && marker.exists()
            {
                // Keep the marker on failure so the next launch tries again.
                match create_autostart_task() {
                    Ok(()) => {
                        let _ = std::fs::remove_file(&marker);
                    }
                    Err(e) => eprintln!("enabling autostart after install failed: {e}"),
                }
            }
            if !cfg!(debug_assertions) {
                // Two schtasks round trips; keep them off the startup path.
                std::thread::spawn(upgrade_autostart_task);
            }
            for window in [main_window(&handle), tray_menu_window(&handle)]
                .into_iter()
                .flatten()
            {
                if let Ok(hwnd) = window.hwnd() {
                    win::disable_transitions(hwnd.0 as _);
                }
            }
            build_tray(&handle)?;
            if !cfg!(debug_assertions) && settings.check_update_on_start {
                let h = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(update::STARTUP_DELAY);
                    tauri::async_runtime::block_on(update::check(&h, true));
                });
            }

            let db = open_db(&data_dir).map_err(std::io::Error::other)?;
            let images = ImageStore::new(&data_dir)?;
            // One-time cleanup: drop files left behind by a crash between writing and saving a row.
            if let Ok(referenced) = db::referenced_files(&db) {
                images.collect_garbage(&referenced);
            }

            let core = Core {
                db: Mutex::new(db),
                images,
                lan: lan::LanService::new(&data_dir),
                settings: RwLock::new(settings.clone()),
                settings_file,
                position_dirty: AtomicBool::new(false),
                data_dir,
            };
            app.manage(core);
            handle.state::<Core>().lan.restore(&handle);

            let monitor_handle = handle.clone();
            clipboard::start_monitor(move |captured| on_local_copy(&monitor_handle, captured));

            let hook_handle = handle.clone();
            win::install_hotkey_hook(move || {
                let h = hook_handle.clone();
                if let Err(e) = hook_handle.run_on_main_thread(move || toggle_main(&h)) {
                    win::log_diagnostic(&format!("dispatch-to-main-failed {e}"));
                }
            });

            if !std::env::args().any(|a| a == AUTOSTART_ARG) {
                show_main(&handle);
            } else if let Some(w) = main_window(&handle) {
                // Started at logon into the tray: hidden until the first hotkey.
                set_low_memory(&w, true);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "tray_menu" {
                match event {
                    WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        hide_tray_menu_window(window.app_handle());
                    }
                    WindowEvent::Focused(focused) => {
                        on_tray_menu_focus_changed(window.app_handle(), *focused)
                    }
                    _ => {}
                }
                return;
            }
            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    hide_main(window.app_handle());
                }
                WindowEvent::Focused(focused) => {
                    on_main_focus_changed(window.app_handle(), *focused)
                }
                WindowEvent::Moved(pos) => on_window_moved(window.app_handle(), *pos),
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_clips,
            get_clip_text,
            update_clip_text,
            get_capture_paused,
            toggle_capture_paused,
            get_lan_state,
            set_lan_receive,
            add_lan_peer,
            remove_lan_peer,
            rotate_lan_code,
            copy_lan_code,
            send_clip_to_peer,
            toggle_favorite,
            toggle_pinned,
            move_clip,
            delete_clip,
            clear_data,
            list_groups,
            create_group,
            rename_group,
            delete_group,
            reorder_groups,
            paste_clip,
            copy_clip,
            hide_window,
            open_release_page,
            get_update_state,
            check_update,
            download_update,
            cancel_update,
            install_update,
            hide_tray_menu,
            tray_menu_ready,
            tray_menu_action,
            start_drag,
            toggle_keep_on_blur,
            get_autostart,
            set_autostart,
            get_settings,
            get_storage_usage,
            update_settings,
            export_data,
            import_data
        ])
        .run(tauri::generate_context!())
        .expect("error while running clipkeep");
}
