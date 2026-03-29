use std::io::{Read, Write};
use std::sync::mpsc;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use tungstenite::client::connect;
use tungstenite::protocol::WebSocket;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::Message;

use crate::listen::{PlaybackClock, RETENTION_MS};
use crate::log::{is_verbose, now_string};
use crate::meta::controller::Control;
use crate::meta::error::MetaResult;
use crate::meta::timeline::TimelineStore;
use crate::station::Station;
use crate::ui::UiEvent;

use super::control::invalidate_ui_schedule;
use super::model::{
    GatewayEnvelope, GatewayHello, EVENT_TRACK_UPDATE, EVENT_TRACK_UPDATE_REQUEST, OP_DISPATCH,
    OP_HEARTBEAT_ACK, OP_HELLO,
};
use super::parse::parse_track_batch;

const HEARTBEAT_PAYLOAD: &str = r#"{"op":9}"#;
const TRACK_UPDATE_REQUEST_PAYLOAD: &str = r#"{"op":2}"#;

macro_rules! debug_gateway {
    ($($arg:tt)*) => {
        if is_verbose() {
            println!("[{}] {}", now_string(), format_args!($($arg)*));
        }
    };
}

pub(super) fn run_once(
    station: Station,
    sender: mpsc::Sender<UiEvent>,
    rx: &mpsc::Receiver<Control>,
    clock: Arc<PlaybackClock>,
    ui_sched_id: Arc<AtomicU64>,
    timeline: Arc<TimelineStore>,
    paused: &mut bool,
) -> MetaResult<()> {
    if let Ok(Control::Stop) | Err(mpsc::TryRecvError::Disconnected) = rx.try_recv() {
        return Ok(());
    }

    let url = station.ws_url();
    let (mut ws, _response) = connect(url)?;
    set_maybe_tls_read_timeout(ws.get_mut(), Duration::from_millis(200))?;
    debug_gateway!("Gateway connected to LISTEN.moe");

    let heartbeat_ms = read_hello_heartbeat(&mut ws)?;
    let _ = ws.send(Message::Text(HEARTBEAT_PAYLOAD.into()));
    let _ = ws.send(Message::Text(TRACK_UPDATE_REQUEST_PAYLOAD.into()));

    let heartbeat_dur = heartbeat_ms.map(Duration::from_millis);
    let mut last_heartbeat: Option<Instant> = heartbeat_dur.map(|_| Instant::now());
    let mut last_any_msg = Instant::now();
    let mut last_heartbeat_ack: Option<Instant> = heartbeat_dur.map(|_| Instant::now());

    loop {
        match rx.try_recv() {
            Ok(Control::Stop) | Err(mpsc::TryRecvError::Disconnected) => {
                invalidate_ui_schedule(&ui_sched_id);
                break;
            }
            Ok(Control::Pause) => {
                debug_gateway!("Pausing meta data");
                *paused = true;
                invalidate_ui_schedule(&ui_sched_id);
            }
            Ok(Control::Resume) => {
                debug_gateway!("Resuming meta data");
                *paused = false;
                resync_ui(
                    sender.clone(),
                    timeline.clone(),
                    &clock,
                    ui_sched_id.clone(),
                );
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }

        if let (Some(interval), Some(last)) = (heartbeat_dur, last_heartbeat.as_mut()) {
            if last.elapsed() >= interval {
                if let Err(err) = ws.send(Message::Text(HEARTBEAT_PAYLOAD.into())) {
                    eprintln!("Gateway heartbeat send error: {err}");
                    break;
                }
                *last = Instant::now();
            }
        }

        if let Some(hb) = heartbeat_ms {
            if let Some(ack) = last_heartbeat_ack.as_ref() {
                let max_silence = Duration::from_millis(hb.saturating_mul(3));
                if ack.elapsed() > max_silence {
                    eprintln!(
                        "Gateway heartbeat ACK timeout (>{:?}); reconnecting…",
                        max_silence
                    );
                    break;
                }
            }
        } else {
            const MAX_INACTIVITY: Duration = Duration::from_secs(30);
            if last_any_msg.elapsed() > MAX_INACTIVITY {
                eprintln!(
                    "Gateway inactivity timeout (>{:?}); reconnecting…",
                    MAX_INACTIVITY
                );
                break;
            }
        }

        let msg = match ws.read() {
            Ok(msg) => msg,
            Err(tungstenite::Error::ConnectionClosed) => break,
            Err(tungstenite::Error::Io(ref err))
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(err) => return Err(err.into()),
        };

        if !msg.is_text() {
            continue;
        }

        let txt = msg.into_text()?;
        let env: GatewayEnvelope = match serde_json::from_str(&txt) {
            Ok(env) => env,
            Err(err) => {
                eprintln!("Gateway JSON parse error: {err}");
                continue;
            }
        };

        last_any_msg = Instant::now();

        match (env.op, env.t.as_deref()) {
            (OP_HEARTBEAT_ACK, _) => {
                last_heartbeat_ack = Some(Instant::now());
                debug_gateway!("Gateway heartbeat");
            }
            (OP_DISPATCH, Some(EVENT_TRACK_UPDATE | EVENT_TRACK_UPDATE_REQUEST)) => {
                if let Some(batch) = parse_track_batch(&env.d) {
                    debug_gateway!(
                        "live track update: {} - {} [{}] (duration={})",
                        batch.current.artist,
                        batch.current.title,
                        batch.current.album,
                        batch.current.duration_secs
                    );

                    let current = batch.current.clone();
                    let mut tracks = batch.history;
                    tracks.push(batch.current);
                    timeline.insert_tracks(tracks)?;
                    let retention_floor = clock.live_head_ms().saturating_sub(RETENTION_MS);
                    let _ = timeline.prune_before(retention_floor)?;

                    if !*paused {
                        if clock.is_direct_live_mode() {
                            invalidate_ui_schedule(&ui_sched_id);
                            debug_gateway!("ui live snap: {} - {}", current.artist, current.title);
                            let _ = sender.send(UiEvent::TrackChanged(current));
                        } else {
                            resync_ui(
                                sender.clone(),
                                timeline.clone(),
                                &clock,
                                ui_sched_id.clone(),
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn resync_ui(
    sender: mpsc::Sender<UiEvent>,
    timeline: Arc<TimelineStore>,
    clock: &Arc<PlaybackClock>,
    ui_sched_id: Arc<AtomicU64>,
) {
    invalidate_ui_schedule(&ui_sched_id);

    let cursor_ms = match clock.playback_cursor_ms() {
        0 => {
            let live_head_ms = clock.live_head_ms();
            if live_head_ms == 0 {
                schedule_delayed_resync(
                    sender,
                    timeline,
                    clock.clone(),
                    ui_sched_id,
                    Duration::from_millis(250),
                );
                return;
            }
            live_head_ms
        }
        cursor_ms => cursor_ms,
    };

    if let Some(track) = timeline.track_for_cursor(cursor_ms) {
        debug_gateway!("ui snap: {} - {}", track.artist, track.title);
        let _ = sender.send(UiEvent::TrackChanged(track));
    }

    schedule_next_ui_switch(sender, timeline, cursor_ms, ui_sched_id);
}
fn schedule_delayed_resync(
    sender: mpsc::Sender<UiEvent>,
    timeline: Arc<TimelineStore>,
    clock: Arc<PlaybackClock>,
    ui_sched_id: Arc<AtomicU64>,
    wait: Duration,
) {
    let my_id = ui_sched_id.fetch_add(1, Ordering::Relaxed) + 1;
    thread::spawn(move || {
        thread::sleep(wait);
        if ui_sched_id.load(Ordering::Relaxed) != my_id {
            return;
        }
        resync_ui(sender, timeline, &clock, ui_sched_id);
    });
}

fn schedule_next_ui_switch(
    sender: mpsc::Sender<UiEvent>,
    timeline: Arc<TimelineStore>,
    cursor_ms: u64,
    ui_sched_id: Arc<AtomicU64>,
) {
    let Some(next) = timeline.next_after(cursor_ms) else {
        return;
    };

    let my_id = ui_sched_id.fetch_add(1, Ordering::Relaxed) + 1;
    let wait_ms = next.start_time_ms.saturating_sub(cursor_ms);

    thread::spawn(move || {
        if wait_ms > 0 {
            thread::sleep(Duration::from_millis(wait_ms));
        }

        if ui_sched_id.load(Ordering::Relaxed) != my_id {
            return;
        }

        let next_cursor_ms = next.start_time_ms;
        let _ = sender.send(UiEvent::TrackChanged(next));
        schedule_next_ui_switch(sender, timeline, next_cursor_ms, ui_sched_id);
    });
}

fn read_hello_heartbeat<S>(ws: &mut WebSocket<S>) -> MetaResult<Option<u64>>
where
    S: Read + Write,
{
    match ws.read() {
        Ok(msg) => {
            if msg.is_text() {
                let txt = msg.into_text()?;
                let env: GatewayEnvelope = serde_json::from_str(&txt)?;

                if env.op == OP_HELLO {
                    let hello: GatewayHello = serde_json::from_value(env.d)?;
                    return Ok(Some(hello.heartbeat));
                }
            }
            Ok(None)
        }
        Err(tungstenite::Error::ConnectionClosed) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn set_maybe_tls_read_timeout(
    stream: &mut MaybeTlsStream<std::net::TcpStream>,
    dur: Duration,
) -> std::io::Result<()> {
    match stream {
        MaybeTlsStream::Plain(tcp) => tcp.set_read_timeout(Some(dur)),
        MaybeTlsStream::Rustls(tls) => tls.get_mut().set_read_timeout(Some(dur)),
        _ => Ok(()),
    }
}
