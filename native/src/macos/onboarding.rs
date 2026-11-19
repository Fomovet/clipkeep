//! First-run Accessibility guide for macOS.
//!
//! Synthetic ⌘V needs Accessibility permission. That prompt used to wait until the first paste.
//! The guide is shown at launch instead, until the user postpones it. Once the app is trusted,
//! a saved postponement is cleared, so turning the permission off later can surface the guide
//! again. Paste still asks macOS itself if this process has not asked yet.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const MARKER: &str = "accessibility-onboarding";

/// While the guide is up, losing focus to System Settings must not hide the panel.
static HOLD_OPEN: AtomicBool = AtomicBool::new(false);

pub fn should_show_guide(trusted: bool, dismissed: bool) -> bool {
    !trusted && !dismissed
}

pub fn dismissed(data_dir: &Path) -> bool {
    data_dir.join(MARKER).is_file()
}

pub fn dismiss(data_dir: &Path) -> io::Result<()> {
    crate::atomic_file::save(&data_dir.join(MARKER), b"dismissed\n")?;
    set_hold(false);
    Ok(())
}

/// Forgets a postponement. Missing file is success: the guide was never postponed.
pub fn clear_dismissal(data_dir: &Path) -> io::Result<()> {
    match std::fs::remove_file(data_dir.join(MARKER)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn should_present(data_dir: &Path) -> bool {
    should_show_guide(crate::win::accessibility_trusted(), dismissed(data_dir))
}

pub fn set_hold(hold: bool) {
    HOLD_OPEN.store(hold, Ordering::Relaxed);
}

pub fn hold_open() -> bool {
    HOLD_OPEN.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guide_shows_until_trusted_or_postponed() {
        assert!(should_show_guide(false, false));
        assert!(!should_show_guide(true, false));
        assert!(!should_show_guide(false, true));
        assert!(!should_show_guide(true, true));
    }

    #[test]
    fn postponing_writes_a_marker_that_clearing_removes() {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let dir = std::env::temp_dir().join(format!(
            "clipkeep-onboarding-{}",
            u64::from_ne_bytes(random)
        ));
        std::fs::create_dir(&dir).unwrap();
        assert!(!dismissed(&dir));
        dismiss(&dir).unwrap();
        assert!(dismissed(&dir));
        assert!(!should_show_guide(false, dismissed(&dir)));
        clear_dismissal(&dir).unwrap();
        assert!(!dismissed(&dir));
        assert!(should_show_guide(false, dismissed(&dir)));
        clear_dismissal(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
