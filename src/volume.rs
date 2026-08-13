//! Linux desktop volume integration.

use libpulse_binding as pulse;
use pulse::{
    callbacks::ListResult,
    context::{
        introspect::SinkInputInfo,
        subscribe::{Facility, InterestMaskSet, Operation},
        Context, FlagSet, State as ContextState,
    },
    mainloop::standard::{IterateResult, Mainloop},
    proplist::{properties::APPLICATION_PROCESS_ID, Proplist},
    volume::{ChannelVolumes, Volume},
};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashSet},
    env, fs,
    panic::{self, AssertUnwindSafe},
    path::Path,
    process,
    rc::Rc,
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
        OnceLock,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// PipeWire property used to associate an audio stream with this controller.
pub const STREAM_INSTANCE_PROPERTY: &str = "listenmoe.instance.id";
const APPLICATION_NAME: &str = "Listen Moe";
const PIPEWIRE_PROPS: &str = "PIPEWIRE_PROPS";
const PULSE_PROP_APPLICATION_NAME: &str = "PULSE_PROP_application.name";
const PULSE_PROP_APPLICATION_ID: &str = "PULSE_PROP_application.id";
const PULSE_PROP_APPLICATION_PROCESS_ID: &str = "PULSE_PROP_application.process.id";
const PULSE_PROP_STREAM_INSTANCE: &str = "PULSE_PROP_listenmoe.instance.id";
const MEDIA_CATEGORY: &str = "media.category";
const MANAGER_CATEGORY: &str = "Manager";

static STREAM_IDENTITY: OnceLock<StreamIdentity> = OnceLock::new();

/// Identity injected into the playback stream's `PIPEWIRE_PROPS` at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamIdentity {
    marker: String,
    process_id: String,
    allow_process_id_fallback: bool,
}

impl StreamIdentity {
    fn new(
        marker: impl Into<String>,
        process_id: impl Into<String>,
        allow_process_id_fallback: bool,
    ) -> Self {
        Self {
            marker: marker.into(),
            process_id: process_id.into(),
            allow_process_id_fallback,
        }
    }

    /// Create the marker before playback or any other worker threads start.
    pub fn for_current_process() -> Self {
        let process_id = process::id().to_string();
        Self::new(
            generate_instance_marker(&process_id),
            process_id,
            !running_in_flatpak(),
        )
    }

    pub fn marker(&self) -> &str {
        &self.marker
    }
}

fn generate_instance_marker(process_id: &str) -> String {
    let kernel_uuid = fs::read_to_string("/proc/sys/kernel/random/uuid").ok();
    let unix_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    instance_marker_from(kernel_uuid.as_deref(), process_id, unix_nanos)
}

fn instance_marker_from(kernel_uuid: Option<&str>, process_id: &str, unix_nanos: u128) -> String {
    if let Some(uuid) = kernel_uuid.map(str::trim).filter(|uuid| !uuid.is_empty()) {
        format!("listenmoe-{uuid}")
    } else {
        format!("listenmoe-{process_id}-{unix_nanos}")
    }
}

fn running_in_flatpak() -> bool {
    env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").is_file()
}

/// Configure the playback stream identity before any audio or worker threads start.
///
/// Existing valid `PIPEWIRE_PROPS` entries are preserved. Our properties are
/// appended, so they take precedence if the caller supplied the same keys.
pub fn configure_stream_identity(app_id: &str) {
    let identity = STREAM_IDENTITY
        .get_or_init(StreamIdentity::for_current_process)
        .clone();
    let marker = identity.marker();
    let existing = env::var(PIPEWIRE_PROPS).ok();
    let merged = merge_pipewire_props(existing.as_deref(), app_id, marker, &identity.process_id);

    // This function is called during single-threaded startup, before Rodio or
    // the volume controller can read the process environment.
    env::set_var(PIPEWIRE_PROPS, merged);
    env::set_var(PULSE_PROP_APPLICATION_NAME, APPLICATION_NAME);
    env::set_var(PULSE_PROP_APPLICATION_ID, app_id);
    env::set_var(PULSE_PROP_APPLICATION_PROCESS_ID, &identity.process_id);
    env::set_var(PULSE_PROP_STREAM_INSTANCE, marker);
}

