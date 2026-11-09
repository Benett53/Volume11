//! Make the first active playback device whose name contains the argument the
//! Windows default. A development helper for testing device switching.
//!
//! cargo run --example set_device -- "Headset"

#[cfg(windows)]
mod imp {
    use volume11::audio::set_default_device;
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::{
        DEVICE_STATE_ACTIVE, IMMDeviceEnumerator, MMDeviceEnumerator, eRender,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        STGM_READ,
    };

    pub fn main() {
        let wanted = std::env::args()
            .nth(1)
            .expect("usage: set_device <name fragment>");

        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).expect("device enumerator");
            let devices = enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
                .expect("device list");

            for index in 0..devices.GetCount().unwrap_or(0) {
                let Ok(device) = devices.Item(index) else {
                    continue;
                };
                let Ok(store) = device.OpenPropertyStore(STGM_READ) else {
                    continue;
                };
                let name = store
                    .GetValue(&PKEY_Device_FriendlyName)
                    .map(|value| value.to_string())
                    .unwrap_or_default();

                if name.contains(&wanted) {
                    let raw = device.GetId().expect("device id");
                    let id = raw.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(raw.as_ptr() as *const _));

                    match set_default_device(&id) {
                        Ok(()) => println!("default is now: {name}"),
                        Err(error) => println!("failed: {error}"),
                    }
                    return;
                }
            }

            println!("no active device matches {wanted:?}");
        }
    }
}

fn main() {
    #[cfg(windows)]
    imp::main();
    #[cfg(not(windows))]
    eprintln!("Windows only. On Linux: pactl set-default-sink <name>");
}
