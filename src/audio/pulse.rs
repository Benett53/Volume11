//! The audio engine thread on Linux.
//!
//! Talks to the PulseAudio protocol, which PipeWire serves through
//! `pipewire-pulse` on every current desktop. libpulse runs its own event loop
//! thread (the "threaded mainloop"); its callbacks only convert what they are
//! given into owned values and push them into channels. This thread does all
//! the bookkeeping, holding the mainloop lock only for the moment a request is
//! issued, which mirrors the Windows engine: callbacks feed a channel, one
//! thread owns the state.
//!
//! The mapping onto the Windows model:
//! - a sink is a playback device, its volume is the master volume,
//! - a sink input is an audio session of a program,
//! - streams with the role `event` (notification sounds) are "system sounds".

use std::collections::{BTreeMap, HashMap};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, select, unbounded};
use libpulse_binding as pulse;
use pulse::callbacks::ListResult;
use pulse::context::introspect::{SinkInfo, SinkInputInfo};
use pulse::context::subscribe::{Facility, InterestMaskSet, Operation as SubscribeOperation};
use pulse::context::{Context, FlagSet as ContextFlags, State};
use pulse::mainloop::threaded::Mainloop;
use pulse::proplist::Proplist;
use pulse::volume::{ChannelVolumes, Volume};

use super::process::{friendly_label, usable_display_name};
use super::{Command, DeviceInfo, Event, MASTER_KEY, SYSTEM_KEY, SessionKind, SessionSnapshot};

/// How long a change notification for something this app just wrote is taken
/// to be the echo of that write. Pulse change events carry no origin, unlike
/// the event context GUID WASAPI passes along, so time is the only marker.
/// Without this a slider being dragged would jump back to older values as the
/// notifications for them arrive.
const ECHO_WINDOW: Duration = Duration::from_millis(400);

/// Upper bound for one query answer. The server answers in microseconds; this
/// only matters if it hangs.
const QUERY_TIMEOUT: Duration = Duration::from_secs(3);

/// Executables that host other programs. Their name says nothing about which
/// program is playing, so the stream's application name is used instead:
/// Electron apps (Vesktop, Discord builds), Java, Python, Wine/Proton games.
const HOST_BINARIES: &[&str] = &[
    "electron",
    "java",
    "node",
    "python",
    "python3",
    "mono",
    "wine",
    "wine64",
    "wine-preloader",
    "wine64-preloader",
    "wineserver",
    "bwrap",
    "flatpak-spawn",
];

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

/// Start the audio thread. `wake` is called after every event so the UI can
/// repaint even when it is otherwise idle.
pub fn spawn<F>(wake: F) -> AudioHandle
where
    F: Fn() + Send + 'static,
{
    let (command_tx, command_rx) = unbounded::<Command>();
    let (event_tx, event_rx) = unbounded::<Event>();

    let thread = std::thread::Builder::new()
        .name("volume11-audio".into())
        .spawn(move || {
            // The sound server may come up after the session starts, or be
            // restarted by the user. Wait for it rather than giving up; this
            // only polls while there is no connection.
            let mut reported = false;
            loop {
                match Engine::connect(event_tx.clone()) {
                    Ok(mut engine) => {
                        reported = false;
                        if engine.run(&command_rx, &wake) == Exit::Shutdown {
                            break;
                        }
                        let _ = event_tx.send(Event::Problem("Sound server disconnected".into()));
                        wake();
                    }
                    Err(error) => {
                        if !reported {
                            let _ =
                                event_tx.send(Event::Problem(format!("No sound server: {error}")));
                            wake();
                            reported = true;
                        }
                    }
                }
                match command_rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                    _ => {}
                }
            }
        })
        .expect("audio thread must start");

    AudioHandle {
        commands: command_tx,
        events: event_rx,
        thread: Some(thread),
    }
}

#[derive(PartialEq, Eq)]
enum Exit {
    Shutdown,
    Disconnected,
}

/// A sink input, reduced to what Volume11 needs.
#[derive(Debug, Clone)]
struct Stream {
    index: u32,
    key: String,
    label: String,
    kind: SessionKind,
    /// Icon name or path, resolved by the UI.
    icon: Option<String>,
    pid: u32,
    channels: u8,
    volume: u8,
    muted: bool,
}