fn merge_pipewire_props(
    existing: Option<&str>,
    app_id: &str,
    marker: &str,
    process_id: &str,
) -> String {
    let existing = existing.and_then(valid_pipewire_object_body);
    let mut merged = String::from("{");

    if let Some(body) = existing.filter(|body| !body.trim().is_empty()) {
        merged.push(' ');
        merged.push_str(body.trim());
    }

    append_pipewire_property(&mut merged, "application.name", APPLICATION_NAME);
    append_pipewire_property(&mut merged, "application.id", app_id);
    append_pipewire_property(&mut merged, APPLICATION_PROCESS_ID, process_id);
    append_pipewire_property(&mut merged, STREAM_INSTANCE_PROPERTY, marker);
    merged.push_str(" }");
    merged
}

fn valid_pipewire_object_body(properties: &str) -> Option<&str> {
    let properties = properties.trim();
    if properties.is_empty() {
        return Some("");
    }

    let body = properties.strip_prefix('{')?.strip_suffix('}')?;
    if balanced_spa_object(properties) {
        Some(body)
    } else {
        None
    }
}

fn balanced_spa_object(properties: &str) -> bool {
    let mut depth = 0_u32;
    let mut quoted = false;
    let mut escaped = false;
    let mut closed_outer = false;

    for character in properties.chars() {
        if closed_outer && !character.is_whitespace() {
            return false;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && character == '\\' {
            escaped = true;
            continue;
        }
        if character == '"' {
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }

        match character {
            '{' => depth = depth.saturating_add(1),
            '}' => {
                let Some(next_depth) = depth.checked_sub(1) else {
                    return false;
                };
                depth = next_depth;
                if depth == 0 {
                    closed_outer = true;
                }
            }
            _ => {}
        }
    }

    closed_outer && depth == 0 && !quoted && !escaped
}

fn append_pipewire_property(output: &mut String, key: &str, value: &str) {
    output.push(' ');
    output.push_str(key);
    output.push_str(" = \"");
    output.push_str(&escape_spa_string(value));
    output.push('"');
}

fn escape_spa_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character => escaped.push(character),
        }
    }
    escaped
}

/// A user-initiated change to the application's desktop volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeCommand {
    /// Set every matching sink input to a percentage in `0..=100` and unmute it.
    ///
    /// Values above 100 are defensively clamped.
    SetPercent(u8),
}

/// Canonical sound-server state. Volume and mute are deliberately independent:
/// muting must not destroy the level that should be restored when unmuted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DesiredVolume {
    raw_percent: u32,
    muted: bool,
}

impl DesiredVolume {
    fn from_command(command: VolumeCommand) -> Self {
        let VolumeCommand::SetPercent(percent) = command;
        Self {
            raw_percent: u32::from(percent.min(100)),
            muted: false,
        }
    }

    fn from_event(raw_percent: u32, muted: bool) -> Self {
        Self { raw_percent, muted }
    }
}

/// Current desktop volume state for this application instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeEvent {
    /// The controller temporarily lost its sound-server connection.
    ///
    /// Consumers must retain their previous gain mode: a playback stream may
    /// still be alive in PipeWire while the Pulse-compatible control service
    /// restarts.
    Disconnected,
    /// The controller is connected, but this instance has no controllable playback stream.
    Unavailable,
    /// A matching sink input is available.
    ///
    /// `raw_percent` deliberately is not capped at 100. Consumers may clamp it
    /// for display, but must not write that clamped value back unless the user
    /// explicitly changes the control.
    Available { raw_percent: u32, muted: bool },
}

/// Start the PipeWire-compatible volume controller on a dedicated thread.
///
/// The worker exits when all command senders are dropped. Controller failures
/// never prevent playback from starting; connection loss is reported as
/// `Disconnected` while the initial no-stream fallback remains available if
/// thread creation itself fails.
pub fn spawn_controller() -> (Sender<VolumeCommand>, Receiver<VolumeEvent>) {
    let (command_tx, command_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();

    // Establish a deterministic initial UI state even if thread creation fails.
    let _ = event_tx.send(VolumeEvent::Unavailable);

    let identity = STREAM_IDENTITY
        .get_or_init(StreamIdentity::for_current_process)
        .clone();
    let _ = thread::Builder::new()
        .name("listenmoe-volume".into())
        .spawn(move || worker_loop(command_rx, event_tx, identity));

    (command_tx, event_rx)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionOutcome {
    Retry,
    Shutdown,
}

fn worker_loop(
    command_rx: Receiver<VolumeCommand>,
    event_tx: Sender<VolumeEvent>,
    identity: StreamIdentity,
) {
    let emitter = Rc::new(RefCell::new(EventEmitter::new(
        event_tx,
        VolumeEvent::Unavailable,
    )));
    let desired = Rc::new(Cell::new(None));

    loop {
        // A transient race with a disconnect can make an introspection method
        // panic. Keep that isolated to this optional integration thread.
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            run_connection(
                &command_rx,
                Rc::clone(&emitter),
                &identity,
                Rc::clone(&desired),
            )
        }))
        .unwrap_or(ConnectionOutcome::Retry);

        if outcome == ConnectionOutcome::Shutdown {
            break;
        }

        emitter.borrow_mut().emit(VolumeEvent::Disconnected);
        if !wait_before_retry(&command_rx, &desired) {
            break;
        }
    }
}

