//! Tray icon for Linux, as a StatusNotifierItem over D-Bus.
//!
//! KDE Plasma shows these natively; GNOME needs the AppIndicator extension.
//! `ksni` runs the D-Bus service on its own thread, so this module only maps
//! clicks and menu entries onto [`TrayMessage`]s.

use std::sync::{Mutex, OnceLock};

use crossbeam_channel::{Receiver, Sender, unbounded};
use ksni::blocking::TrayMethods;
use ksni::menu::{MenuItem, StandardItem};

use crate::pngutil;

/// What the tray reports to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayMessage {
    /// Left click: show the mixer if hidden, hide it if already visible.
    Toggle,
    /// Context menu "Open", or a second start of Volume11.
    Show,
    /// Context menu "Settings".
    ShowSettings,
    /// Context menu "Quit", or `volume11 --quit`.
    Quit,
}

static SENDER: OnceLock<Sender<TrayMessage>> = OnceLock::new();
static WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
/// Screen position of the last click on the icon, used to open the window next to it.
static LAST_CLICK: Mutex<Option<(i32, i32)>> = Mutex::new(None);

/// Where the tray icon was last clicked, if the panel reported it.
pub fn last_click() -> Option<(i32, i32)> {
    *LAST_CLICK.lock().ok()?
}

/// Hand a message to the UI. Also used by the single-instance listener.
pub fn notify(message: TrayMessage) {
    if let Some(sender) = SENDER.get() {
        let _ = sender.send(message);
    }
    if let Some(wake) = WAKE.get() {
        wake();
    }
}

struct VolumeTray {
    icon: Vec<ksni::Icon>,
}

impl ksni::Tray for VolumeTray {
    fn id(&self) -> String {
        "volume11".into()
    }

    fn title(&self) -> String {
        "Volume11".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Volume11".into(),
            ..Default::default()
        }
    }

    fn activate(&mut self, x: i32, y: i32) {
        // Hosts without a position send 0,0.
        if (x, y) != (0, 0)
            && let Ok(mut last) = LAST_CLICK.lock()
        {
            *last = Some((x, y));
        }
        notify(TrayMessage::Toggle);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Open".into(),
                activate: Box::new(|_: &mut Self| notify(TrayMessage::Show)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Settings".into(),
                activate: Box::new(|_: &mut Self| notify(TrayMessage::ShowSettings)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|_: &mut Self| notify(TrayMessage::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// ARGB32 in network byte order, as the StatusNotifierItem spec wants it.
fn pixmaps() -> Vec<ksni::Icon> {
    [pngutil::OWN_ICON_32, pngutil::OWN_ICON_64]
        .into_iter()
        .filter_map(pngutil::decode)
        .map(|image| ksni::Icon {
            width: image.width as i32,
            height: image.height as i32,
            data: image
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[3], p[0], p[1], p[2]])
                .collect(),
        })
        .collect()
}

/// Register the tray icon. The returned receiver stays valid for the process
/// lifetime. Without a StatusNotifierItem host the icon simply does not appear;
/// the window can still be opened by starting Volume11 again.
pub fn spawn<F>(wake: F) -> Receiver<TrayMessage>
where
    F: Fn() + Send + Sync + 'static,
{
    let (tx, rx) = unbounded();
    let _ = SENDER.set(tx);
    let _ = WAKE.set(Box::new(wake));

    let tray = VolumeTray { icon: pixmaps() };
    match tray.spawn() {
        // The handle only matters for updating or removing the icon; the
        // service keeps running after it is dropped and ends with the process.
        Ok(handle) => std::mem::forget(handle),
        Err(error) => eprintln!("volume11: tray icon unavailable: {error}"),
    }

    rx
}
