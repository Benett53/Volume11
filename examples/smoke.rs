//! Manual smoke test for the audio layer: prints sessions and reacts to changes.
//! Run with: cargo run --example smoke
use std::path::Path;

use volume11::audio;

fn main() {
    let handle = audio::spawn(|| {});
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);

    while std::time::Instant::now() < deadline {
        match handle
            .events
            .recv_timeout(std::time::Duration::from_millis(500))
        {
            Ok(audio::Event::Sessions(list)) => {
                println!("--- {} sessions ---", list.len());
                for s in &list {
                    let exe = s
                        .executable
                        .as_deref()
                        .and_then(|p| Path::new(p).file_name())
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    println!(
                        "  [{:?}] key={:<24} vol={:>3} mute={:<5} pid={:<6} label={:?} exe={}",
                        s.kind, s.key, s.volume, s.muted, s.pid, s.label, exe
                    );
                }
            }
            Ok(audio::Event::Changed { key, volume, muted }) => {
                println!("  ~ external change: {key} -> {volume} muted={muted}");
            }
            Ok(audio::Event::DeviceChanged { name, .. }) => println!("  ~ device now: {name}"),
            Ok(audio::Event::Devices { list, current }) => {
                println!("--- {} playback devices ---", list.len());
                for d in &list {
                    let mark = if d.id == current { "*" } else { " " };
                    println!("  {mark} {}  [{}]", d.name, d.id);
                }
            }
            Ok(audio::Event::SessionAdded { key, .. }) => println!("  ~ new session: {key}"),
            Ok(audio::Event::Problem(msg)) => println!("  ! {msg}"),
            Ok(audio::Event::Fatal(msg)) => {
                println!("FATAL: {msg}");
                return;
            }
            Err(_) => {}
        }
    }
    println!("done");
}