#[derive(Debug, Clone)]
struct Sink {
    index: u32,
    name: String,
    description: String,
    channels: u8,
    volume: u8,
    muted: bool,
}

/// Raw subscription events, pushed by the libpulse thread.
#[derive(Debug, Clone, Copy)]
struct Notice {
    facility: Facility,
    operation: SubscribeOperation,
    index: u32,
}

fn to_percent(volumes: &ChannelVolumes) -> u8 {
    let ratio = f64::from(volumes.avg().0) / f64::from(Volume::NORMAL.0);
    (ratio * 100.0).round().clamp(0.0, 100.0) as u8
}

fn to_volumes(percent: u8, channels: u8) -> ChannelVolumes {
    let raw = (f64::from(Volume::NORMAL.0) * f64::from(percent.min(100)) / 100.0).round() as u32;
    let mut volumes = ChannelVolumes::default();
    volumes.set(channels.max(1), Volume(raw));
    volumes
}

fn prop(list: &Proplist, key: &str) -> Option<String> {
    list.get_str(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The configuration key of a stream: the program's executable name where it
/// identifies the program, otherwise the application name it reports.
///
/// `-bin` is dropped because several browsers run as `firefox-bin` or
/// `zen-bin` while their launcher, and their Windows executable, carry the
/// plain name.
pub(crate) fn stream_key(
    binary: Option<&str>,
    application: Option<&str>,
    node: Option<&str>,
) -> Option<String> {
    let binary = binary
        .map(str::to_lowercase)
        .filter(|binary| !HOST_BINARIES.contains(&binary.as_str()));
    let key = binary
        .or_else(|| application.map(str::to_lowercase))
        .or_else(|| node.map(str::to_lowercase))?;
    let key = key
        .strip_suffix("-bin")
        .or_else(|| key.strip_suffix(".bin"))
        .unwrap_or(&key)
        .to_string();
    (!key.is_empty()).then_some(key)
}

/// A readable name: the application name unless it is just a lower-case
/// program name, which reads better tidied up (`vesktop` → `Vesktop`).
fn stream_label(application: Option<&str>, key: &str) -> String {
    match application.and_then(usable_display_name) {
        Some(name) if name.chars().any(char::is_uppercase) => name,
        Some(name) => friendly_label(&name),
        None => friendly_label(key),
    }
}

fn stream_from(info: &SinkInputInfo) -> Option<Stream> {
    let props = &info.proplist;
    let pid: u32 = prop(props, "application.process.id")
        .and_then(|pid| pid.parse().ok())
        .unwrap_or(0);

    // Never list Volume11 itself.
    if pid == std::process::id() {
        return None;
    }

    let application = prop(props, "application.name");
    let is_event = prop(props, "media.role").as_deref() == Some("event");

    let (key, label, kind) = if is_event {
        (
            SYSTEM_KEY.to_string(),
            "System sounds".to_string(),
            SessionKind::System,
        )
    } else {
        let key = stream_key(
            prop(props, "application.process.binary").as_deref(),
            application.as_deref(),
            prop(props, "node.name").as_deref(),
        )?;
        let label = stream_label(application.as_deref(), &key);
        (key, label, SessionKind::App)
    };

    let icon = if kind == SessionKind::App {
        prop(props, "application.icon_name").or_else(|| Some(key.clone()))
    } else {
        None
    };

    Some(Stream {
        index: info.index,
        key,
        label,
        kind,
        icon,
        pid,
        channels: info.volume.len(),
        volume: to_percent(&info.volume),
        muted: info.mute,
    })
}

fn sink_from(info: &SinkInfo) -> Option<Sink> {
    let name = info.name.as_deref()?.to_string();
    Some(Sink {
        index: info.index,
        description: info
            .description
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| name.clone()),
        name,
        channels: info.volume.len(),
        volume: to_percent(&info.volume),
        muted: info.mute,
    })
}

struct Engine {
    events: Sender<Event>,
    notices: Receiver<Notice>,

    // Declared before the mainloop so it is dropped first.
    context: Context,
    mainloop: Mainloop,

    /// Name of the sink that is the current default ("the device").
    sink: Option<Sink>,
    streams: HashMap<u32, Stream>,

    /// When this app last wrote each stream (by index) and the master volume.
    written: HashMap<u32, Instant>,
    master_written: Option<Instant>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.mainloop.lock();
        self.context.set_subscribe_callback(None);
        self.context.disconnect();
        self.mainloop.unlock();
        self.mainloop.stop();
    }
}

