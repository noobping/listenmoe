use serde::Deserialize;
use serde_json::Value;
use std::collections::VecDeque;
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

#[cfg(debug_assertions)]
use crate::log::now_string;

use super::controller::Control;
use super::error::MetaResult;
use super::schedule::{pick_track_for_playback, schedule_next_from_history, schedule_ui_switch};
use super::time_parse::parse_rfc3339_system_time;
use super::track::{TrackInfo, ALBUM_COVER_BASE, ARTIST_IMAGE_BASE};
use crate::station::Station;

/// Protocol-level types for the LISTEN.moe gateway

#[derive(Debug, Deserialize)]
struct GatewayHello {
    heartbeat: u64,
}

#[derive(Debug, Deserialize)]
struct GatewaySongPayload {
    song: Song,
    #[serde(rename = "startTime")]
    start_time: String,
}

#[derive(Debug, Deserialize)]
struct Song {
    title: Option<String>,
    #[serde(default)]
    artists: Vec<Artist>,
    #[serde(default)]
    albums: Vec<Album>,
    duration: Option<u32>,
}

impl Song {
    fn display_title(self: &Self) -> String {
        self.title
            .clone()
            .unwrap_or_else(|| "unknown title".to_owned())
    }

    fn display_artist(self: &Self) -> String {
        if self.artists.is_empty() {
            return "Unknown artist".to_owned();
        }

        self.artists
            .iter()
            .filter_map(|a| a.name.as_deref())
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn album_cover_url(self: &Self) -> Option<String> {
        self.albums
            .first()
            .and_then(|album| album.image.as_deref())
            .as_cdn_url(ALBUM_COVER_BASE)
    }

    fn artist_image_url(self: &Self) -> Option<String> {
        self.artists
            .first()
            .and_then(|artist| artist.image.as_deref())
            .as_cdn_url(ARTIST_IMAGE_BASE)
    }

    fn duration_secs(self: &Self) -> u32 {
        self.duration.unwrap_or(0)
    }
}

#[derive(Debug, Deserialize)]
struct Artist {
    name: Option<String>,
    image: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Album {
    image: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GatewayEnvelope {
    op: u8,
    #[serde(default)]
    t: Option<String>,
    #[serde(default)]
    d: Value,
}

trait CdnImageExt {
    fn as_cdn_url(self, base: &str) -> Option<String>;
}

impl CdnImageExt for Option<&str> {
    fn as_cdn_url(self, base: &str) -> Option<String> {
        self.map(|name| format!("{base}{name}"))
    }
}

const OP_HELLO: u8 = 0;
const OP_DISPATCH: u8 = 1;
const OP_HEARTBEAT_ACK: u8 = 10;
const EVENT_TRACK_UPDATE: &str = "TRACK_UPDATE";
const HEARTBEAT_PAYLOAD: &str = r#"{"op":9}"#;

macro_rules! debug_gateway {
    ($($arg:tt)*) => {
        #[cfg(debug_assertions)]
        println!("[{}] {}", now_string(), format_args!($($arg)*));
    };
}

enum OuterLoopAction {
    Continue,
    Stop,
    Sleep(Duration),
}

fn invalidate_ui_schedule(ui_sched_id: &Arc<AtomicU64>) {
    ui_sched_id.fetch_add(1, Ordering::Relaxed);
}

fn handle_outer_control(
    rx: &mpsc::Receiver<Control>,
    paused: &mut bool,
    ui_sched_id: &Arc<AtomicU64>,
    empty_sleep: Duration,
) -> OuterLoopAction {
    match rx.try_recv() {
        Ok(Control::Stop) | Err(mpsc::TryRecvError::Disconnected) => OuterLoopAction::Stop,
        Ok(Control::Pause) => {
            *paused = true;
            invalidate_ui_schedule(ui_sched_id);
            OuterLoopAction::Sleep(Duration::from_secs(1))
        }
        Ok(Control::Resume) => {
            *paused = false;
            invalidate_ui_schedule(ui_sched_id);
            OuterLoopAction::Sleep(Duration::from_secs(1))
        }
        Err(mpsc::TryRecvError::Empty) if empty_sleep.is_zero() => OuterLoopAction::Continue,
        Err(mpsc::TryRecvError::Empty) => OuterLoopAction::Sleep(empty_sleep),
    }
}

/// Outer reconnect loop using blocking tungstenite.
pub fn run_meta_loop(
    station: Station,
    sender: mpsc::Sender<TrackInfo>,
    rx: mpsc::Receiver<Control>,
    lag_ms: Arc<AtomicU64>,
    ui_sched_id: Arc<AtomicU64>,
) -> MetaResult<()> {
    let mut paused = false;
    let retry_delay = Duration::from_secs(5);

    loop {
        match handle_outer_control(&rx, &mut paused, &ui_sched_id, Duration::ZERO) {
            OuterLoopAction::Stop => return Ok(()),
            OuterLoopAction::Sleep(wait) => thread::sleep(wait),
            OuterLoopAction::Continue => {}
        }

        match run_once(
            station,
            sender.clone(),
            &rx,
            lag_ms.clone(),
            ui_sched_id.clone(),
            &mut paused,
        ) {
            Ok(()) => {}
            Err(err) => {
                eprintln!("Gateway connection error: {err}, retrying in 5s…");
            }
        }

        // Session ended or failed: apply control/backoff policy once.
        match handle_outer_control(&rx, &mut paused, &ui_sched_id, retry_delay) {
            OuterLoopAction::Stop => return Ok(()),
            OuterLoopAction::Sleep(wait) => thread::sleep(wait),
            OuterLoopAction::Continue => {}
        }
    }
}

/// Single websocket session, with a simple heartbeat loop.
/// Keeps history and does "snap-to-buffered-track" on Resume.
fn run_once(
    station: Station,
    sender: mpsc::Sender<TrackInfo>,
    rx: &mpsc::Receiver<Control>,
    lag_ms: Arc<AtomicU64>,
    ui_sched_id: Arc<AtomicU64>,
    paused: &mut bool,
) -> MetaResult<()> {
    if let Ok(Control::Stop) | Err(mpsc::TryRecvError::Disconnected) = rx.try_recv() {
        return Ok(());
    }

    let url = station.ws_url();
    let (mut ws, _response) = connect(url)?;
    set_maybe_tls_read_timeout(ws.get_mut(), Duration::from_millis(200))?;
    debug_gateway!("Gateway connected to LISTEN.moe");

    // Read hello and get heartbeat interval (if any).
    let heartbeat_ms = read_hello_heartbeat(&mut ws)?;
    // Send an immediate heartbeat once after HELLO, then continue on the interval.
    let _ = ws.send(Message::Text(HEARTBEAT_PAYLOAD.into()));

    let heartbeat_dur = heartbeat_ms.map(Duration::from_millis);
    let mut last_heartbeat: Option<Instant> = heartbeat_dur.map(|_| Instant::now());

    // Liveness tracking: when the network interface changes, the socket may stop delivering
    // messages without cleanly closing.
    let mut last_any_msg = Instant::now();
    let mut last_heartbeat_ack: Option<Instant> = heartbeat_dur.map(|_| Instant::now());

    let mut history: VecDeque<TrackInfo> = VecDeque::new();

    loop {
        // Check for control messages first.
        match rx.try_recv() {
            Ok(Control::Stop) | Err(mpsc::TryRecvError::Disconnected) => {
                invalidate_ui_schedule(&ui_sched_id);
                break;
            }
            Ok(Control::Pause) => {
                debug_gateway!("Pausing meta data");
                *paused = true;
                invalidate_ui_schedule(&ui_sched_id); // invalidate any pending scheduled sends
            }
            Ok(Control::Resume) => {
                debug_gateway!("Resuming meta data");
                *paused = false;
                invalidate_ui_schedule(&ui_sched_id); // invalidate timers from before pause

                // Snap UI to the track that matches buffered playback time.
                let lag = lag_ms.load(Ordering::Relaxed);
                if let Some(t) = pick_track_for_playback(&history, lag) {
                    debug_gateway!("ui snap: {} - {}", t.artist, t.title);
                }
                // Immediately snap UI to what playback should be on resume
                if let Some(correct) = pick_track_for_playback(&history, lag) {
                    let _ = sender.send(correct);
                }
                // Also schedule the next switch that should happen after resume
                schedule_next_from_history(sender.clone(), &history, lag, ui_sched_id.clone());
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }

        // Heartbeat: if an interval is known, send a heartbeat when it elapses.
        if let (Some(interval), Some(last)) = (heartbeat_dur, last_heartbeat.as_mut()) {
            if last.elapsed() >= interval {
                if let Err(err) = ws.send(Message::Text(HEARTBEAT_PAYLOAD.into())) {
                    eprintln!("Gateway heartbeat send error: {err}");
                    break;
                }
                *last = Instant::now();
            }
        }

        // If the socket goes silent, force a reconnect.
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
            // No heartbeat info from the server — fall back to a generic inactivity timeout.
            const MAX_INACTIVITY: Duration = Duration::from_secs(30);
            if last_any_msg.elapsed() > MAX_INACTIVITY {
                eprintln!(
                    "Gateway inactivity timeout (>{:?}); reconnecting…",
                    MAX_INACTIVITY
                );
                break;
            }
        }

        // Incoming messages.
        let msg = match ws.read() {
            Ok(msg) => msg,
            Err(tungstenite::Error::ConnectionClosed) => break,
            Err(tungstenite::Error::Io(ref e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // No websocket message right now; loop again so the process can check controls/heartbeats.
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
            (OP_DISPATCH, Some(EVENT_TRACK_UPDATE)) => {
                if let Some(info) = parse_track_info(&env.d) {
                    debug_gateway!(
                        "live track update: {} - {} (duration={})",
                        info.artist,
                        info.title,
                        info.duration_secs
                    );

                    history.push_back(info);

                    if !*paused {
                        let lag = lag_ms.load(Ordering::Relaxed);
                        let my_id = ui_sched_id.fetch_add(1, Ordering::Relaxed) + 1;

                        if let Some(track) = history.back() {
                            debug_gateway!(
                                "ui {} scheduled: {} - {} (lag_ms={})",
                                my_id,
                                track.artist,
                                track.title,
                                lag
                            );

                            // Schedule the *new* track to appear when playback reaches it
                            schedule_ui_switch(
                                sender.clone(),
                                track.clone(),
                                lag,
                                ui_sched_id.clone(),
                                my_id,
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

/// Read the initial hello and extract the heartbeat interval (if any).
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
        Err(err) => return Err(err.into()),
    }
}

/// Extract artist(s) + title from the gateway payload.
fn parse_track_info(d: &Value) -> Option<TrackInfo> {
    let payload: GatewaySongPayload = serde_json::from_value(d.clone()).ok()?;

    let start_time_utc = parse_rfc3339_system_time(&payload.start_time)?;
    let track = payload.song;

    Some(TrackInfo {
        artist: track.display_artist(),
        title: track.display_title(),
        album_cover: track.album_cover_url(),
        artist_image: track.artist_image_url(),
        start_time_utc,
        duration_secs: track.duration_secs(),
    })
}

fn set_maybe_tls_read_timeout(
    stream: &mut MaybeTlsStream<std::net::TcpStream>,
    dur: std::time::Duration,
) -> std::io::Result<()> {
    match stream {
        MaybeTlsStream::Plain(tcp) => tcp.set_read_timeout(Some(dur)),
        MaybeTlsStream::Rustls(tls) => tls.get_mut().set_read_timeout(Some(dur)),
        _ => Ok(()),
    }
}
