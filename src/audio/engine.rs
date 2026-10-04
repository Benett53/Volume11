//! The audio engine thread.
//!
//! Owns every COM object and the session cache. Nothing here is shared with the UI
//! except two channels, which keeps all the `!Send` COM pointers on one thread.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, select, unbounded};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::Audio::Endpoints::{IAudioEndpointVolume, IAudioEndpointVolumeCallback};
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IAudioSessionControl, IAudioSessionControl2, IAudioSessionEvents,
    IAudioSessionManager2, IAudioSessionNotification, IMMDevice, IMMDeviceEnumerator,
    IMMNotificationClient, ISimpleAudioVolume, MMDeviceEnumerator, eMultimedia, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, STGM_READ,
};
use windows::core::{GUID, Interface};

use super::callbacks::{
    DeviceNotification, EndpointVolumeCallback, Notification, SessionEvents, SessionNotification,
};
use super::policy::set_default_device;
use super::process::{
    executable_name, executable_path, file_description, friendly_label, usable_display_name,
};
use super::{
    Command, DeviceInfo, Event, MASTER_KEY, SYSTEM_KEY, SessionKind, SessionSnapshot,
    percent_to_scalar, scalar_to_percent,
};

/// Passed as the event context on every write this app makes, so the resulting
/// change notification can be recognised as our own echo and ignored. Without this
/// the UI would fight the user: our write triggers a callback which would push the
/// value straight back into the slider being dragged.
const APP_EVENT_CONTEXT: GUID = GUID::from_u128(0x7b1d4c2e_9f83_4a17_b5d6_2e8c41f09a63);

/// Handle held by the UI thread.
pub struct AudioHandle {
    pub commands: Sender<Command>,
    pub events: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

impl AudioHandle {
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

impl Drop for AudioHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Start the audio thread. `wake` is called after every event so the UI can repaint
/// even when it is otherwise idle (the UI runs in reactive mode, not a render loop).
pub fn spawn<F>(wake: F) -> AudioHandle
where
    F: Fn() + Send + 'static,
{
    let (command_tx, command_rx) = unbounded::<Command>();
    let (event_tx, event_rx) = unbounded::<Event>();

    let thread_events = event_tx.clone();

    let thread = std::thread::Builder::new()
        .name("volume11-audio".into())
        .spawn(move || {
            // Multithreaded apartment: WASAPI session notifications are delivered on
            // RPC worker threads and need no message pump here.
            let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if com.is_err() {
                let _ = thread_events.send(Event::Fatal(format!("COM: {com:?}")));
                wake();
                return;
            }

            // With no playback device at all, for example a Bluetooth headset
            // that connects after login, there is nothing to attach to yet.
            // Giving up here used to leave the application without audio
            // control until it was restarted, so it waits for one instead.
            // This is the only polling in the program and only runs while no
            // device exists.
            let mut reported = false;
            let engine = loop {
                match Engine::new(thread_events.clone()) {
                    Ok(engine) => break Some(engine),
                    Err(error) => {
                        if !reported {
                            let _ = thread_events
                                .send(Event::Problem(format!("No playback device: {error}")));
                            wake();
                            reported = true;
                        }
                        match command_rx.recv_timeout(Duration::from_secs(2)) {
                            Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                                break None;
                            }
                            _ => {}
                        }
                    }
                }
            };

            if let Some(mut engine) = engine {
                engine.run(&command_rx, &wake);
            }

            unsafe { CoUninitialize() };
        })
        .expect("audio thread must start");

    AudioHandle {
        commands: command_tx,
        events: event_rx,
        thread: Some(thread),
    }
}

/// One cached session. Holding `volume` is what makes slider drags cheap.
struct Entry {
    pid: u32,
    key: String,
    label: String,
    kind: SessionKind,
    executable: Option<String>,
    control: IAudioSessionControl,
    volume: ISimpleAudioVolume,
    /// Kept so it can be unregistered again; dropping it without unregistering
    /// would leak the callback for the lifetime of the session.
    events: Option<IAudioSessionEvents>,
}

impl Entry {
    fn level(&self) -> Option<(f32, bool)> {
        unsafe {
            Some((
                self.volume.GetMasterVolume().ok()?,
                self.volume.GetMute().ok()?.as_bool(),
            ))
        }
    }