fn run_connection(
    command_rx: &Receiver<VolumeCommand>,
    emitter: Rc<RefCell<EventEmitter>>,
    identity: &StreamIdentity,
    desired: Rc<Cell<Option<DesiredVolume>>>,
) -> ConnectionOutcome {
    let Some(mut mainloop) = Mainloop::new() else {
        return ConnectionOutcome::Retry;
    };
    let Some(context_properties) = manager_context_properties() else {
        return ConnectionOutcome::Retry;
    };
    let Some(mut context) = Context::new_with_proplist(
        &mainloop,
        "Listen Moe volume controller",
        &context_properties,
    ) else {
        return ConnectionOutcome::Retry;
    };
    if context.connect(None, FlagSet::NOFLAGS, None).is_err() {
        return ConnectionOutcome::Retry;
    }

    loop {
        if !iterate(&mut mainloop) {
            return ConnectionOutcome::Retry;
        }
        if receive_pending_commands(command_rx, &desired) == ConnectionOutcome::Shutdown {
            return ConnectionOutcome::Shutdown;
        }

        match context.get_state() {
            ContextState::Ready => break,
            ContextState::Failed | ContextState::Terminated => {
                return ConnectionOutcome::Retry;
            }
            _ => thread::sleep(POLL_INTERVAL),
        }
    }

    let tracker = Rc::new(RefCell::new(StreamTracker::new(Rc::clone(&desired))));
    let refreshes = Rc::new(RefCell::new(HashSet::<u32>::new()));
    let subscription_failed = Rc::new(Cell::new(false));

    install_subscription(
        &mut context,
        Rc::clone(&tracker),
        Rc::clone(&refreshes),
        Rc::clone(&emitter),
        Rc::clone(&subscription_failed),
    );
    request_all_streams(
        &context,
        Rc::clone(&tracker),
        Rc::clone(&emitter),
        identity.clone(),
        context.introspect(),
        Rc::clone(&subscription_failed),
    );

    loop {
        if !iterate(&mut mainloop) || context.get_state() != ContextState::Ready {
            return ConnectionOutcome::Retry;
        }
        if subscription_failed.get() {
            return ConnectionOutcome::Retry;
        }

        let pending = refreshes.take();
        for index in pending {
            request_stream(
                &context,
                index,
                Rc::clone(&tracker),
                Rc::clone(&emitter),
                identity.clone(),
                context.introspect(),
            );
        }

        match command_rx.recv_timeout(POLL_INTERVAL) {
            Ok(command) => {
                let command = drain_to_latest(command_rx, command);
                let desired_volume = DesiredVolume::from_command(command);
                desired.set(Some(desired_volume));
                tracker.borrow_mut().set_desired(desired_volume);
                let mut introspector = context.introspect();
                apply_desired(&mut introspector, &tracker.borrow(), desired_volume);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return ConnectionOutcome::Shutdown,
        }
    }
}

fn manager_context_properties() -> Option<Proplist> {
    let mut properties = Proplist::new()?;
    properties.set_str(MEDIA_CATEGORY, MANAGER_CATEGORY).ok()?;
    Some(properties)
}

fn install_subscription(
    context: &mut Context,
    tracker: Rc<RefCell<StreamTracker>>,
    refreshes: Rc<RefCell<HashSet<u32>>>,
    emitter: Rc<RefCell<EventEmitter>>,
    subscription_failed: Rc<Cell<bool>>,
) {
    let callback_tracker = Rc::clone(&tracker);
    let callback_refreshes = Rc::clone(&refreshes);
    let callback_emitter = Rc::clone(&emitter);
    context.set_subscribe_callback(Some(Box::new(move |facility, operation, index| {
        if facility != Some(Facility::SinkInput) {
            return;
        }

        if operation == Some(Operation::Removed) {
            callback_refreshes.borrow_mut().remove(&index);
            callback_tracker.borrow_mut().remove(index);
            emit_current(&callback_tracker, &callback_emitter);
        } else if matches!(operation, Some(Operation::New | Operation::Changed)) {
            callback_refreshes.borrow_mut().insert(index);
        }
    })));

    let failed = subscription_failed;
    let _ = context.subscribe(InterestMaskSet::SINK_INPUT, move |success| {
        if !success {
            failed.set(true);
        }
    });
}

fn request_all_streams(
    context: &Context,
    tracker: Rc<RefCell<StreamTracker>>,
    emitter: Rc<RefCell<EventEmitter>>,
    identity: StreamIdentity,
    mut introspector: pulse::context::introspect::Introspector,
    request_failed: Rc<Cell<bool>>,
) {
    let _ = context
        .introspect()
        .get_sink_input_info_list(move |result| match result {
            ListResult::Item(info) => {
                update_from_info(&tracker, &emitter, &identity, &mut introspector, info);
            }
            ListResult::End => emit_current(&tracker, &emitter),
            ListResult::Error => {
                tracker.borrow_mut().clear();
                emit_current(&tracker, &emitter);
                request_failed.set(true);
            }
        });
}

fn request_stream(
    context: &Context,
    index: u32,
    tracker: Rc<RefCell<StreamTracker>>,
    emitter: Rc<RefCell<EventEmitter>>,
    identity: StreamIdentity,
    mut introspector: pulse::context::introspect::Introspector,
) {
    let mut saw_item = false;
    let _ = context
        .introspect()
        .get_sink_input_info(index, move |result| match result {
            ListResult::Item(info) => {
                saw_item = true;
                update_from_info(&tracker, &emitter, &identity, &mut introspector, info);
            }
            ListResult::End => {
                if !saw_item {
                    tracker.borrow_mut().remove(index);
                    emit_current(&tracker, &emitter);
                }
            }
            ListResult::Error => {
                tracker.borrow_mut().remove(index);
                emit_current(&tracker, &emitter);
            }
        });
}

fn update_from_info(
    tracker: &Rc<RefCell<StreamTracker>>,
    emitter: &Rc<RefCell<EventEmitter>>,
    identity: &StreamIdentity,
    introspector: &mut pulse::context::introspect::Introspector,
    info: &SinkInputInfo<'_>,
) {
    let instance_marker = info.proplist.get_str(STREAM_INSTANCE_PROPERTY);
    let process_id = info.proplist.get_str(APPLICATION_PROCESS_ID);
    let matches =
        matches_stream_identity(instance_marker.as_deref(), process_id.as_deref(), identity);

    if matches && info.has_volume && info.volume_writable && info.volume.len() > 0 {
        let update = tracker.borrow_mut().observe(
            info.index,
            raw_to_percent(info.volume.max().0),
            info.mute,
            info.volume.len(),
        );
        match update {
            StreamUpdate::Publish(event) => emitter.borrow_mut().emit(event),
            StreamUpdate::Apply(desired) => {
                apply_desired_to_stream(introspector, info.index, info.volume.len(), desired);
            }
            StreamUpdate::PublishAndApply {
                event,
                desired,
                except,
            } => {
                emitter.borrow_mut().emit(event);
                apply_desired_except(introspector, &tracker.borrow(), desired, except);
            }
        }
    } else {
        tracker.borrow_mut().remove(info.index);
        emit_current(tracker, emitter);
    }
}

fn matches_stream_identity(
    instance_marker: Option<&str>,
    process_id: Option<&str>,
    identity: &StreamIdentity,
) -> bool {
    match instance_marker {
        Some(marker) => marker == identity.marker,
        None => {
            identity.allow_process_id_fallback && process_id == Some(identity.process_id.as_str())
        }
    }
}

fn emit_current(tracker: &Rc<RefCell<StreamTracker>>, emitter: &Rc<RefCell<EventEmitter>>) {
    let event = tracker.borrow().current_event();
    emitter.borrow_mut().emit(event);
}

fn apply_desired(
    introspector: &mut pulse::context::introspect::Introspector,
    tracker: &StreamTracker,
    desired: DesiredVolume,
) {
    for action in tracker.plan(desired) {
        apply_action(introspector, action);
    }
}

fn apply_desired_except(
    introspector: &mut pulse::context::introspect::Introspector,
    tracker: &StreamTracker,
    desired: DesiredVolume,
    except: u32,
) {
    for (&index, stream) in &tracker.streams {
        if index != except && stream.pending == Some(desired) {
            apply_desired_to_stream(introspector, index, stream.channels, desired);
        }
    }
}

fn apply_desired_to_stream(
    introspector: &mut pulse::context::introspect::Introspector,
    index: u32,
    channels: u8,
    desired: DesiredVolume,
) {
    for action in planned_actions(index, channels, desired) {
        apply_action(introspector, action);
    }
}

fn apply_action(
    introspector: &mut pulse::context::introspect::Introspector,
    action: PlannedAction,
) {
    match action {
        PlannedAction::SetMute { index, muted } => {
            let _ = introspector.set_sink_input_mute(index, muted, None);
        }
        PlannedAction::SetVolume {
            index,
            channels,
            raw_volume,
        } => {
            let mut volumes = ChannelVolumes::default();
            volumes.set(channels, Volume(raw_volume));
            let _ = introspector.set_sink_input_volume(index, &volumes, None);
        }
    }
}

fn iterate(mainloop: &mut Mainloop) -> bool {
    matches!(mainloop.iterate(false), IterateResult::Success(_))
}

fn receive_pending_commands(
    command_rx: &Receiver<VolumeCommand>,
    desired: &Cell<Option<DesiredVolume>>,
) -> ConnectionOutcome {
    loop {
        match command_rx.try_recv() {
            Ok(command) => desired.set(Some(DesiredVolume::from_command(command))),
            Err(TryRecvError::Empty) => return ConnectionOutcome::Retry,
            Err(TryRecvError::Disconnected) => return ConnectionOutcome::Shutdown,
        }
    }
}

fn drain_to_latest(
    command_rx: &Receiver<VolumeCommand>,
    mut latest: VolumeCommand,
) -> VolumeCommand {
    while let Ok(command) = command_rx.try_recv() {
        latest = command;
    }
    latest
}

fn wait_before_retry(
    command_rx: &Receiver<VolumeCommand>,
    desired: &Cell<Option<DesiredVolume>>,
) -> bool {
    let deadline = Instant::now() + RETRY_INTERVAL;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return true;
        }

        match command_rx.recv_timeout(deadline - now) {
            Ok(command) => desired.set(Some(DesiredVolume::from_command(command))),
            Err(RecvTimeoutError::Timeout) => return true,
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

fn raw_to_percent(raw_volume: u32) -> u32 {
    let normal = u64::from(Volume::NORMAL.0);
    ((u64::from(raw_volume) * 100 + normal / 2) / normal) as u32
}

fn percent_to_raw(percent: u32) -> u32 {
    let percent = u64::from(percent);
    let normal = u64::from(Volume::NORMAL.0);
    ((normal * percent + 50) / 100).min(u64::from(Volume::MAX.0)) as u32
}

struct EventEmitter {
    event_tx: Sender<VolumeEvent>,
    last_event: VolumeEvent,
}

impl EventEmitter {
    fn new(event_tx: Sender<VolumeEvent>, initial: VolumeEvent) -> Self {
        Self {
            event_tx,
            last_event: initial,
        }
    }

    fn emit(&mut self, event: VolumeEvent) {
        if event == self.last_event {
            return;
        }
        self.last_event = event;
        let _ = self.event_tx.send(event);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TrackedStream {
    raw_percent: u32,
    muted: bool,
    channels: u8,
    pending: Option<DesiredVolume>,
}

#[derive(Debug)]
struct StreamTracker {
    streams: BTreeMap<u32, TrackedStream>,
    desired: Rc<Cell<Option<DesiredVolume>>>,
    authoritative_index: Option<u32>,
}

impl StreamTracker {
    fn new(desired: Rc<Cell<Option<DesiredVolume>>>) -> Self {
        Self {
            streams: BTreeMap::new(),
            desired,
            authoritative_index: None,
        }
    }

    fn observe(&mut self, index: u32, raw_percent: u32, muted: bool, channels: u8) -> StreamUpdate {
        let event = VolumeEvent::Available { raw_percent, muted };

        if let Some(stream) = self.streams.get_mut(&index) {
            let pending = stream.pending;
            stream.raw_percent = raw_percent;
            stream.muted = muted;
            stream.channels = channels;

            if let Some(desired) = pending {
                if observation_confirms(desired, raw_percent, muted) {
                    stream.pending = None;
                    self.authoritative_index = Some(index);
                    return StreamUpdate::Publish(event);
                }
                return StreamUpdate::Apply(desired);
            }

            // A non-pending update came from the desktop. It becomes canonical
            // and is copied to sibling streams.
            let desired = DesiredVolume::from_event(raw_percent, muted);
            self.desired.set(Some(desired));
            self.authoritative_index = Some(index);
            for (&other_index, stream) in &mut self.streams {
                if other_index != index {
                    stream.pending =
                        (!observation_confirms(desired, stream.raw_percent, stream.muted))
                            .then_some(desired);
                }
            }
            return StreamUpdate::PublishAndApply {
                event,
                desired,
                except: index,
            };
        }

        if let Some(desired) = self.desired.get() {
            let confirmed = observation_confirms(desired, raw_percent, muted);
            self.streams.insert(
                index,
                TrackedStream {
                    raw_percent,
                    muted,
                    channels,
                    pending: (!confirmed).then_some(desired),
                },
            );
            if confirmed {
                self.authoritative_index.get_or_insert(index);
                StreamUpdate::Publish(event)
            } else {
                StreamUpdate::Apply(desired)
            }
        } else {
            self.desired
                .set(Some(DesiredVolume::from_event(raw_percent, muted)));
            self.authoritative_index = Some(index);
            self.streams.insert(
                index,
                TrackedStream {
                    raw_percent,
                    muted,
                    channels,
                    pending: None,
                },
            );
            StreamUpdate::Publish(event)
        }
    }

    fn set_desired(&mut self, desired: DesiredVolume) {
        self.desired.set(Some(desired));
        for stream in self.streams.values_mut() {
            stream.pending = (!observation_confirms(desired, stream.raw_percent, stream.muted))
                .then_some(desired);
        }
    }

    fn remove(&mut self, index: u32) {
        self.streams.remove(&index);
        if self.authoritative_index == Some(index) {
            self.authoritative_index = self
                .streams
                .iter()
                .find_map(|(&index, stream)| stream.pending.is_none().then_some(index));
        }
    }

    fn clear(&mut self) {
        self.streams.clear();
        self.authoritative_index = None;
    }

    fn current_event(&self) -> VolumeEvent {
        self.authoritative_index
            .and_then(|index| self.streams.get(&index))
            .map_or(VolumeEvent::Unavailable, |stream| VolumeEvent::Available {
                raw_percent: stream.raw_percent,
                muted: stream.muted,
            })
    }

    fn plan(&self, desired: DesiredVolume) -> Vec<PlannedAction> {
        let mut actions = Vec::with_capacity(self.streams.len() * 2);

        for (&index, stream) in &self.streams {
            actions.extend(planned_actions(index, stream.channels, desired));
        }
        actions
    }
}

impl Default for StreamTracker {
    fn default() -> Self {
        Self::new(Rc::new(Cell::new(None)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamUpdate {
    Publish(VolumeEvent),
    Apply(DesiredVolume),
    PublishAndApply {
        event: VolumeEvent,
        desired: DesiredVolume,
        except: u32,
    },
}

fn observation_confirms(desired: DesiredVolume, raw_percent: u32, muted: bool) -> bool {
    muted == desired.muted && raw_percent == desired.raw_percent
}

fn planned_actions(index: u32, channels: u8, desired: DesiredVolume) -> [PlannedAction; 2] {
    [
        PlannedAction::SetVolume {
            index,
            channels,
            raw_volume: percent_to_raw(desired.raw_percent),
        },
        PlannedAction::SetMute {
            index,
            muted: desired.muted,
        },
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlannedAction {
    SetVolume {
        index: u32,
        channels: u8,
        raw_volume: u32,
    },
    SetMute {
        index: u32,
        muted: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desired(raw_percent: u32, muted: bool) -> DesiredVolume {
        DesiredVolume { raw_percent, muted }
    }

    #[test]
    fn instance_markers_use_uuid_and_unique_fallback_material() {
        assert_eq!(
            instance_marker_from(Some(" uuid-123\n"), "2", 10),
            "listenmoe-uuid-123"
        );
        assert_ne!(
            instance_marker_from(None, "2", 10),
            instance_marker_from(None, "2", 11)
        );
        assert_ne!(instance_marker_from(None, "2", 10), "2");
    }

    #[test]
    fn stream_matching_prefers_marker_and_disables_pid_fallback_in_flatpak() {
        let direct = StreamIdentity::new("instance-123", "42", true);
        let flatpak = StreamIdentity::new("instance-123", "2", false);

        assert!(matches_stream_identity(
            Some("instance-123"),
            Some("unrelated-process"),
            &direct,
        ));
        assert!(matches_stream_identity(None, Some("42"), &direct));
        assert!(matches_stream_identity(
            Some("instance-123"),
            Some("2"),
            &flatpak
        ));
        assert!(!matches_stream_identity(None, Some("2"), &flatpak));

        // An explicit marker is authoritative; a mismatched stream must not be
        // accepted merely because its fallback process property happens to match.
        assert!(!matches_stream_identity(
            Some("another-instance"),
            Some("42"),
            &direct,
        ));
        assert!(!matches_stream_identity(None, None, &direct));
    }

    #[test]
    fn pipewire_merge_preserves_valid_existing_properties() {
        let merged = merge_pipewire_props(
            Some(" { custom.key = \"kept\" nested = { enabled = true } } "),
            "io.github.example.Listen",
            "random-marker",
            "1234",
        );

        assert!(merged.starts_with("{ custom.key = \"kept\" nested = { enabled = true }"));
        assert!(merged.contains("application.name = \"Listen Moe\""));
        assert!(merged.contains("application.id = \"io.github.example.Listen\""));
        assert!(merged.contains("application.process.id = \"1234\""));
        assert!(merged.contains("listenmoe.instance.id = \"random-marker\""));
    }

    #[test]
    fn pipewire_merge_discards_malformed_existing_properties() {
        let merged = merge_pipewire_props(
            Some("{ custom.key = \"unterminated }"),
            "io.github.example.Listen",
            "random-marker",
            "1234",
        );

        assert!(!merged.contains("custom.key"));
        assert!(merged.starts_with("{ application.name"));
    }

    #[test]
    fn pipewire_values_are_escaped() {
        let merged = merge_pipewire_props(None, "an\\app\"id\n", "marker\tvalue", "12");

        assert!(merged.contains("application.id = \"an\\\\app\\\"id\\n\""));
        assert!(merged.contains("listenmoe.instance.id = \"marker\\tvalue\""));
    }

    #[test]
    fn controller_context_is_tagged_as_a_manager() {
        let properties = manager_context_properties().expect("PulseAudio proplist");

        assert_eq!(
            properties.get_str(MEDIA_CATEGORY).as_deref(),
            Some(MANAGER_CATEGORY)
        );
    }

    #[test]
    fn first_discovered_stream_wins_without_a_desired_value() {
        let mut tracker = StreamTracker::default();
        assert_eq!(
            tracker.observe(4, 45, false, 2),
            StreamUpdate::Publish(VolumeEvent::Available {
                raw_percent: 45,
                muted: false,
            })
        );
        assert_eq!(
            tracker.observe(9, 82, false, 2),
            StreamUpdate::Apply(desired(45, false))
        );

        assert_eq!(
            tracker.current_event(),
            VolumeEvent::Available {
                raw_percent: 45,
                muted: false,
            }
        );

        tracker.remove(4);
        assert_eq!(tracker.current_event(), VolumeEvent::Unavailable);

        assert_eq!(
            tracker.observe(9, 45, false, 2),
            StreamUpdate::Publish(VolumeEvent::Available {
                raw_percent: 45,
                muted: false,
            })
        );
        assert_eq!(
            tracker.current_event(),
            VolumeEvent::Available {
                raw_percent: 45,
                muted: false,
            }
        );
    }

    #[test]
    fn desired_before_discovery_suppresses_stale_state_and_reapplies_until_confirmed() {
        let desired_state = Rc::new(Cell::new(Some(desired(37, false))));
        let mut tracker = StreamTracker::new(desired_state);

        assert_eq!(
            tracker.observe(4, 100, false, 2),
            StreamUpdate::Apply(desired(37, false))
        );
        assert_eq!(tracker.current_event(), VolumeEvent::Unavailable);
        assert_eq!(
            tracker.observe(4, 37, true, 2),
            StreamUpdate::Apply(desired(37, false))
        );
        assert_eq!(tracker.current_event(), VolumeEvent::Unavailable);
        assert_eq!(
            tracker.observe(4, 37, false, 2),
            StreamUpdate::Publish(VolumeEvent::Available {
                raw_percent: 37,
                muted: false,
            })
        );
    }

    #[test]
    fn external_change_becomes_shared_desired_value() {
        let desired_state = Rc::new(Cell::new(None));
        let mut tracker = StreamTracker::new(Rc::clone(&desired_state));
        let _ = tracker.observe(4, 45, false, 2);
        let _ = tracker.observe(9, 45, false, 2);

        assert_eq!(
            tracker.observe(4, 63, false, 2),
            StreamUpdate::PublishAndApply {
                event: VolumeEvent::Available {
                    raw_percent: 63,
                    muted: false,
                },
                desired: desired(63, false),
                except: 4,
            }
        );
        assert_eq!(desired_state.get(), Some(desired(63, false)));
        assert_eq!(tracker.streams[&9].pending, Some(desired(63, false)));
    }

    #[test]
    fn conversion_preserves_amplified_server_values() {
        let amplified_raw = Volume::NORMAL.0 + Volume::NORMAL.0 / 2;
        assert_eq!(raw_to_percent(amplified_raw), 150);
        assert_eq!(raw_to_percent(Volume::NORMAL.0), 100);
        assert_eq!(percent_to_raw(100), Volume::NORMAL.0);
        assert_eq!(percent_to_raw(150), amplified_raw);

        // Local commands are still capped at 100%; only a value already
        // observed from the sound server can retain amplification.
        assert_eq!(
            DesiredVolume::from_command(VolumeCommand::SetPercent(150)),
            desired(100, false)
        );
    }

    #[test]
    fn external_mute_preserves_raw_level_across_stream_recreation() {
        let desired_state = Rc::new(Cell::new(None));
        let mut tracker = StreamTracker::new(Rc::clone(&desired_state));
        let _ = tracker.observe(4, 68, false, 2);

        assert_eq!(
            tracker.observe(4, 68, true, 2),
            StreamUpdate::PublishAndApply {
                event: VolumeEvent::Available {
                    raw_percent: 68,
                    muted: true,
                },
                desired: desired(68, true),
                except: 4,
            }
        );
        assert_eq!(desired_state.get(), Some(desired(68, true)));

        tracker.remove(4);
        assert_eq!(
            tracker.observe(9, 100, false, 2),
            StreamUpdate::Apply(desired(68, true))
        );
        assert_eq!(
            planned_actions(9, 2, desired(68, true)),
            [
                PlannedAction::SetVolume {
                    index: 9,
                    channels: 2,
                    raw_volume: percent_to_raw(68),
                },
                PlannedAction::SetMute {
                    index: 9,
                    muted: true,
                },
            ]
        );

        // Volume and mute notifications may arrive separately. Neither the
        // stale initial value nor this intermediate state becomes canonical.
        assert_eq!(
            tracker.observe(9, 68, false, 2),
            StreamUpdate::Apply(desired(68, true))
        );
        assert_eq!(
            tracker.observe(9, 68, true, 2),
            StreamUpdate::Publish(VolumeEvent::Available {
                raw_percent: 68,
                muted: true,
            })
        );
    }

    #[test]
    fn zero_sets_every_stream_to_zero_and_unmutes() {
        assert_eq!(
            DesiredVolume::from_command(VolumeCommand::SetPercent(0)),
            desired(0, false)
        );
        let mut tracker = StreamTracker::default();
        let _ = tracker.observe(4, 73, true, 2);
        tracker.streams.insert(
            9,
            TrackedStream {
                raw_percent: 41,
                muted: false,
                channels: 6,
                pending: None,
            },
        );

        assert_eq!(
            tracker.plan(desired(0, false)),
            vec![
                PlannedAction::SetVolume {
                    index: 4,
                    channels: 2,
                    raw_volume: Volume::MUTED.0,
                },
                PlannedAction::SetMute {
                    index: 4,
                    muted: false,
                },
                PlannedAction::SetVolume {
                    index: 9,
                    channels: 6,
                    raw_volume: Volume::MUTED.0,
                },
                PlannedAction::SetMute {
                    index: 9,
                    muted: false,
                },
            ]
        );
    }

    #[test]
    fn positive_value_updates_and_unmutes_every_stream() {
        assert_eq!(
            DesiredVolume::from_command(VolumeCommand::SetPercent(55)),
            desired(55, false)
        );
        let mut tracker = StreamTracker::default();
        let _ = tracker.observe(4, 20, true, 2);
        tracker.streams.insert(
            9,
            TrackedStream {
                raw_percent: 80,
                muted: false,
                channels: 6,
                pending: None,
            },
        );
        let raw_volume = percent_to_raw(55);

        assert_eq!(
            tracker.plan(desired(55, false)),
            vec![
                PlannedAction::SetVolume {
                    index: 4,
                    channels: 2,
                    raw_volume,
                },
                PlannedAction::SetMute {
                    index: 4,
                    muted: false,
                },
                PlannedAction::SetVolume {
                    index: 9,
                    channels: 6,
                    raw_volume,
                },
                PlannedAction::SetMute {
                    index: 9,
                    muted: false,
                },
            ]
        );
    }

    #[test]
    fn muted_state_keeps_the_servers_raw_level() {
        let mut tracker = StreamTracker::default();
        let _ = tracker.observe(4, 68, true, 2);

        assert_eq!(
            tracker.current_event(),
            VolumeEvent::Available {
                raw_percent: 68,
                muted: true,
            }
        );
    }
}
