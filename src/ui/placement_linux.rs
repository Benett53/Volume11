//! Where the mixer window opens on Linux, and whether it is shown.
//!
//! The window runs through XWayland (see `main.rs`), so both questions go to
//! the X server. Under Wayland the X root window carries no work area, which
//! is how Windows finds the taskbar, so the window is placed next to where the
//! tray icon was clicked instead: KDE passes that position along with the
//! click. Opened any other way, it goes to the bottom right corner of the
//! primary monitor, clear of a typical bottom panel.

use std::sync::OnceLock;

use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, MapState};
use x11rb::rust_connection::RustConnection;

/// Gap between the window and the screen edge or the panel.
const MARGIN: f32 = 12.0;
/// Room left for a bottom panel when there is no click position to go by.
const PANEL_ALLOWANCE: f32 = 64.0;

fn connection() -> Option<&'static (RustConnection, usize)> {
    static CONNECTION: OnceLock<Option<(RustConnection, usize)>> = OnceLock::new();
    CONNECTION
        .get_or_init(|| x11rb::connect(None).ok())
        .as_ref()
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl Rect {
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// Monitor rectangles in X pixels, primary first.
fn monitors() -> Vec<Rect> {
    let Some((conn, screen)) = connection() else {
        return Vec::new();
    };
    let root = conn.setup().roots[*screen].root;
    let Some(reply) = conn
        .randr_get_monitors(root, true)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
    else {
        return Vec::new();
    };
    let mut list: Vec<(bool, Rect)> = reply
        .monitors
        .iter()
        .map(|m| {
            (
                m.primary,
                Rect {
                    x: f32::from(m.x),
                    y: f32::from(m.y),
                    width: f32::from(m.width),
                    height: f32::from(m.height),
                },
            )
        })
        .collect();
    list.sort_by_key(|(primary, _)| !primary);
    list.into_iter().map(|(_, rect)| rect).collect()
}

/// Top left corner for a window of `width`×`height` pixels opened from a
/// click at (`x`, `y`) on `monitor`: centred on the click, on the side of it
/// that faces the middle of the screen, and kept on the monitor.
fn near_click(monitor: Rect, x: f32, y: f32, width: f32, height: f32) -> (f32, f32) {
    let left = (x - width / 2.0)
        .min(monitor.x + monitor.width - width - MARGIN)
        .max(monitor.x + MARGIN);
    let top = if y > monitor.y + monitor.height / 2.0 {
        y - height - MARGIN
    } else {
        y + MARGIN
    };
    let top = top
        .min(monitor.y + monitor.height - height - MARGIN)
        .max(monitor.y + MARGIN);
    (left, top)
}

fn bottom_right_of(monitor: Rect, width: f32, height: f32) -> (f32, f32) {
    (
        (monitor.x + monitor.width - width - MARGIN).max(monitor.x),
        (monitor.y + monitor.height - height - PANEL_ALLOWANCE).max(monitor.y),
    )
}

/// Where to open the window, in the points egui expects.
///
/// Same signature as on Windows. `current_scale` converts pixels to points.
pub fn bottom_right(size: egui::Vec2, current_scale: f32) -> Option<egui::Pos2> {
    let scale = if current_scale > 0.0 {
        current_scale
    } else {
        1.0
    };
    let (width, height) = (size.x * scale, size.y * scale);
    let monitors = monitors();

    let (x, y) = match crate::tray::last_click() {
        Some((cx, cy)) => {
            let (cx, cy) = (cx as f32, cy as f32);
            let monitor = monitors
                .iter()
                .find(|m| m.contains(cx, cy))
                .or(monitors.first())
                .copied()?;
            near_click(monitor, cx, cy, width, height)
        }
        None => bottom_right_of(*monitors.first()?, width, height),
    };

    Some(egui::pos2(x / scale, y / scale))
}

/// Whether the X window is mapped, i.e. really on screen.
pub fn is_mapped(window: u32) -> bool {
    let Some((conn, _)) = connection() else {
        return false;
    };
    conn.get_window_attributes(window)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|attributes| attributes.map_state == MapState::VIEWABLE)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 3440.0,
        height: 1440.0,
    };

    #[test]
    fn click_on_a_bottom_panel_opens_above_it() {
        let (x, y) = near_click(SCREEN, 3300.0, 1415.0, 424.0, 572.0);
        assert_eq!(x, 3440.0 - 424.0 - MARGIN);
        assert_eq!(y, 1415.0 - 572.0 - MARGIN);
    }

    #[test]
    fn click_on_a_top_panel_opens_below_it() {
        let (x, y) = near_click(SCREEN, 1000.0, 20.0, 424.0, 572.0);
        assert_eq!(x, 1000.0 - 212.0);
        assert_eq!(y, 20.0 + MARGIN);
    }

    #[test]
    fn fallback_is_on_screen() {
        let (x, y) = bottom_right_of(SCREEN, 424.0, 572.0);
        assert!(x + 424.0 <= 3440.0 && y + 572.0 <= 1440.0);
    }
}