    fn snapshot(&self) -> Option<SessionSnapshot> {
        let pid = self.pid;
        let scalar = unsafe { self.volume.GetMasterVolume().ok()? };
        let muted = unsafe { self.volume.GetMute().ok()?.as_bool() };

        Some(SessionSnapshot {
            key: self.key.clone(),
            label: self.label.clone(),
            pid,
            volume: scalar_to_percent(scalar),
            muted,
            kind: self.kind,
            executable: self.executable.clone(),
        })
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        if let Some(events) = self.events.take() {
            unsafe {
                let _ = self.control.UnregisterAudioSessionNotification(&events);
            }
        }
    }
}

struct Engine {
    events: Sender<Event>,
    notify_tx: Sender<Notification>,
    notify_rx: Receiver<Notification>,

    enumerator: IMMDeviceEnumerator,
    device_callback: IMMNotificationClient,

    /// Everything below is rebuilt when the default device changes.
    device: IMMDevice,
    endpoint: IAudioEndpointVolume,
    endpoint_callback: IAudioEndpointVolumeCallback,
    manager: IAudioSessionManager2,
    session_callback: IAudioSessionNotification,
    /// Keyed by session instance identifier, not process id: one process can
    /// own several sessions, and several processes can belong to one program.
    sessions: HashMap<String, Entry>,
}

impl Engine {
    fn new(events: Sender<Event>) -> windows::core::Result<Self> {
        let (notify_tx, notify_rx) = unbounded::<Notification>();

        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };

        let device_callback: IMMNotificationClient =
            DeviceNotification::new(notify_tx.clone()).into();
        unsafe {
            enumerator.RegisterEndpointNotificationCallback(&device_callback)?;
        }

