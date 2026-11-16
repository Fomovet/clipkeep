//! Taking over from an older copy of clipkeep — macOS counterpart.
//!
//! On macOS `tauri-plugin-single-instance` already hands a second launch to the running copy,
//! and the LaunchAgent always points at the app bundle the user installed. There is no scheduled
//! task or stray single-file exe to retire, so this is intentionally a no-op.

pub fn retire_older_instance() {
    // Nothing to do: the single-instance plugin owns this on macOS.
}
