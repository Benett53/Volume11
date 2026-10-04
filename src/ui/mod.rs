//! The mixer window.
//!
//! Runs in egui's reactive mode: frames are only produced in response to input or
//! to a wake-up from the audio or tray thread. An idle Volume11 draws nothing.

mod appicon;
mod icons;
mod placement;
mod theme;
mod widgets;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Align, Color32, FontId, Layout, RichText, Sense, Vec2, ViewportCommand};

use crate::audio::{
    AudioHandle, Command, DeviceInfo, Event, MASTER_KEY, SYSTEM_KEY, SessionKind, SessionSnapshot,
};
use crate::autostart;
use crate::config::{Config, UnknownAppPolicy};
use crate::tray::TrayMessage;
use appicon::IconCache;
use icons::Icon;
use theme::{ACCENT_CHOICES, CONTENT_MARGIN, Palette, accent_to_hex, parse_accent};
use widgets::{
    chip_button, colour_swatch, device_option, device_selector, icon_button, number_field,
    volume_slider,
};

pub const WINDOW_WIDTH: f32 = 424.0;
pub const WINDOW_HEIGHT: f32 = 572.0;

/// How long a status line stays on screen.
const STATUS_TIMEOUT: Duration = Duration::from_secs(3);

/// Edge length of the application icon in a row.
const ICON_SIZE: f32 = 21.0;
/// Width of the editable percentage box.
const NUMBER_FIELD_WIDTH: f32 = 60.0;
/// Identity for the settings panel's percentage box; no session uses this key.
const DEFAULT_VOLUME_KEY: &str = "@default";

/// Where the window waits before it has ever been shown.
///
/// Windows parks minimised windows at -32000 too, so this is well outside any
/// real desktop no matter how the monitors are arranged.
pub const OFFSCREEN: egui::Pos2 = egui::pos2(-32000.0, -32000.0);

#[derive(PartialEq, Eq, Clone, Copy)]
enum View {
    Mixer,
    Settings,
}

pub struct VolumeApp {
    audio: AudioHandle,
    tray: crossbeam_channel::Receiver<TrayMessage>,

    config: Config,
    config_path: PathBuf,

    sessions: Vec<SessionSnapshot>,
    device_name: String,
    /// Endpoint id of the current default device; the key into the per-device
    /// saved levels. Empty until the audio engine has reported it.
    device_id: String,
    /// Active playback devices that can be switched to.
    devices: Vec<DeviceInfo>,
    /// Whether the device list under the header is unfolded.
    device_menu_open: bool,

    view: View,
    palette: Palette,
    visible: bool,

    status: Option<(String, Instant)>,
    /// A problem the audio side recovers from by itself, such as no playback
    /// device being present. Shown until audio is available again, without
    /// replacing the mixer the way `fatal` does.
    notice: Option<String>,
    fatal: Option<String>,
    /// Set by `--show` before the first frame.
    show_requested: bool,

    /// Set once the user has really chosen to quit, so the close request that
    /// follows is allowed through instead of being turned into a hide.
    quitting: bool,

    /// Application icons, loaded once per executable.
    icons: IconCache,
    /// Session key and raw text of the percentage field being typed into.
    editing: Option<(String, String)>,
}