        let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia)? };
        let (endpoint, endpoint_callback, manager, session_callback) =
            Self::activate(&device, &notify_tx)?;

        let mut engine = Self {
            events,
            notify_tx,
            notify_rx,
            enumerator,
            device_callback,
            device,
            endpoint,
            endpoint_callback,
            manager,
            session_callback,
            sessions: HashMap::new(),
        };

        let _ = engine.rebuild_sessions();

        Ok(engine)
    }

    /// Activate the per device interfaces and register their callbacks.
    fn activate(
        device: &IMMDevice,
        notify_tx: &Sender<Notification>,
    ) -> windows::core::Result<(
        IAudioEndpointVolume,
        IAudioEndpointVolumeCallback,
        IAudioSessionManager2,
        IAudioSessionNotification,
    )> {
        unsafe {
            let endpoint: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
            let endpoint_callback: IAudioEndpointVolumeCallback =
                EndpointVolumeCallback::new(notify_tx.clone()).into();
            endpoint.RegisterControlChangeNotify(&endpoint_callback)?;

            let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;

            // Enumerating once before registering is required: without it Windows
            // does not deliver session-created notifications for this manager.
            let _ = manager.GetSessionEnumerator()?;

            let session_callback: IAudioSessionNotification =
                SessionNotification::new(notify_tx.clone()).into();
            manager.RegisterSessionNotification(&session_callback)?;

            Ok((endpoint, endpoint_callback, manager, session_callback))
        }
    }

    fn run<F: Fn()>(&mut self, commands: &Receiver<Command>, wake: &F) {
        self.publish_devices(wake);
        self.publish_sessions(wake);

        loop {
            select! {
                recv(commands) -> message => match message {
                    Ok(Command::Shutdown) | Err(_) => break,
                    Ok(command) => self.handle_command(command, wake),
                },
                recv(self.notify_rx) -> message => match message {
                    Ok(notification) => self.handle_notification(notification, wake),
                    Err(_) => break,
                },
            }
        }

        self.teardown();
    }

    fn handle_command<F: Fn()>(&mut self, command: Command, wake: &F) {
        match command {
            Command::SetVolume { key, volume } => self.write_volume(&key, Some(volume), None),
            Command::SetMute { key, muted } => self.write_volume(&key, None, Some(muted)),
            Command::ApplyAll(entries) => {
                for (key, volume, muted) in entries {
                    self.write_volume(&key, Some(volume), Some(muted));
                }
                self.publish_sessions(wake);
            }
            Command::Refresh => {
                let _ = self.rebuild_sessions();
                self.publish_sessions(wake);
            }
            Command::SetDefaultDevice(id) => {
                // Success needs no reply here: Windows answers with
                // OnDefaultDeviceChanged, which runs the same switch as a
                // change made anywhere else.
                if let Err(error) = set_default_device(&id) {
                    let _ = self
                        .events
                        .send(Event::Problem(format!("Could not switch device: {error}")));
                    wake();
                }
            }
            Command::Shutdown => {}
        }
    }

    /// The hot path. One hash lookup and one COM call — no re-enumeration.
    fn write_volume(&mut self, key: &str, volume: Option<u8>, muted: Option<bool>) {
        if key == MASTER_KEY {
            unsafe {
                if let Some(volume) = volume {
                    let _ = self
                        .endpoint
                        .SetMasterVolumeLevelScalar(percent_to_scalar(volume), &APP_EVENT_CONTEXT);
                }
                if let Some(muted) = muted {
                    let _ = self.endpoint.SetMute(muted, &APP_EVENT_CONTEXT);
                }
            }
            return;
        }

        // A program's level applies to all of its sessions. Writing only the
        // first match left the others untouched, and with hash map ordering
        // "first" was not even the session the row showed.
        for entry in self.sessions.values().filter(|entry| entry.key == key) {
            unsafe {
                if let Some(volume) = volume {
                    let _ = entry
                        .volume
                        .SetMasterVolume(percent_to_scalar(volume), &APP_EVENT_CONTEXT);
                }
                if let Some(muted) = muted {
                    let _ = entry.volume.SetMute(muted, &APP_EVENT_CONTEXT);
                }
            }
        }
    }

    fn handle_notification<F: Fn()>(&mut self, notification: Notification, wake: &F) {
        match notification {
            Notification::SessionCreated => {
                let added = self.rebuild_sessions();
                self.publish_sessions(wake);

                // Only genuinely new sessions are announced. Reporting every session
                // here would let a single new one trigger a re-apply across the
                // board, undoing whatever the user had just adjusted by hand.
                for (key, label) in added {
                    let _ = self.events.send(Event::SessionAdded { key, label });
                }
                wake();
            }
            Notification::SessionStateChanged { session, expired } => {
                if expired && self.sessions.remove(&session).is_some() {
                    self.publish_sessions(wake);
                }
            }
            Notification::SessionVolume {
                session,
                scalar,
                muted,
                context,
            } => {
                if context == APP_EVENT_CONTEXT {
                    return; // our own write coming back
                }
                if let Some(entry) = self.sessions.get(&session) {
                    let _ = self.events.send(Event::Changed {
                        key: entry.key.clone(),
                        volume: scalar_to_percent(scalar),
                        muted,
                    });
                    wake();
                }
            }
            Notification::MasterVolume {
                scalar,
                muted,
                context,
            } => {
                if context == APP_EVENT_CONTEXT {
                    return;
                }
                let _ = self.events.send(Event::Changed {
                    key: MASTER_KEY.to_string(),
                    volume: scalar_to_percent(scalar),
                    muted,
                });
                wake();
            }
            Notification::DefaultDeviceChanged => {
                self.switch_device(wake);
                self.publish_devices(wake);
            }
            Notification::DevicesChanged => {
                self.publish_devices(wake);
            }
        }
    }

    /// Rebuild everything bound to the old endpoint. This is the fix for the bug
    /// where the app kept controlling the previous device after plugging in headphones.
    fn switch_device<F: Fn()>(&mut self, wake: &F) {
        self.release_device();

        let device = match unsafe {
            self.enumerator
                .GetDefaultAudioEndpoint(eRender, eMultimedia)
        } {
            Ok(device) => device,
            Err(error) => {
                let _ = self
                    .events
                    .send(Event::Problem(format!("No playback device: {error}")));
                wake();
                return;
            }
        };

        match Self::activate(&device, &self.notify_tx) {
            Ok((endpoint, endpoint_callback, manager, session_callback)) => {
                self.device = device;
                self.endpoint = endpoint;
                self.endpoint_callback = endpoint_callback;
                self.manager = manager;
                self.session_callback = session_callback;

                let _ = self.rebuild_sessions();

                let name = self.device_name();
                let id = device_id(&self.device).unwrap_or_default();
                let _ = self.events.send(Event::DeviceChanged { id, name });
                self.publish_sessions(wake);
            }
            Err(error) => {
                let _ = self
                    .events
                    .send(Event::Problem(format!("Device change failed: {error}")));
                wake();
            }
        }
    }

    fn device_name(&self) -> String {
        friendly_name(&self.device)
    }

    /// Every active playback device, plus the id of the current default.
    fn publish_devices<F: Fn()>(&self, wake: &F) {
        let mut list = Vec::new();

        unsafe {
            if let Ok(collection) = self
                .enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
                && let Ok(count) = collection.GetCount()
            {
                for index in 0..count {
                    let Ok(device) = collection.Item(index) else {
                        continue;
                    };
                    let Some(id) = device_id(&device) else {
                        continue;
                    };
                    list.push(DeviceInfo {
                        id,
                        name: friendly_name(&device),
                    });
                }
            }
        }

        list.sort_by_key(|device| device.name.to_lowercase());

        let current = device_id(&self.device).unwrap_or_default();
        let _ = self.events.send(Event::Devices { list, current });
        wake();
    }

    /// Full re-enumeration. Only runs on structural changes, never while dragging.
    ///
    /// Returns the `(key, label)` of every program that was not playing before.
    /// A new session of a program that already had one is brought to that
    /// program's current level instead, so opening a second stream does not
    /// reset a level the user has just adjusted.
    fn rebuild_sessions(&mut self) -> Vec<(String, String)> {
        let mut added: Vec<(String, String)> = Vec::new();
        let known_keys: HashSet<String> = self
            .sessions
            .values()
            .map(|entry| entry.key.clone())
            .collect();

        let Ok(enumerator) = (unsafe { self.manager.GetSessionEnumerator() }) else {
            return added;
        };
        let Ok(count) = (unsafe { enumerator.GetCount() }) else {
            return added;
        };

        let mut seen: HashSet<String> = HashSet::with_capacity(count as usize);

        for index in 0..count {
            let Ok(control) = (unsafe { enumerator.GetSession(index) }) else {
                continue;
            };
            let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
                continue;
            };
            let Ok(pid) = (unsafe { control2.GetProcessId() }) else {
                continue;
            };

            // Returns S_OK for the system sounds session and S_FALSE otherwise.
            // Both are success codes, so `is_ok()` would classify every session
            // as system sounds; only S_OK counts.
            let is_system = unsafe { control2.IsSystemSoundsSession() } == S_OK;

            // Never show our own process; muting Volume11 from inside Volume11 is
            // confusing and it produces no audio anyway.
            if !is_system && pid == std::process::id() {
                continue;
            }

            // Expired sessions linger in the enumerator; skip them.
            if let Ok(state) = unsafe { control.GetState() }
                && state.0 == 2
            {
                continue;
            }

            let Some(instance) = (unsafe { control2.GetSessionInstanceIdentifier() })
                .ok()
                .and_then(take_pwstr)
            else {
                continue;
            };

            seen.insert(instance.clone());

            if self.sessions.contains_key(&instance) {
                continue;
            }

            let Ok(volume) = control.cast::<ISimpleAudioVolume>() else {
                continue;
            };

            let executable = executable_path(pid);
            let key = if is_system {
                SYSTEM_KEY.to_string()
            } else {
                match executable_name(pid) {
                    Some(name) => name,
                    None => continue,
                }
            };

            let label = if is_system {
                "System sounds".to_string()
            } else {
                // Preference order: the session's own display name, then the
                // executable's FileDescription (what Task Manager shows), then a
                // tidied up file name.
                unsafe { control.GetDisplayName() }
                    .ok()
                    .and_then(take_pwstr)
                    .and_then(|raw| usable_display_name(&raw))
                    .or_else(|| {
                        executable
                            .as_deref()
                            .and_then(file_description)
                            .and_then(|raw| usable_display_name(&raw))
                    })
                    .unwrap_or_else(|| friendly_label(&key))
            };

            let kind = if is_system {
                SessionKind::System
            } else {
                SessionKind::App
            };

            if known_keys.contains(&key) {
                // Another session of a program that is already playing: match
                // the level its existing sessions have.
                let level = self
                    .sessions
                    .values()
                    .filter(|entry| entry.key == key)
                    .min_by_key(|entry| entry.pid)
                    .and_then(Entry::level);

                if let Some((scalar, muted)) = level {
                    unsafe {
                        let _ = volume.SetMasterVolume(scalar, &APP_EVENT_CONTEXT);
                        let _ = volume.SetMute(muted, &APP_EVENT_CONTEXT);
                    }
                }
            } else if kind == SessionKind::App && !added.iter().any(|(k, _)| *k == key) {
                added.push((key.clone(), label.clone()));
            }

            let session_events: IAudioSessionEvents =
                SessionEvents::new(self.notify_tx.clone(), instance.clone()).into();
            let registered = unsafe { control.RegisterAudioSessionNotification(&session_events) };

            self.sessions.insert(
                instance,
                Entry {
                    pid,
                    key,
                    label,
                    kind,
                    executable,
                    control,
                    volume,
                    events: registered.is_ok().then_some(session_events),
                },
            );
        }

        self.sessions.retain(|instance, _| seen.contains(instance));

        added
    }

    fn publish_sessions<F: Fn()>(&self, wake: &F) {
        let mut snapshots = Vec::with_capacity(self.sessions.len() + 1);

        let master_volume = unsafe { self.endpoint.GetMasterVolumeLevelScalar() }.unwrap_or(0.0);
        let master_muted = unsafe { self.endpoint.GetMute() }
            .map(|value| value.as_bool())
            .unwrap_or(false);

        snapshots.push(SessionSnapshot {
            key: MASTER_KEY.to_string(),
            label: self.device_name(),
            pid: 0,
            volume: scalar_to_percent(master_volume),
            muted: master_muted,
            kind: SessionKind::Master,
            executable: None,
        });

        // One row per program. Its sessions share a level (see
        // `rebuild_sessions` and `write_volume`), so the row shows the session
        // with the lowest process id, which keeps the choice stable.
        let mut programs: BTreeMap<&str, &Entry> = BTreeMap::new();
        for entry in self.sessions.values() {
            programs
                .entry(entry.key.as_str())
                .and_modify(|shown| {
                    if entry.pid < shown.pid {
                        *shown = entry;
                    }
                })
                .or_insert(entry);
        }

        snapshots.extend(programs.values().filter_map(|entry| entry.snapshot()));

        snapshots.sort_by(|a, b| {
            a.kind
                .rank()
                .cmp(&b.kind.rank())
                .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
        });

        let _ = self.events.send(Event::Sessions(snapshots));
        wake();
    }

    fn release_device(&mut self) {
        // Entries unregister themselves in Drop.
        self.sessions.clear();

        unsafe {
            let _ = self
                .manager
                .UnregisterSessionNotification(&self.session_callback);
            let _ = self
                .endpoint
                .UnregisterControlChangeNotify(&self.endpoint_callback);
        }
    }

    fn teardown(&mut self) {
        self.release_device();
        unsafe {
            let _ = self
                .enumerator
                .UnregisterEndpointNotificationCallback(&self.device_callback);
        }
    }
}

/// Take ownership of a `PWSTR` returned by COM, copy it, and free the original.
fn take_pwstr(raw: windows::core::PWSTR) -> Option<String> {
    if raw.is_null() {
        return None;
    }

    let text = unsafe { raw.to_string() }.ok();

    unsafe {
        CoTaskMemFree(Some(raw.as_ptr() as *const _));
    }

    text
}

/// Endpoint id of a device, e.g. `{0.0.0.00000000}.{8a0e…}`.
fn device_id(device: &IMMDevice) -> Option<String> {
    unsafe { device.GetId() }.ok().and_then(take_pwstr)
}

/// Name as shown in the Windows sound settings.
fn friendly_name(device: &IMMDevice) -> String {
    unsafe {
        let Ok(store) = device.OpenPropertyStore(STGM_READ) else {
            return "Unknown".into();
        };
        let Ok(value) = store.GetValue(&PKEY_Device_FriendlyName) else {
            return "Unknown".into();
        };
        value.to_string()
    }
}