impl Engine {
    fn connect(events: Sender<Event>) -> Result<Self, String> {
        let mut mainloop = Mainloop::new().ok_or("mainloop")?;
        let mut properties = Proplist::new().ok_or("proplist")?;
        let _ = properties.set_str("application.name", "Volume11");
        let _ = properties.set_str("application.id", "volume11");
        let _ = properties.set_str("application.icon_name", "volume11");
        let mut context =
            Context::new_with_proplist(&mainloop, "Volume11", &properties).ok_or("context")?;

        context
            .connect(None, ContextFlags::NOAUTOSPAWN, None)
            .map_err(|error| error.to_string().unwrap_or_else(|| format!("{error:?}")))?;
        mainloop
            .start()
            .map_err(|error| error.to_string().unwrap_or_else(|| format!("{error:?}")))?;

        // Connection takes a few milliseconds; polling during that short
        // window is simpler than signalling back from the state callback.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            mainloop.lock();
            let state = context.get_state();
            mainloop.unlock();
            match state {
                State::Ready => break,
                State::Failed | State::Terminated => {
                    mainloop.stop();
                    return Err("connection refused".into());
                }
                _ if Instant::now() > deadline => {
                    mainloop.stop();
                    return Err("connection timed out".into());
                }
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }

        let (notice_tx, notice_rx) = unbounded::<Notice>();
        mainloop.lock();
        context.set_subscribe_callback(Some(Box::new(move |facility, operation, index| {
            if let (Some(facility), Some(operation)) = (facility, operation) {
                let _ = notice_tx.send(Notice {
                    facility,
                    operation,
                    index,
                });
            }
        })));
        let _ = context.subscribe(
            InterestMaskSet::SINK | InterestMaskSet::SINK_INPUT | InterestMaskSet::SERVER,
            |_| {},
        );
        mainloop.unlock();

        let mut engine = Self {
            events,
            notices: notice_rx,
            context,
            mainloop,
            sink: None,
            streams: HashMap::new(),
            written: HashMap::new(),
            master_written: None,
        };

        engine.sink = engine.default_sink();
        for stream in engine.all_streams() {
            engine.streams.insert(stream.index, stream);
        }

        Ok(engine)
    }

    fn connected(&mut self) -> bool {
        self.mainloop.lock();
        let state = self.context.get_state();
        self.mainloop.unlock();
        state == State::Ready
    }

    // --- Queries ---------------------------------------------------------------