impl VolumeApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        audio: AudioHandle,
        tray: crossbeam_channel::Receiver<TrayMessage>,
        config: Config,
        config_path: PathBuf,
    ) -> Self {
        let palette = Palette::new(
            config.settings.oled_black,
            parse_accent(&config.settings.accent),
        );
        theme::apply(&cc.egui_ctx, &palette);

        Self {
            audio,
            tray,
            config,
            config_path,
            sessions: Vec::new(),
            device_name: String::new(),
            device_id: String::new(),
            devices: Vec::new(),
            device_menu_open: false,
            view: View::Mixer,
            palette,
            visible: false,
            status: None,
            notice: None,
            fatal: None,
            show_requested: false,
            quitting: false,
            icons: IconCache::default(),
            editing: None,
        }
    }

    /// Ask for the window to be shown on the first `logic` call, once egui has a
    /// context to send viewport commands through.
    pub fn request_show(&mut self) {
        self.show_requested = true;
    }

    fn refresh_palette(&mut self, ctx: &egui::Context) {
        self.palette = Palette::new(
            self.config.settings.oled_black,
            parse_accent(&self.config.settings.accent),
        );
        theme::apply(ctx, &self.palette);
    }

    fn set_status(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), Instant::now()));
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        let size = window_size();
        let scale = ctx.pixels_per_point();

        // Win+D and "show desktop" minimise the window rather than hiding it.
        // A minimised window keeps a 160x28 client area, which the renderer
        // faithfully draws the whole interface into; the result looks shredded
        // and survives until the process is restarted. Clearing the minimised
        // state and restating the size is what makes reopening reliable.
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));

        // Always the bottom right corner of the primary monitor's work area.
        // Dragging the window elsewhere is for the moment you need it out of the
        // way; the next open puts it back where it belongs.
        if let Some(position) = placement::bottom_right(size, scale) {
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(position));
        }

        self.apply_window_level(ctx);
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Focus);

        self.visible = true;

        // Levels may have moved while hidden.
        self.audio.send(Command::Refresh);
    }

    /// Apply the saved "always on top" preference to the real window.
    ///
    /// This has to happen on every show, not just when the pin is clicked: the
    /// setting is persisted, so a restart has to honour it too.
    fn apply_window_level(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(
            if self.config.settings.always_on_top {
                egui::viewport::WindowLevel::AlwaysOnTop
            } else {
                egui::viewport::WindowLevel::Normal
            },
        ));
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        // Never leave the window hidden *and* minimised: the next show would
        // otherwise have to undo two states instead of one.
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        self.visible = false;
    }

    fn drain_tray(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.tray.try_recv() {
            match message {
                TrayMessage::Toggle => {
                    if self.visible {
                        self.hide_window(ctx);
                    } else {
                        self.view = View::Mixer;
                        self.show_window(ctx);
                    }
                }
                TrayMessage::Show => {
                    self.view = View::Mixer;
                    self.show_window(ctx);
                }
                TrayMessage::ShowSettings => {
                    self.view = View::Settings;
                    self.show_window(ctx);
                }
                TrayMessage::Quit => {
                    let _ = self.config.save(&self.config_path);
                    self.quitting = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }
    }

    fn drain_audio(&mut self) {
        while let Ok(event) = self.audio.events.try_recv() {
            match event {
                Event::Sessions(list) => {
                    // Sessions only arrive while a device is attached, so any
                    // "no playback device" notice is out of date now.
                    self.notice = None;
                    if let Some(master) = list.iter().find(|s| s.kind == SessionKind::Master) {
                        self.device_name = master.label.clone();
                    }

                    let live: Vec<String> =
                        list.iter().filter_map(|s| s.executable.clone()).collect();
                    self.icons.retain(&live);

                    self.sessions = list;
                }
                Event::Changed { key, volume, muted } => {
                    if let Some(session) = self.sessions.iter_mut().find(|s| s.key == key) {
                        session.volume = volume;
                        session.muted = muted;
                    }
                }
                Event::DeviceChanged { id, name } => {
                    self.set_status(name.clone());
                    self.device_name = name;
                    self.device_id = id;
                    self.adopt_legacy_levels();

                    // Each device has its own saved levels. Windows keeps
                    // per-program volume per device too, so after a switch the
                    // new device's levels are applied to everything playing.
                    if self.config.settings.auto_apply {
                        let entries = self.config.apply_list(&self.device_id);
                        if !entries.is_empty() {
                            self.audio.send(Command::ApplyAll(entries));
                        }
                    }
                }
                Event::Devices { list, current } => {
                    if self.device_id.is_empty() {
                        self.device_id = current;
                        if let Some(device) = list.iter().find(|d| d.id == self.device_id) {
                            self.device_name = device.name.clone();
                        }
                        self.adopt_legacy_levels();
                    }
                    self.devices = list;
                }
                Event::SessionAdded { key, label } => self.apply_on_start(&key, &label),
                Event::Problem(message) => self.notice = Some(message),
                Event::Fatal(message) => self.fatal = Some(message),
            }
        }
    }

    /// The feature that justifies the whole application: a program starts playing
    /// and immediately gets the level it had last time.
    fn apply_on_start(&mut self, key: &str, label: &str) {
        if !self.config.settings.auto_apply {
            return;
        }

        if let Some(entry) = self.config.get(&self.device_id, key) {
            self.audio.send(Command::SetVolume {
                key: key.to_string(),
                volume: entry.volume,
            });
            self.audio.send(Command::SetMute {
                key: key.to_string(),
                muted: entry.muted,
            });
            self.set_status(format!("{label}: {} %", entry.volume));
        } else if self.config.settings.unknown_app_policy == UnknownAppPolicy::ApplyDefault {
            let volume = self.config.settings.default_volume;
            self.audio.send(Command::SetVolume {
                key: key.to_string(),
                volume,
            });
            self.set_status(format!("{label}: {volume} %"));
        }
    }

    /// Store the current levels of everything on screen, for the current device.
    fn save_current(&mut self) {
        if self.device_id.is_empty() {
            return;
        }

        for session in &self.sessions {
            self.config.set(
                &self.device_id,
                &self.device_name,
                &session.key,
                session.volume,
                session.muted,
                &session.label,
            );
        }

        let count = self
            .config
            .device(&self.device_id)
            .map_or(0, |entry| entry.apps.len());

        match self.config.save(&self.config_path) {
            Ok(()) => self.set_status(format!("{count} saved")),
            Err(error) => self.set_status(format!("Error: {error}")),
        }
    }

    /// Move levels saved by version 1 onto the device in use now, once.
    fn adopt_legacy_levels(&mut self) {
        let (id, name) = (self.device_id.clone(), self.device_name.clone());
        if self.config.adopt_legacy(&id, &name) {
            self.persist_settings();
        }
    }

    /// Push the saved levels back onto whatever is currently playing.
    fn sync_saved(&mut self) {
        let entries = self.config.apply_list(&self.device_id);
        let count = entries.len();
        self.audio.send(Command::ApplyAll(entries));
        self.set_status(format!("{count} applied"));
    }

    fn persist_settings(&mut self) {
        if let Err(error) = self.config.save(&self.config_path) {
            self.set_status(format!("Error: {error}"));
        }
    }
}

impl eframe::App for VolumeApp {
    /// Transparent so the rounded window corners are not drawn onto a black square.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    /// Runs even while the window is hidden, which is what lets a tray-only app
    /// react to the icon being clicked without drawing anything in the meantime.
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.show_requested {
            self.show_requested = false;
            self.show_window(ctx);
        }

        // Closing the window — the header button, Alt+F4, the taskbar — only
        // sends Volume11 back to the notification area. Quitting for real is
        // deliberately limited to "Quit" in the tray menu.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.hide_window(ctx);
        }

        self.drain_tray(ctx);
        self.drain_audio();

        // eframe force-shows the window once, after its first rendered frame.
        // Hiding it again here is what keeps Volume11 in the tray when Windows
        // starts it. It runs after `drain_tray`, so a tray click that just asked
        // for the window is not undone.
        //
        // The command is sent only while Windows reports the window as shown.
        // Every viewport command requests a repaint, so sending it on every pass
        // while hidden made each pass schedule the next one: an idle Volume11
        // spun through ~0.6 % of a core doing nothing. egui's own visibility
        // flag only tracks minimised and covered windows, not hidden ones, which
        // is why Windows is asked directly.
        if !self.visible && window_is_shown(frame) {
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        }

        if let Some((_, since)) = self.status
            && since.elapsed() > STATUS_TIMEOUT
        {
            self.status = None;
        }

        if !self.visible {
            return;
        }

        // The window deliberately does NOT hide when it loses focus. Clicking
        // away used to close it, which made it impossible to use alongside the
        // program whose volume was being adjusted. It closes on Esc, on the
        // header's close button, or from the tray.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.hide_window(ctx);
        }

        // Keep the status line disappearing on time without a render loop.
        if self.status.is_some() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        let frame = egui::Frame {
            fill: self.palette.background,
            stroke: egui::Stroke::new(1.0, self.palette.border),
            corner_radius: theme::WINDOW_CORNER,
            inner_margin: CONTENT_MARGIN,
            ..Default::default()
        };

        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            self.header(ui, &ctx);
            ui.add_space(4.0);

            if let Some(message) = self.fatal.clone() {
                ui.colored_label(self.palette.danger, message);
                return;
            }

            if let Some(message) = &self.notice {
                ui.label(RichText::new(message).size(13.0).color(self.palette.danger));
                ui.add_space(4.0);
            }

            match self.view {
                View::Mixer => self.mixer(ui),
                View::Settings => self.settings(ui, &ctx),
            }

            if let Some((text, _)) = &self.status {
                ui.add_space(4.0);
                ui.label(RichText::new(text).size(13.0).color(self.palette.text_dim));
            }
        });
    }
}

