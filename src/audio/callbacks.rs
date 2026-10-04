//! COM callback objects.
//!
//! Windows calls these from its own threads (RPC worker threads, the audio engine
//! thread). They must therefore do as little as possible: every handler just pushes
//! a message into a channel and returns. All real work happens back on the engine
//! thread, which owns the COM objects.

use crossbeam_channel::Sender;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    AUDIO_VOLUME_NOTIFICATION_DATA, AudioSessionDisconnectReason, AudioSessionState, DEVICE_STATE,
    EDataFlow, ERole, IAudioSessionControl, IAudioSessionEvents, IAudioSessionEvents_Impl,
    IAudioSessionNotification, IAudioSessionNotification_Impl, IMMNotificationClient,
    IMMNotificationClient_Impl,
};
use windows::core::{BOOL, GUID, PCWSTR, Ref, Result, implement};

/// Messages produced by the COM callbacks, consumed by the engine thread.
#[derive(Debug, Clone)]
pub enum Notification {
    /// A new audio session was created. The engine re-enumerates to pick it up.
    SessionCreated,
    /// A session changed state (active / inactive / expired).
    SessionStateChanged { session: String, expired: bool },
    /// Volume or mute changed for a session, triggered from outside this app.
    SessionVolume {
        session: String,
        scalar: f32,
        muted: bool,
        /// Event context, so the engine can ignore echoes of its own writes.
        context: GUID,
    },
    /// Master (endpoint) volume changed outside this app, e.g. the volume keys.
    MasterVolume {
        scalar: f32,
        muted: bool,
        context: GUID,
    },
    /// The default playback device changed.
    DefaultDeviceChanged,
    /// A device was added, removed, enabled or disabled.
    DevicesChanged,
}

/// `IAudioSessionNotification` — fires when any process opens a new audio stream.
#[implement(IAudioSessionNotification)]
pub struct SessionNotification {
    tx: Sender<Notification>,
}

impl SessionNotification {
    pub fn new(tx: Sender<Notification>) -> Self {
        Self { tx }
    }
}

impl IAudioSessionNotification_Impl for SessionNotification_Impl {
    fn OnSessionCreated(&self, _new_session: Ref<'_, IAudioSessionControl>) -> Result<()> {
        // Deliberately does not touch the session here: the engine re-enumerates on
        // its own thread, where it can safely cache the new session.
        let _ = self.tx.send(Notification::SessionCreated);
        Ok(())
    }
}

/// `IAudioSessionEvents` — per session, so external volume changes are picked up
/// immediately instead of being discovered by a polling timer.
#[implement(IAudioSessionEvents)]
pub struct SessionEvents {
    tx: Sender<Notification>,
    /// Session instance identifier. A process can own several sessions, so the
    /// process id is not enough to tell them apart.
    session: String,
}

impl SessionEvents {
    pub fn new(tx: Sender<Notification>, session: String) -> Self {
        Self { tx, session }
    }
}

#[allow(non_snake_case)]
impl IAudioSessionEvents_Impl for SessionEvents_Impl {
    fn OnSimpleVolumeChanged(
        &self,
        new_volume: f32,
        new_mute: BOOL,
        event_context: *const GUID,
    ) -> Result<()> {
        let context = if event_context.is_null() {
            GUID::zeroed()
        } else {
            unsafe { *event_context }
        };

        let _ = self.tx.send(Notification::SessionVolume {
            session: self.session.clone(),
            scalar: new_volume,
            muted: new_mute.as_bool(),
            context,
        });

        Ok(())
    }

    fn OnStateChanged(&self, new_state: AudioSessionState) -> Result<()> {
        // AudioSessionStateExpired == 2: the process released the stream for good.
        let _ = self.tx.send(Notification::SessionStateChanged {
            session: self.session.clone(),
            expired: new_state.0 == 2,
        });
        Ok(())
    }

    fn OnSessionDisconnected(&self, _reason: AudioSessionDisconnectReason) -> Result<()> {
        let _ = self.tx.send(Notification::SessionStateChanged {
            session: self.session.clone(),
            expired: true,
        });
        Ok(())
    }

    fn OnDisplayNameChanged(&self, _new: &PCWSTR, _ctx: *const GUID) -> Result<()> {
        Ok(())
    }

    fn OnIconPathChanged(&self, _new: &PCWSTR, _ctx: *const GUID) -> Result<()> {
        Ok(())
    }

    fn OnChannelVolumeChanged(
        &self,
        _count: u32,
        _volumes: *const f32,
        _channel: u32,
        _ctx: *const GUID,
    ) -> Result<()> {
        Ok(())
    }

    fn OnGroupingParamChanged(&self, _group: *const GUID, _ctx: *const GUID) -> Result<()> {
        Ok(())
    }
}

/// `IMMNotificationClient` — fixes the long standing bug where the app kept
/// controlling the previous output device after switching to headphones.
#[implement(IMMNotificationClient)]
pub struct DeviceNotification {
    tx: Sender<Notification>,
}

impl DeviceNotification {
    pub fn new(tx: Sender<Notification>) -> Self {
        Self { tx }
    }
}

#[allow(non_snake_case)]
impl IMMNotificationClient_Impl for DeviceNotification_Impl {
    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        _device_id: &PCWSTR,
    ) -> Result<()> {
        // eRender == 0, eMultimedia == 1. Only the device we actually use matters.
        if flow.0 == 0 && role.0 == 1 {
            let _ = self.tx.send(Notification::DefaultDeviceChanged);
        }
        Ok(())
    }

    // Plugging headphones in or out, or enabling a device in the Sound panel,
    // changes what can be switched to.
    fn OnDeviceStateChanged(&self, _device_id: &PCWSTR, _new_state: DEVICE_STATE) -> Result<()> {
        let _ = self.tx.send(Notification::DevicesChanged);
        Ok(())
    }

    fn OnDeviceAdded(&self, _device_id: &PCWSTR) -> Result<()> {
        let _ = self.tx.send(Notification::DevicesChanged);
        Ok(())
    }

    fn OnDeviceRemoved(&self, _device_id: &PCWSTR) -> Result<()> {
        let _ = self.tx.send(Notification::DevicesChanged);
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _device_id: &PCWSTR, _key: &PROPERTYKEY) -> Result<()> {
        Ok(())
    }
}

/// `IAudioEndpointVolumeCallback` — keeps the master row in sync when the user
/// presses the hardware volume keys or moves the system slider.
#[implement(IAudioEndpointVolumeCallback)]
pub struct EndpointVolumeCallback {
    tx: Sender<Notification>,
}

impl EndpointVolumeCallback {
    pub fn new(tx: Sender<Notification>) -> Self {
        Self { tx }
    }
}

#[allow(non_snake_case)]
impl IAudioEndpointVolumeCallback_Impl for EndpointVolumeCallback_Impl {
    fn OnNotify(&self, data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        if data.is_null() {
            return Ok(());
        }

        let (scalar, muted, context) = unsafe {
            (
                (*data).fMasterVolume,
                (*data).bMuted.as_bool(),
                (*data).guidEventContext,
            )
        };

        let _ = self.tx.send(Notification::MasterVolume {
            scalar,
            muted,
            context,
        });

        Ok(())
    }
}
