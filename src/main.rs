//! Volume11 — per-application volume manager for Windows.
//!
//! Lives in the notification area. The window is created hidden and shown on
//! demand, so starting with Windows costs a window that is never drawn.

// No console window in release builds. Debug builds keep it so panics and the
// logging from the audio thread stay visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use volume11::config::Config;
use volume11::{audio, autostart, config, instance, tray, ui, update};

/// Renderer configuration, tuned for starting reliably rather than for raw speed.
///
/// Windows ships no OpenGL beyond version 1.1; anything higher comes from the GPU
/// vendor's driver. A machine running on the Microsoft Basic Display Adapter — a
/// fresh install, a VM, some remote sessions — therefore has no usable OpenGL at
/// all, which is why the GL renderer was dropped.
///
/// Direct3D 12 is a first-class Windows API with a guaranteed software fallback
/// (WARP), and on a normal desktop it is also the faster path, because D3D12
/// drivers get far more attention on Windows than OpenGL ones do.
///
/// Vulkan was enabled alongside it at first and has been dropped again: measured
/// against a build with D3D12 alone it cost 44 MB of working set, 123 handles and
/// 9 threads at startup for a backend Windows never picks when D3D12 is present.
fn renderer_options() -> egui_wgpu::WgpuConfiguration {
    use egui_wgpu::wgpu;

    let mut configuration = egui_wgpu::WgpuConfiguration {
        // A volume mixer does no continuous GPU work, so latency matters more
        // than throughput: fewer queued frames means the slider tracks the mouse.
        surface: egui_wgpu::SurfaceConfig::LOW_LATENCY,
        ..Default::default()
    };

    if let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut configuration.wgpu_setup {
        setup.instance_descriptor.backends = wgpu::Backends::DX12;
        // An integrated GPU is plenty for a few hundred triangles and avoids
        // waking a discrete card, which on laptops costs battery for nothing.
        setup.power_preference = wgpu::PowerPreference::LowPower;
    }

    configuration
}

fn main() -> eframe::Result<()> {
    // `--show` opens the mixer straight away instead of starting minimised.
    // Useful for a desktop shortcut, and it is how the window gets tested.
    let show_on_start = std::env::args().any(|argument| argument == "--show");

    // Started by an update: the previous copy is still shutting down and holds
    // the single-instance slot for a moment longer.
    let updated = std::env::args().any(|argument| argument == "--updated");

    // Only one copy may run: two would both sit in the tray and both write the
    // configuration file. A second start hands over to the first and stops.
    let mut instance = instance::acquire();
    for _ in 0..50 {
        if instance.is_some() || !updated {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        instance = instance::acquire();
    }
    let Some(_instance) = instance else {
        instance::signal_existing();
        return Ok(());
    };

    let config_path = config::config_path();
    let mut loaded = Config::load(&config_path);

    // An autostart entry written by a copy that has since moved would start a
    // file that no longer exists. Repoint it before reading its state.
    autostart::repair();

    // The registry is the source of truth for autostart: the user may have removed
    // the entry through Task Manager since the last run.
    loaded.settings.start_with_windows = autostart::is_enabled();

    let size = ui::window_size();

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(size)
        // eframe shows the window unconditionally after its first rendered
        // frame, to avoid a white flash on startup — `with_visible(false)` does
        // not survive that. A tray application must not pop up when Windows
        // starts it, so the window begins far off-screen, where that forced
        // first show cannot be seen. `ui::VolumeApp` hides it again on the next
        // pass and positions it properly before it is ever shown for real.
        .with_position(ui::OFFSCREEN)
        .with_min_inner_size(size)
        .with_decorations(false)
        .with_transparent(true)
        .with_resizable(false)
        .with_visible(false)
        .with_title("Volume11");

    if let Some(icon) = ui::window_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        // Reactive mode: no frames are produced while nothing happens.
        run_and_return: false,
        wgpu_options: renderer_options(),
        ..Default::default()
    };

    eframe::run_native(
        "Volume11",
        options,
        Box::new(move |cc| {
            let ctx_for_audio = cc.egui_ctx.clone();
            let ctx_for_tray = cc.egui_ctx.clone();

            let audio = audio::spawn(move || ctx_for_audio.request_repaint());
            let tray = tray::spawn(move || ctx_for_tray.request_repaint());

            let ctx_for_update = cc.egui_ctx.clone();
            let updater = update::Updater::new(
                loaded.settings.check_updates,
                loaded.settings.last_update_check,
                move || ctx_for_update.request_repaint(),
            );

            let mut app = ui::VolumeApp::new(cc, audio, tray, updater, loaded, config_path);

            if show_on_start || updated {
                app.request_show();
            }

            Ok(Box::new(app))
        }),
    )
}
