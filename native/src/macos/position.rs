//! macOS popup placement.
//!
//! tao reports both the cursor and window positions in a coordinate space whose x is display
//! points and whose y is `CGDisplayPixelsHigh(main) - CocoaY` (scaled by the main display's
//! backing scale). Its `monitor_from_point()` however looks displays up in plain global display
//! points, so on a Retina screen that lookup fails and the popup is never clamped to the screen.
//! AppKit's `NSScreen.visibleFrame` gives the usable area (menu bar and Dock excluded) directly
//! in the coordinate space the cursor uses, so clamping against it keeps the panel on screen.

use core_graphics::display::CGDisplay;
use objc2_app_kit::{NSEvent, NSScreen};
use objc2_foundation::MainThreadMarker;

/// Usable screen rectangle in tao's logical (`LogicalPosition`) coordinate space.
#[derive(Clone, Copy, Debug)]
pub struct PopupArea {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    /// Backing scale of the screen, for turning `outer_size()` physical pixels into points.
    pub scale: f64,
}

/// `CGDisplayPixelsHigh` is a *pixel* height. tao subtracts Cocoa's bottom-left y from it before
/// applying the backing scale, so both `cursor_position()` and `LogicalPosition` live here.
fn display_height() -> f64 {
    CGDisplay::main().pixels_high() as f64
}

/// Cursor in tao's logical coordinate space. Keeps the exact anchor the Windows build uses
/// (the mouse plus a small offset), only expressed in units `set_position()` understands.
pub fn cursor_point() -> (f64, f64) {
    let mouse = NSEvent::mouseLocation();
    (mouse.x, display_height() - mouse.y)
}

fn visible_area(screen: &NSScreen, height: f64) -> PopupArea {
    let frame = screen.visibleFrame();
    PopupArea {
        left: frame.origin.x,
        top: height - (frame.origin.y + frame.size.height),
        right: frame.origin.x + frame.size.width,
        bottom: height - frame.origin.y,
        scale: screen.backingScaleFactor().max(1.0),
    }
}

/// Visible area of the screen under the cursor. Falls back to the main screen so a popup is
/// always clamped, even when the cursor sits on the seam between two displays.
pub fn popup_area(point: (f64, f64)) -> PopupArea {
    // Placement runs on Tauri's main thread (show_main / tray click).
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let height = display_height();
    let screens = NSScreen::screens(mtm);
    let mut first: Option<PopupArea> = None;
    for index in 0..screens.count() {
        let screen = screens.objectAtIndex(index);
        let area = visible_area(&screen, height);
        let frame = screen.frame();
        let on_screen = point.0 >= frame.origin.x
            && point.0 <= frame.origin.x + frame.size.width
            && point.1 >= height - (frame.origin.y + frame.size.height)
            && point.1 <= height - frame.origin.y;
        if on_screen {
            return area;
        }
        if first.is_none() {
            first = Some(area);
        }
    }
    first.unwrap_or_else(|| {
        NSScreen::mainScreen(mtm).map_or(
            PopupArea {
                left: 0.0,
                top: 0.0,
                right: f64::MAX / 4.0,
                bottom: height,
                scale: 1.0,
            },
            |screen| visible_area(&screen, height),
        )
    })
}
