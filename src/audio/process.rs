//! Resolving a process id to a stable identity (executable name) and a display label.
//!
//! The old WPF version keyed its saved configuration on the WASAPI *display name*.
//! That string is frequently empty, sometimes localized, and sometimes an
//! unexpanded resource reference such as `@%SystemRoot%\System32\AudioSrv.Dll,-202`.
//! Keying on the executable file name instead gives a stable identity that survives
//! restarts, updates and locale changes.

#[cfg(windows)]
use std::path::Path;

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HANDLE, MAX_PATH};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

#[cfg(windows)]
/// Full path of the executable backing `pid`, if it can be opened.
///
/// Requires no elevation: `PROCESS_QUERY_LIMITED_INFORMATION` is enough for
/// processes of the same user, which is all an audio session can belong to.
pub fn executable_path(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }

    unsafe {
        let handle: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;

        let mut buffer = [0u16; MAX_PATH as usize];
        let mut len = buffer.len() as u32;

        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut len,
        );

        let _ = CloseHandle(handle);

        result.ok()?;

        Some(String::from_utf16_lossy(&buffer[..len as usize]))
    }
}

#[cfg(windows)]
/// Lower-cased file name of the executable, e.g. `firefox.exe`.
///
/// This is the key used in the configuration file, so it must stay stable.
pub fn executable_name(pid: u32) -> Option<String> {
    let path = executable_path(pid)?;
    Path::new(&path)
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
}

/// A human friendly label derived from the executable name: `firefox.exe` -> `Firefox`.
///
/// Used only when the session reports no usable display name of its own.
pub fn friendly_label(executable: &str) -> String {
    let stem = executable
        .strip_suffix(".exe")
        .unwrap_or(executable)
        .replace(['_', '-'], " ");

    let mut label = String::with_capacity(stem.len());
    let mut capitalize_next = true;

    for ch in stem.chars() {
        if ch == ' ' {
            capitalize_next = true;
            label.push(ch);
        } else if capitalize_next {
            label.extend(ch.to_uppercase());
            capitalize_next = false;
        } else {
            label.push(ch);
        }
    }

    label
}

/// WASAPI display names are unusable in several well known shapes. Filter those out
/// so the caller can fall back to the executable name.
pub fn usable_display_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();

    if trimmed.is_empty() {
        return None;
    }

    // Unexpanded indirect string, e.g. `@%SystemRoot%\System32\AudioSrv.Dll,-202`.
    if trimmed.starts_with('@') {
        return None;
    }

    Some(trimmed.to_string())
}

#[cfg(windows)]
/// The `FileDescription` recorded in an executable's version resource.
///
/// This is what Task Manager and Explorer show, so it reads far better than a
/// capitalised file name: `RobloxStudioBeta.exe` becomes `Roblox Studio`.
/// Returns `None` for executables without a version resource.
pub fn file_description(executable_path: &str) -> Option<String> {
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    use windows::core::{HSTRING, PCWSTR};

    let wide = HSTRING::from(executable_path);

    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None);
        if size == 0 {
            return None;
        }

        let mut buffer = vec![0u8; size as usize];
        GetFileVersionInfoW(
            PCWSTR(wide.as_ptr()),
            None,
            size,
            buffer.as_mut_ptr() as *mut _,
        )
        .ok()?;

        // The description is stored per language/codepage, so the translation table
        // has to be read first rather than guessing the usual 040904b0.
        let mut translations: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut translations_len: u32 = 0;
        let translation_query = HSTRING::from(r"\VarFileInfo\Translation");

        if !VerQueryValueW(
            buffer.as_ptr() as *const _,
            PCWSTR(translation_query.as_ptr()),
            &mut translations,
            &mut translations_len,
        )
        .as_bool()
            || translations_len < 4
        {
            return None;
        }

        // Each entry is a u16 language id followed by a u16 code page.
        let language = *(translations as *const u16);
        let codepage = *(translations as *const u16).add(1);

        let query = HSTRING::from(format!(
            r"\StringFileInfo\{language:04x}{codepage:04x}\FileDescription"
        ));

        let mut value: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut value_len: u32 = 0;

        if !VerQueryValueW(
            buffer.as_ptr() as *const _,
            PCWSTR(query.as_ptr()),
            &mut value,
            &mut value_len,
        )
        .as_bool()
            || value_len == 0
        {
            return None;
        }

        // `value_len` is documented to count characters including the NUL, but
        // not every executable's resource agrees: Spotify's reports a length
        // that runs past its description into the next entry, which showed up
        // as "Spotify8?FileV". The string ends at the first NUL, whatever the
        // length claims.
        let chars = std::slice::from_raw_parts(value as *const u16, value_len as usize);
        Some(clean_description(chars)).filter(|text| !text.is_empty())
    }
}

/// A UTF-16 resource string cut at its first NUL, with surrounding space removed.
#[cfg(windows)]
fn clean_description(chars: &[u16]) -> String {
    let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..end]).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn description_stops_at_the_first_nul() {
        // What Spotify's version resource hands back: the name, a terminator,
        // then the start of the next entry.
        let raw: Vec<u16> = "Spotify\0\u{8}\u{1}FileVersion".encode_utf16().collect();
        assert_eq!(clean_description(&raw), "Spotify");

        let padded: Vec<u16> = " Firefox \0".encode_utf16().collect();
        assert_eq!(clean_description(&padded), "Firefox");

        let unterminated: Vec<u16> = "Steam".encode_utf16().collect();
        assert_eq!(clean_description(&unterminated), "Steam");
    }

    #[test]
    fn friendly_label_strips_extension_and_capitalizes() {
        assert_eq!(friendly_label("firefox.exe"), "Firefox");
        assert_eq!(friendly_label("vlc.exe"), "Vlc");
        assert_eq!(friendly_label("some_app-name.exe"), "Some App Name");
    }

    #[test]
    fn indirect_strings_are_rejected() {
        assert_eq!(
            usable_display_name(r"@%SystemRoot%\System32\AudioSrv.Dll,-202"),
            None
        );
        assert_eq!(usable_display_name("   "), None);
        assert_eq!(
            usable_display_name(" Firefox "),
            Some("Firefox".to_string())
        );
    }

    #[test]
    #[cfg(windows)]
    fn current_process_resolves_to_an_executable() {
        let pid = std::process::id();
        let name = executable_name(pid).expect("own process must resolve");
        assert!(name.ends_with(".exe"), "unexpected name: {name}");
    }
}

#[cfg(all(test, windows))]
mod live {
    use super::*;

    /// Prints the description read from a real executable. Ignored by default;
    /// run with `VOLUME11_DESCRIBE=<path> cargo test describe -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn describe() {
        let path = std::env::var("VOLUME11_DESCRIBE").expect("set VOLUME11_DESCRIBE");
        println!("description: {:?}", file_description(&path));
    }
}
