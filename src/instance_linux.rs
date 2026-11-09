//! Single instance on Linux.
//!
//! The running copy listens on a Unix socket in the user's runtime directory.
//! A second start connects, asks it to show its window and exits. A socket
//! left behind by a crashed copy accepts no connection and is replaced.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use crate::tray::{self, TrayMessage};

/// Holds the listening socket for as long as this process runs.
pub struct Guard {
    path: PathBuf,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    // A test build with its own configuration must not hand over to the
    // copy the user is running, so the socket follows the override.
    let suffix = match std::env::var("VOLUME11_CONFIG") {
        Ok(path) if !path.trim().is_empty() => format!("-{:x}", fnv(path.as_bytes())),
        _ => String::new(),
    };
    dir.join(format!("volume11{suffix}.sock"))
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x100000001b3)
    })
}

/// Claim the single-instance slot. `None` means another copy already has it.
pub fn acquire() -> Option<Guard> {
    let path = socket_path();

    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(_) => {
            if UnixStream::connect(&path).is_ok() {
                return None;
            }
            // Stale socket from a copy that did not shut down cleanly.
            let _ = std::fs::remove_file(&path);
            UnixListener::bind(&path).ok()?
        }
    };

    std::thread::Builder::new()
        .name("volume11-instance".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                let _ = BufReader::new(stream).read_line(&mut line);
                match line.trim() {
                    "quit" => tray::notify(TrayMessage::Quit),
                    "settings" => tray::notify(TrayMessage::ShowSettings),
                    "show" => tray::notify(TrayMessage::Show),
                    // A bare connection is another copy checking whether
                    // this one is alive.
                    _ => {}
                }
            }
        })
        .ok()?;

    Some(Guard { path })
}

fn send(request: &str) -> bool {
    UnixStream::connect(socket_path())
        .and_then(|mut stream| stream.write_all(format!("{request}\n").as_bytes()))
        .is_ok()
}

/// Ask the running instance to open its window.
pub fn signal_existing() {
    send("show");
}

/// Ask the running instance to quit. Used by updates and the uninstaller.
pub fn signal_quit() -> bool {
    send("quit")
}
