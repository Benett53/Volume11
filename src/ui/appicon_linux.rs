//! Application icons for the mixer rows, on Linux.
//!
//! Streams name an icon (`application.icon_name`) or at least a program. That
//! name is looked up the freedesktop way: the hicolor theme in the usual data
//! directories (system, user, Flatpak), then `/usr/share/pixmaps`. If nothing
//! matches, the desktop entry of the program supplies the icon name. PNG is
//! preferred; SVG is rasterised when a program ships nothing else.
//! Lookups are cached by name, including misses.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::{ColorImage, TextureHandle, TextureOptions};

use crate::pngutil;

/// Sizes tried in order: the row draws 21 px, so 32 and 48 px scale down best.
const SIZES: &[&str] = &[
    "32x32", "48x48", "64x64", "24x24", "128x128", "256x256", "512x512",
];

/// Caches one texture per icon name.
#[derive(Default)]
pub struct IconCache {
    entries: HashMap<String, Option<TextureHandle>>,
}

impl IconCache {
    /// Texture for `name`, loading it on first use.
    pub fn get(&mut self, ctx: &egui::Context, name: &str) -> Option<&TextureHandle> {
        if !self.entries.contains_key(name) {
            let texture = find_icon(name)
                .and_then(|path| load_image(&path))
                .map(|image| {
                    ctx.load_texture(format!("appicon:{name}"), image, TextureOptions::LINEAR)
                });
            self.entries.insert(name.to_string(), texture);
        }
        self.entries.get(name)?.as_ref()
    }

    /// Drop textures for programs that are no longer playing.
    pub fn retain(&mut self, live: &[String]) {
        self.entries
            .retain(|name, _| live.iter().any(|n| n == name));
    }
}

/// Edge length SVG icons are rasterised at: twice the row size, for sharp
/// downscaling on high-DPI screens.
const SVG_SIZE: u32 = 48;

fn load_image(path: &Path) -> Option<ColorImage> {
    if path.extension().is_some_and(|ext| ext == "svg") {
        load_svg(path)
    } else {
        load_png(path)
    }
}

fn load_svg(path: &Path) -> Option<ColorImage> {
    use resvg::{tiny_skia, usvg};

    let data = std::fs::read(path).ok()?;
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let scale = SVG_SIZE as f32 / size.width().max(size.height());
    let mut pixmap = tiny_skia::Pixmap::new(SVG_SIZE, SVG_SIZE)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Some(ColorImage::from_rgba_premultiplied(
        [SVG_SIZE as usize, SVG_SIZE as usize],
        pixmap.data(),
    ))
}

fn load_png(path: &Path) -> Option<ColorImage> {
    let file = std::fs::File::open(path).ok()?;
    let image = pngutil::decode(std::io::BufReader::new(file))?;
    Some(ColorImage::from_rgba_unmultiplied(
        [image.width as usize, image.height as usize],
        &image.pixels,
    ))
}

/// Volume11's own icon, compiled into the binary.
pub fn own_icon(_size: i32) -> Option<ColorImage> {
    let image = pngutil::decode(pngutil::OWN_ICON_64)?;
    Some(ColorImage::from_rgba_unmultiplied(
        [image.width as usize, image.height as usize],
        &image.pixels,
    ))
}

/// `$XDG_DATA_HOME` and `$XDG_DATA_DIRS`, plus the Flatpak export folders,
/// which are not always listed.
fn data_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) => dirs.push(PathBuf::from(dir)),
        None => dirs.extend(home.as_ref().map(|h| h.join(".local/share"))),
    }
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').map(PathBuf::from));
    dirs.extend(home.map(|h| h.join(".local/share/flatpak/exports/share")));
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    dirs.dedup();
    dirs
}

fn find_icon(name: &str) -> Option<PathBuf> {
    if name.starts_with('/') {
        return Path::new(name).is_file().then(|| PathBuf::from(name));
    }
    let dirs = data_dirs();
    icon_file(&dirs, name)
        .or_else(|| icon_file(&dirs, &name.to_lowercase()))
        .or_else(|| {
            let icon = desktop_icon(&dirs, name)?;
            if icon.starts_with('/') {
                Path::new(&icon).is_file().then(|| PathBuf::from(&icon))
            } else {
                icon_file(&dirs, &icon)
            }
        })
}

fn icon_file(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    let png = &format!("{name}.png");
    let svg = &format!("{name}.svg");
    let hicolor = |dir: &PathBuf, size: &str, file: &str| {
        dir.join("icons/hicolor").join(size).join("apps").join(file)
    };

    dirs.iter()
        .flat_map(|dir| SIZES.iter().map(move |size| hicolor(dir, size, png)))
        .chain(dirs.iter().map(|dir| dir.join("pixmaps").join(png)))
        .chain(dirs.iter().map(|dir| hicolor(dir, "scalable", svg)))
        .chain(dirs.iter().map(|dir| dir.join("pixmaps").join(svg)))
        .find(|path| path.is_file())
}

/// The `Icon=` of the desktop entry belonging to `program`: one named after
/// it, or one whose `Exec` or `StartupWMClass` is the program.
fn desktop_icon(dirs: &[PathBuf], program: &str) -> Option<String> {
    let program = program.to_lowercase();
    let program = program.strip_suffix(".exe").unwrap_or(&program);
    let mut fallback = None;

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir.join("applications")) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "desktop") {
                continue;
            }
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(icon) = desktop_value(&text, "Icon") else {
                continue;
            };
            // `spotify.desktop`, `com.spotify.Client.desktop`
            if stem == program || stem.rsplit('.').next() == Some(program) {
                return Some(icon);
            }
            let exec = desktop_value(&text, "Exec").unwrap_or_default();
            let exec_name = exec
                .split_whitespace()
                .next()
                .and_then(|cmd| cmd.rsplit('/').next())
                .unwrap_or_default()
                .to_lowercase();
            let wm_class = desktop_value(&text, "StartupWMClass")
                .unwrap_or_default()
                .to_lowercase();
            if fallback.is_none() && (exec_name == program || wm_class == program) {
                fallback = Some(icon);
            }
        }
    }
    fallback
}

/// A key from the `[Desktop Entry]` group.
fn desktop_value(text: &str, key: &str) -> Option<String> {
    let mut in_main = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if in_main
            && let Some(value) = line
                .strip_prefix(key)
                .and_then(|r| r.trim_start().strip_prefix('='))
        {
            let value = value.trim();
            return (!value.is_empty()).then(|| value.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_values_come_from_the_main_group() {
        let text = "[Desktop Entry]\nName=Foo\nIcon = foo\n[Desktop Action new]\nIcon=bar\n";
        assert_eq!(desktop_value(text, "Icon").as_deref(), Some("foo"));
        assert_eq!(desktop_value(text, "Exec"), None);
    }

    #[test]
    fn svg_icons_are_rasterised() {
        let dir = std::env::temp_dir().join(format!("volume11-svg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.svg");
        std::fs::write(
            &path,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16" fill="red"/></svg>"#,
        )
        .unwrap();
        let image = load_image(&path).expect("svg loads");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(image.size, [SVG_SIZE as usize, SVG_SIZE as usize]);
        assert_eq!(image.pixels[0], egui::Color32::from_rgb(255, 0, 0));
    }

    #[test]
    fn own_icon_is_available() {
        assert!(own_icon(64).is_some());
    }
}