impl VolumeApp {
    fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let header = ui.horizontal(|ui| {
            let title = match self.view {
                View::Mixer => "Volume11",
                View::Settings => "Settings",
            };

            // The window has no title bar of its own, so the title doubles as the
            // drag handle. `StartDrag` hands the move over to Windows, which gives
            // the usual snapping and multi-monitor behaviour for free.
            ui.label(
                RichText::new(title)
                    .size(15.5)
                    .strong()
                    .color(self.palette.text),
            );

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, Icon::Close, "Close", false, &self.palette).clicked() {
                    self.hide_window(ctx);
                }

                let settings_open = self.view == View::Settings;
                if icon_button(ui, Icon::Settings, "Settings", settings_open, &self.palette)
                    .clicked()
                {
                    self.view = if settings_open {
                        View::Mixer
                    } else {
                        View::Settings
                    };
                }

                let pinned = self.config.settings.always_on_top;
                if icon_button(ui, Icon::Pin, "Always on top", pinned, &self.palette).clicked() {
                    self.config.settings.always_on_top = !pinned;
                    self.apply_window_level(ctx);
                    self.persist_settings();
                }

                if self.view == View::Mixer {
                    if icon_button(ui, Icon::Sync, "Apply", false, &self.palette).clicked() {
                        self.sync_saved();
                    }

                    if icon_button(ui, Icon::Save, "Save", false, &self.palette).clicked() {
                        self.save_current();
                    }
                }

                // Where the buttons actually end, so the drag area can stop
                // short of them instead of swallowing their clicks.
                ui.min_rect().left()
            })
            .inner
        });

        // The whole empty part of the header bar is the drag handle, not just the
        // title text — aiming at a word to move a window is needlessly fiddly.
        // This is registered after the buttons and covers a strip that does not
        // overlap them, so their clicks are unaffected.
        let header_rect = header.response.rect;
        let drag_rect = egui::Rect::from_min_max(
            header_rect.min,
            egui::pos2(header.inner - 4.0, header_rect.max.y),
        );

        if drag_rect.width() > 0.0 {
            let handle = ui
                .interact(
                    drag_rect,
                    ui.id().with("header_drag"),
                    Sense::click_and_drag(),
                )
                .on_hover_cursor(egui::CursorIcon::Grab);

            if handle.drag_started() {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            }
        }

        if self.view == View::Mixer && !self.device_name.is_empty() {
            self.device_picker(ui);
        }
    }

    /// Current output device, unfolding into a list of the others.
    ///
    /// Picking one makes it the Windows default for every role. The engine then
    /// sees the change like any other and reports back, which is where the new
    /// device's saved levels get applied.
    fn device_picker(&mut self, ui: &mut egui::Ui) {
        let palette = self.palette;
        let switchable = self.devices.len() > 1;

        let selector = device_selector(
            ui,
            &self.device_name,
            self.device_menu_open,
            switchable,
            &palette,
        )
        .on_hover_text(&self.device_name);

        if switchable && selector.clicked() {
            self.device_menu_open = !self.device_menu_open;
        }

        if !(switchable && self.device_menu_open) {
            return;
        }

        let mut chosen: Option<String> = None;

        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(0, 2))
            .show(ui, |ui| {
                for device in &self.devices {
                    let current = device.id == self.device_id;
                    if device_option(ui, &device.name, current, &palette).clicked() && !current {
                        chosen = Some(device.id.clone());
                    }
                }
            });

        if let Some(id) = chosen {
            self.audio.send(Command::SetDefaultDevice(id));
            self.device_menu_open = false;
        }

        ui.add_space(4.0);
    }

    fn mixer(&mut self, ui: &mut egui::Ui) {
        if self.sessions.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("Nothing playing")
                        .size(14.0)
                        .color(self.palette.text_dim),
                );
            });
            return;
        }

        // Collected first so the borrow on `self.sessions` ends before commands are
        // sent; egui closures would otherwise hold it across the mutation.
        let mut changes: Vec<(String, u8, bool, bool)> = Vec::new();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for index in 0..self.sessions.len() {
                    if index > 0 {
                        ui.add_space(2.0);
                    }
                    if let Some(change) = self.session_row(ui, index) {
                        changes.push(change);
                    }
                }
            });

        for (key, volume, muted, mute_changed) in changes {
            if mute_changed {
                self.audio.send(Command::SetMute {
                    key: key.clone(),
                    muted,
                });
            } else {
                self.audio.send(Command::SetVolume { key, volume });
            }
        }
    }

    /// Returns `(key, volume, muted, mute_was_toggled)` when the row changed.
    fn session_row(&mut self, ui: &mut egui::Ui, index: usize) -> Option<(String, u8, bool, bool)> {
        let palette = self.palette;
        let ctx = ui.ctx().clone();

        // The icon is looked up before the mutable borrow of the session, because
        // the cache needs `&mut self` and the row body needs `&mut session`.
        let icon_texture = {
            let session = &self.sessions[index];
            session
                .executable
                .clone()
                .and_then(|path| self.icons.get(&ctx, &path).cloned())
        };

        // Only one number field can hold focus, so a single slot is enough state.
        let mut editing = self.editing.take();

        let session = &mut self.sessions[index];

        let is_master = session.kind == SessionKind::Master;
        let title = if is_master {
            "Master".to_string()
        } else {
            session.label.clone()
        };

        let mut result = None;

        let response = egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(8, 6))
            .corner_radius(theme::ROW_CORNER)
            .fill(if is_master {
                palette.surface
            } else {
                Color32::TRANSPARENT
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(texture) = &icon_texture {
                        let tint = if session.muted {
                            Color32::from_gray(140)
                        } else {
                            Color32::WHITE
                        };
                        ui.add(
                            egui::Image::new(texture)
                                .fit_to_exact_size(Vec2::splat(ICON_SIZE))
                                .tint(tint),
                        );
                        ui.add_space(2.0);
                    } else if !is_master {
                        // Keep the text aligned with rows that do have an icon.
                        ui.add_space(ICON_SIZE + 2.0);
                    }

                    let label =
                        ui.label(RichText::new(&title).size(14.5).color(if session.muted {
                            palette.text_dim
                        } else {
                            palette.text
                        }));

                    // Several Chromium based programs report the same display name,
                    // so the executable and process id are there to tell them apart.
                    if !is_master {
                        let executable = session
                            .executable
                            .as_deref()
                            .and_then(|path| std::path::Path::new(path).file_name())
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| session.key.clone());

                        label.on_hover_text(format!("{executable} · PID {}", session.pid));
                    }
                });

                ui.horizontal(|ui| {
                    let icon = if session.muted {
                        Icon::SpeakerMuted
                    } else {
                        Icon::Speaker
                    };
                    let tooltip = if session.muted { "Unmute" } else { "Mute" };

                    if icon_button(ui, icon, tooltip, session.muted, &palette).clicked() {
                        session.muted = !session.muted;
                        result = Some((session.key.clone(), session.volume, session.muted, true));
                    }

                    ui.add_space(2.0);

                    // The number field is laid out first from the right so the
                    // slider can claim everything that is left over.
                    let field_width = NUMBER_FIELD_WIDTH;
                    let slider_width =
                        (ui.available_width() - field_width - ui.spacing().item_spacing.x)
                            .max(60.0);

                    let slider = ui
                        .scope(|ui| {
                            ui.set_width(slider_width);
                            volume_slider(ui, &mut session.volume, &palette, !session.muted)
                        })
                        .inner;

                    if slider.changed() {
                        // Typing is abandoned as soon as the slider is used.
                        editing = None;
                        result = Some((session.key.clone(), session.volume, session.muted, false));
                    }

                    if let Some(volume) = number_field(
                        ui,
                        session.volume,
                        &session.key,
                        &mut editing,
                        &palette,
                        field_width,
                    ) && volume != session.volume
                    {
                        session.volume = volume;
                        result = Some((session.key.clone(), volume, session.muted, false));
                    }
                });
            })
            .response;

        self.editing = editing;

        // Subtle hover highlight for application rows.
        if !is_master
            && response.interact(Sense::hover()).hovered()
            && ui.is_rect_visible(response.rect)
        {
            ui.painter().rect_filled(
                response.rect,
                theme::ROW_CORNER,
                palette.surface.gamma_multiply(0.6),
            );
        }

        result
    }

    fn settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.section(ui, "Behaviour");

                let mut changed = false;

                changed |= ui
                    .checkbox(
                        &mut self.config.settings.auto_apply,
                        RichText::new("Apply automatically").size(14.0),
                    )
                    .changed();

                ui.add_space(8.0);
                self.section(ui, "New programs");

                let policy = self.config.settings.unknown_app_policy;
                ui.horizontal(|ui| {
                    if chip_button(
                        ui,
                        "Leave alone",
                        policy == UnknownAppPolicy::LeaveAlone,
                        &self.palette,
                    )
                    .clicked()
                    {
                        self.config.settings.unknown_app_policy = UnknownAppPolicy::LeaveAlone;
                        changed = true;
                    }

                    if chip_button(
                        ui,
                        "Set default",
                        policy == UnknownAppPolicy::ApplyDefault,
                        &self.palette,
                    )
                    .clicked()
                    {
                        self.config.settings.unknown_app_policy = UnknownAppPolicy::ApplyDefault;
                        changed = true;
                    }
                });

                if self.config.settings.unknown_app_policy == UnknownAppPolicy::ApplyDefault {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Default")
                                .size(13.0)
                                .color(self.palette.text_dim),
                        );

                        let palette = self.palette;

                        // The same editable box as in the mixer, so the exact
                        // value is visible and typeable rather than guessed from
                        // the slider position.
                        let slider_width = (ui.available_width()
                            - NUMBER_FIELD_WIDTH
                            - ui.spacing().item_spacing.x)
                            .max(60.0);

                        changed |= ui
                            .scope(|ui| {
                                ui.set_width(slider_width);
                                volume_slider(
                                    ui,
                                    &mut self.config.settings.default_volume,
                                    &palette,
                                    true,
                                )
                            })
                            .inner
                            .changed();

                        let mut editing = self.editing.take();
                        if let Some(volume) = number_field(
                            ui,
                            self.config.settings.default_volume,
                            DEFAULT_VOLUME_KEY,
                            &mut editing,
                            &palette,
                            NUMBER_FIELD_WIDTH,
                        ) && volume != self.config.settings.default_volume
                        {
                            self.config.settings.default_volume = volume;
                            changed = true;
                        }
                        self.editing = editing;
                    });
                }

                ui.add_space(8.0);
                self.section(ui, "Appearance");

                changed |= ui
                    .checkbox(
                        &mut self.config.settings.oled_black,
                        RichText::new("True black").size(14.0),
                    )
                    .changed();

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let current = parse_accent(&self.config.settings.accent);
                    for (name, hex) in ACCENT_CHOICES {
                        let colour = parse_accent(hex);
                        if colour_swatch(ui, colour, colour == current, &self.palette)
                            .on_hover_text(*name)
                            .clicked()
                        {
                            self.config.settings.accent = accent_to_hex(colour);
                            changed = true;
                        }
                    }
                });

                ui.add_space(8.0);
                self.section(ui, "System");

                let mut autostart_enabled = self.config.settings.start_with_windows;
                if ui
                    .checkbox(
                        &mut autostart_enabled,
                        RichText::new("Start with Windows").size(14.0),
                    )
                    .changed()
                {
                    match autostart::set(autostart_enabled) {
                        Ok(()) => {
                            self.config.settings.start_with_windows = autostart_enabled;
                            changed = true;
                        }
                        Err(error) => {
                            self.set_status(format!("Error: {error}"));
                        }
                    }
                }

                ui.add_space(10.0);

                // Saved levels belong to a device, so the list shows the current
                // device's set and names it.
                let saved = self
                    .config
                    .device(&self.device_id)
                    .map(|entry| entry.apps.clone())
                    .unwrap_or_default();

                self.section(ui, format!("Saved ({})", saved.len()));
                if !self.device_name.is_empty() {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&self.device_name)
                                .size(12.5)
                                .color(self.palette.text_dim),
                        )
                        .truncate(),
                    );
                }

                let mut remove: Option<String> = None;
                let entries: Vec<(String, String, u8, bool)> = saved
                    .iter()
                    .map(|(key, entry)| {
                        (
                            key.clone(),
                            if entry.label.is_empty() {
                                key.clone()
                            } else {
                                entry.label.clone()
                            },
                            entry.volume,
                            entry.muted,
                        )
                    })
                    .collect();

                if entries.is_empty() {
                    ui.label(
                        RichText::new("Nothing saved")
                            .size(13.0)
                            .color(self.palette.text_dim),
                    );
                }

                for (key, label, volume, muted) in entries {
                    ui.horizontal(|ui| {
                        let name = match key.as_str() {
                            MASTER_KEY => "Master".to_string(),
                            SYSTEM_KEY => "System sounds".to_string(),
                            _ => label,
                        };

                        ui.label(RichText::new(name).size(13.5).color(self.palette.text));

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icon_button(ui, Icon::Trash, "Remove", false, &self.palette)
                                .clicked()
                            {
                                remove = Some(key.clone());
                            }

                            ui.label(
                                RichText::new(if muted {
                                    "stumm".to_string()
                                } else {
                                    format!("{volume} %")
                                })
                                .size(13.0)
                                .family(egui::FontFamily::Monospace)
                                .color(self.palette.text_dim),
                            );
                        });
                    });
                }

                if let Some(key) = remove {
                    self.config.remove(&self.device_id, &key);
                    changed = true;
                }

                if changed {
                    self.refresh_palette(ctx);
                    self.persist_settings();
                }
            });
    }

    fn section(&self, ui: &mut egui::Ui, title: impl Into<String>) {
        ui.add_space(2.0);
        ui.label(
            RichText::new(title.into().to_uppercase())
                .size(11.5)
                .color(self.palette.text_dim)
                .font(FontId::proportional(11.5)),
        );
        ui.add_space(3.0);
    }
}

/// Volume11's icon for the window, taskbar and Alt+Tab.
///
/// 64 px because Windows scales that down cleanly for every place it is used.
pub fn window_icon() -> Option<egui::IconData> {
    let image = appicon::own_icon(64)?;

    Some(egui::IconData {
        width: image.width() as u32,
        height: image.height() as u32,
        rgba: image.as_raw().to_vec(),
    })
}

/// Whether Windows currently shows the application window.
///
/// Asked of Windows rather than egui, whose visibility flag describes minimised
/// and covered windows but not hidden ones.
fn window_is_shown(frame: &eframe::Frame) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;

    let Ok(handle) = frame.window_handle() else {
        return false;
    };

    match handle.as_raw() {
        RawWindowHandle::Win32(win32) => unsafe {
            IsWindowVisible(HWND(win32.hwnd.get() as *mut _)).as_bool()
        },
        _ => false,
    }
}

/// Size used for the native window.
pub fn window_size() -> Vec2 {
    Vec2::new(WINDOW_WIDTH, WINDOW_HEIGHT)
}