    /// Issue a list query and collect its answer on this thread.
    fn collect<T: Send + 'static>(
        &mut self,
        issue: impl FnOnce(&Context, Sender<Option<T>>),
    ) -> Vec<T> {
        let (tx, rx) = unbounded::<Option<T>>();
        self.mainloop.lock();
        issue(&self.context, tx);
        self.mainloop.unlock();

        let mut items = Vec::new();
        let deadline = Instant::now() + QUERY_TIMEOUT;
        while let Ok(Some(item)) = rx.recv_deadline(deadline) {
            items.push(item);
        }
        items
    }

    fn all_streams(&mut self) -> Vec<Stream> {
        self.collect(|context, tx| {
            context
                .introspect()
                .get_sink_input_info_list(move |result| match result {
                    ListResult::Item(info) => {
                        if let Some(stream) = stream_from(info) {
                            let _ = tx.send(Some(stream));
                        }
                    }
                    _ => {
                        let _ = tx.send(None);
                    }
                });
        })
    }

    fn stream(&mut self, index: u32) -> Option<Stream> {
        self.collect(|context, tx| {
            context
                .introspect()
                .get_sink_input_info(index, move |result| match result {
                    ListResult::Item(info) => {
                        if let Some(stream) = stream_from(info) {
                            let _ = tx.send(Some(stream));
                        }
                    }
                    _ => {
                        let _ = tx.send(None);
                    }
                });
        })
        .pop()
    }

    fn all_sinks(&mut self) -> Vec<Sink> {
        self.collect(|context, tx| {
            context
                .introspect()
                .get_sink_info_list(move |result| match result {
                    ListResult::Item(info) => {
                        if let Some(sink) = sink_from(info) {
                            let _ = tx.send(Some(sink));
                        }
                    }
                    _ => {
                        let _ = tx.send(None);
                    }
                });
        })
    }

    fn default_sink_name(&mut self) -> Option<String> {
        self.collect(|context, tx| {
            context.introspect().get_server_info(move |info| {
                let _ = tx.send(info.default_sink_name.as_deref().map(str::to_string));
                let _ = tx.send(None);
            });
        })
        .pop()
    }

    fn default_sink(&mut self) -> Option<Sink> {
        let name = self.default_sink_name()?;
        self.all_sinks().into_iter().find(|sink| sink.name == name)
    }

    // --- Main loop -------------------------------------------------------------

    fn run<F: Fn()>(&mut self, commands: &Receiver<Command>, wake: &F) -> Exit {
        self.publish_devices(wake);
        if let Some(sink) = &self.sink {
            let _ = self.events.send(Event::DeviceChanged {
                id: sink.name.clone(),
                name: sink.description.clone(),
            });
        }
        self.publish_sessions(wake);

        // Liveness is checked on a slow timer: a dead server sends nothing,
        // and the connection state only changes on the libpulse thread.
        let heartbeat = Duration::from_secs(5);

        loop {
            select! {
                recv(commands) -> command => match command {
                    Ok(Command::Shutdown) | Err(_) => return Exit::Shutdown,
                    Ok(command) => self.handle_command(command, wake),
                },
                recv(self.notices) -> notice => {
                    if let Ok(notice) = notice {
                        self.handle_notice(notice, wake);
                    }
                },
                default(heartbeat) => {
                    if !self.connected() {
                        return Exit::Disconnected;
                    }
                }
            }
        }
    }

    fn handle_command<F: Fn()>(&mut self, command: Command, wake: &F) {
        match command {
            Command::SetVolume { key, volume } => self.write(&key, Some(volume), None),
            Command::SetMute { key, muted } => self.write(&key, None, Some(muted)),
            Command::ApplyAll(entries) => {
                for (key, volume, muted) in entries {
                    self.write(&key, Some(volume), Some(muted));
                }
                self.publish_sessions(wake);
            }
            Command::Refresh => {
                self.sink = self.default_sink();
                self.streams = self
                    .all_streams()
                    .into_iter()
                    .map(|stream| (stream.index, stream))
                    .collect();
                self.publish_sessions(wake);
            }
            Command::SetDefaultDevice(name) => {
                // The server answers with a change event, which runs the same
                // switch as a change made in the system settings.
                let (tx, rx) = unbounded::<bool>();
                self.mainloop.lock();
                let _ = self.context.set_default_sink(&name, move |ok| {
                    let _ = tx.send(ok);
                });
                self.mainloop.unlock();
                if rx.recv_timeout(QUERY_TIMEOUT) != Ok(true) {
                    let _ = self
                        .events
                        .send(Event::Problem(format!("Could not switch device: {name}")));
                    wake();
                }
            }
            Command::Shutdown => {}
        }
    }

    /// Set the level of the master volume or of every stream of a program.
    fn write(&mut self, key: &str, volume: Option<u8>, muted: Option<bool>) {
        let now = Instant::now();

        if key == MASTER_KEY {
            let Some(sink) = self.sink.as_mut() else {
                return;
            };
            if let Some(volume) = volume {
                sink.volume = volume;
            }
            if let Some(muted) = muted {
                sink.muted = muted;
            }
            let (name, channels) = (sink.name.clone(), sink.channels);
            self.master_written = Some(now);

            self.mainloop.lock();
            let mut introspect = self.context.introspect();
            if let Some(volume) = volume {
                let _ =
                    introspect.set_sink_volume_by_name(&name, &to_volumes(volume, channels), None);
            }
            if let Some(muted) = muted {
                let _ = introspect.set_sink_mute_by_name(&name, muted, None);
            }
            self.mainloop.unlock();
            return;
        }

        let targets: Vec<(u32, u8)> = self
            .streams
            .values_mut()
            .filter(|stream| stream.key == key)
            .map(|stream| {
                if let Some(volume) = volume {
                    stream.volume = volume;
                }
                if let Some(muted) = muted {
                    stream.muted = muted;
                }
                (stream.index, stream.channels)
            })
            .collect();

        if targets.is_empty() {
            return;
        }

        self.mainloop.lock();
        let mut introspect = self.context.introspect();
        for &(index, channels) in &targets {
            if let Some(volume) = volume {
                let _ =
                    introspect.set_sink_input_volume(index, &to_volumes(volume, channels), None);
            }
            if let Some(muted) = muted {
                let _ = introspect.set_sink_input_mute(index, muted, None);
            }
        }
        self.mainloop.unlock();

        for (index, _) in targets {
            self.written.insert(index, now);
        }
    }

    fn is_echo(&self, index: u32) -> bool {
        self.written
            .get(&index)
            .is_some_and(|at| at.elapsed() < ECHO_WINDOW)
    }

    fn handle_notice<F: Fn()>(&mut self, notice: Notice, wake: &F) {
        use SubscribeOperation as Op;

        match (notice.facility, notice.operation) {
            (Facility::SinkInput, Op::New) => {
                let Some(stream) = self.stream(notice.index) else {
                    return;
                };
                let existing = self
                    .streams
                    .values()
                    .filter(|other| other.key == stream.key)
                    .min_by_key(|other| other.pid)
                    .map(|other| (other.volume, other.muted));
                let (key, label) = (stream.key.clone(), stream.label.clone());
                self.streams.insert(stream.index, stream);

                match existing {
                    // Another stream of a program already playing: match it,
                    // so a second stream does not undo a level set by hand.
                    Some((volume, muted)) => self.write(&key, Some(volume), Some(muted)),
                    // Notification sounds come as a new stream every time, so
                    // their saved level is applied like a program's.
                    None => {
                        let _ = self.events.send(Event::SessionAdded { key, label });
                    }
                }
                self.publish_sessions(wake);
            }
            (Facility::SinkInput, Op::Changed) => {
                let Some(stream) = self.stream(notice.index) else {
                    return;
                };
                let echo = self.is_echo(stream.index);
                let previous = self.streams.insert(stream.index, stream.clone());
                if echo {
                    // Keep the value this app wrote, not the late echo of an
                    // earlier step of the same drag.
                    if let (Some(previous), Some(current)) =
                        (previous, self.streams.get_mut(&notice.index))
                    {
                        current.volume = previous.volume;
                        current.muted = previous.muted;
                    }
                    return;
                }
                match previous {
                    Some(previous)
                        if previous.volume == stream.volume && previous.muted == stream.muted => {}
                    Some(_) => {
                        let _ = self.events.send(Event::Changed {
                            key: stream.key,
                            volume: stream.volume,
                            muted: stream.muted,
                        });
                        wake();
                    }
                    None => self.publish_sessions(wake),
                }
            }
            (Facility::SinkInput, Op::Removed) => {
                self.written.remove(&notice.index);
                if self.streams.remove(&notice.index).is_some() {
                    self.publish_sessions(wake);
                }
            }
            (Facility::Sink, Op::Changed) => {
                if self.sink.as_ref().map(|sink| sink.index) != Some(notice.index) {
                    return;
                }
                let echo = self
                    .master_written
                    .is_some_and(|at| at.elapsed() < ECHO_WINDOW);
                if echo {
                    return;
                }
                let Some(current) = self.default_sink() else {
                    return;
                };
                let changed = self.sink.as_ref().is_some_and(|sink| {
                    sink.name == current.name
                        && (sink.volume != current.volume || sink.muted != current.muted)
                });
                if changed {
                    let _ = self.events.send(Event::Changed {
                        key: MASTER_KEY.to_string(),
                        volume: current.volume,
                        muted: current.muted,
                    });
                    self.sink = Some(current);
                    wake();
                }
            }
            (Facility::Sink, _) => self.publish_devices(wake),
            (Facility::Server, _) => {
                let current = self.default_sink();
                let switched = match (&self.sink, &current) {
                    (Some(old), Some(new)) => old.name != new.name,
                    (None, None) => false,
                    _ => true,
                };
                if switched {
                    self.sink = current;
                    if let Some(sink) = &self.sink {
                        let _ = self.events.send(Event::DeviceChanged {
                            id: sink.name.clone(),
                            name: sink.description.clone(),
                        });
                    } else {
                        let _ = self
                            .events
                            .send(Event::Problem("No playback device".into()));
                    }
                    self.publish_sessions(wake);
                    self.publish_devices(wake);
                }
            }
            _ => {}
        }
    }

    // --- Publishing ------------------------------------------------------------

    fn publish_devices<F: Fn()>(&mut self, wake: &F) {
        let mut list: Vec<DeviceInfo> = self
            .all_sinks()
            .into_iter()
            .map(|sink| DeviceInfo {
                id: sink.name,
                name: sink.description,
            })
            .collect();
        list.sort_by_key(|device| device.name.to_lowercase());

        let current = self
            .sink
            .as_ref()
            .map(|sink| sink.name.clone())
            .unwrap_or_default();
        let _ = self.events.send(Event::Devices { list, current });
        wake();
    }

    fn publish_sessions<F: Fn()>(&self, wake: &F) {
        let Some(sink) = &self.sink else {
            return;
        };

        let mut snapshots = Vec::with_capacity(self.streams.len() + 1);
        snapshots.push(SessionSnapshot {
            key: MASTER_KEY.to_string(),
            label: sink.description.clone(),
            pid: 0,
            volume: sink.volume,
            muted: sink.muted,
            kind: SessionKind::Master,
            executable: None,
        });

        // One row per program, showing its stream with the lowest process id
        // (then index), which keeps the choice stable.
        let mut programs: BTreeMap<&str, &Stream> = BTreeMap::new();
        for stream in self.streams.values() {
            programs
                .entry(stream.key.as_str())
                .and_modify(|shown| {
                    if (stream.pid, stream.index) < (shown.pid, shown.index) {
                        *shown = stream;
                    }
                })
                .or_insert(stream);
        }

        snapshots.extend(programs.values().map(|stream| SessionSnapshot {
            key: stream.key.clone(),
            label: stream.label.clone(),
            pid: stream.pid,
            volume: stream.volume,
            muted: stream.muted,
            kind: stream.kind,
            executable: stream.icon.clone(),
        }));

        snapshots.sort_by(|a, b| {
            a.kind
                .rank()
                .cmp(&b.kind.rank())
                .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
        });

        let _ = self.events.send(Event::Sessions(snapshots));
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_binaries_fall_back_to_the_application_name() {
        assert_eq!(
            stream_key(Some("electron"), Some("vesktop"), None).as_deref(),
            Some("vesktop")
        );
        assert_eq!(
            stream_key(Some("wine64-preloader"), Some("Game.exe"), None).as_deref(),
            Some("game.exe")
        );
    }

    #[test]
    fn executable_names_are_normalised() {
        assert_eq!(
            stream_key(Some("zen-bin"), Some("Zen"), None).as_deref(),
            Some("zen")
        );
        assert_eq!(
            stream_key(Some("Discord"), None, None).as_deref(),
            Some("discord")
        );
        assert_eq!(
            stream_key(None, None, Some("spotify")).as_deref(),
            Some("spotify")
        );
        assert_eq!(stream_key(None, None, None), None);
    }

    #[test]
    fn labels_are_readable() {
        assert_eq!(stream_label(Some("vesktop"), "vesktop"), "Vesktop");
        assert_eq!(stream_label(Some("Spotify"), "spotify"), "Spotify");
        assert_eq!(stream_label(None, "game.exe"), "Game");
    }

    #[test]
    fn volumes_round_trip() {
        for percent in 0u8..=100 {
            assert_eq!(to_percent(&to_volumes(percent, 2)), percent);
        }
    }
}
