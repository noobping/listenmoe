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

const OP_HELLO: u8 = 0;
const OP_DISPATCH: u8 = 1;
const OP_HEARTBEAT_ACK: u8 = 10;
const EVENT_TRACK_UPDATE: &str = "TRACK_UPDATE";

/// Outer reconnect loop using blocking tungstenite.
pub fn run_meta_loop(
    station: Station,
    sender: mpsc::Sender<TrackInfo>,
    rx: mpsc::Receiver<Control>,
    lag_ms: Arc<AtomicU64>,
    ui_sched_id: Arc<AtomicU64>,
) -> MetaResult<()> {
    let mut paused = false;

    loop {
        match rx.try_recv() {
            Ok(Control::Stop) | Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            Ok(Control::Pause) => {
                paused = true;
                ui_sched_id.fetch_add(1, Ordering::Relaxed);
            }
            Ok(Control::Resume) => {
                paused = false;
                ui_sched_id.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        match run_once(
            station,
            sender.clone(),
            &rx,
            lag_ms.clone(),
            ui_sched_id.clone(),
            &mut paused,
        ) {
            Ok(()) => {
                // Normal end (server closed the connection). Respect stop; otherwise retry.
                match rx.try_recv() {
                    Ok(Control::Stop) | Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                    Ok(Control::Pause) => {
                        paused = true;
                        ui_sched_id.fetch_add(1, Ordering::Relaxed);
                        thread::sleep(Duration::from_secs(1));
                    }
                    Ok(Control::Resume) => {
                        paused = false;
                        ui_sched_id.fetch_add(1, Ordering::Relaxed);
                        thread::sleep(Duration::from_secs(1));
                    }
                    Err(mpsc::TryRecvError::Empty) => thread::sleep(Duration::from_secs(5)),
                }
            }
            Err(err) => {
                eprintln!("Gateway connection error: {err}, retrying in 5s…");
                match rx.try_recv() {
                    Ok(Control::Stop) | Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                    Ok(Control::Pause) => {
                        paused = true;
                        ui_sched_id.fetch_add(1, Ordering::Relaxed);
                        thread::sleep(Duration::from_secs(1));
                    }
                    Ok(Control::Resume) => {
                        paused = false;
                        ui_sched_id.fetch_add(1, Ordering::Relaxed);
                        thread::sleep(Duration::from_secs(1));
                    }
                    Err(mpsc::TryRecvError::Empty) => thread::sleep(Duration::from_secs(5)),
                }
            }
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
    #[cfg(debug_assertions)]
    println!("[{}] Gateway connected to LISTEN.moe", now_string());

    // Read hello and get heartbeat interval (if any).
    let heartbeat_ms = read_hello_heartbeat(&mut ws)?;
    // Send an immediate heartbeat once after HELLO, then continue on the interval.
    let _ = ws.send(Message::Text(r#"{"op":9}"#.into()));

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
                ui_sched_id.fetch_add(1, Ordering::Relaxed);
                break;
            }
            Ok(Control::Pause) => {
                #[cfg(debug_assertions)]
                println!("[{}] Pausing meta data", now_string());
                *paused = true;
                ui_sched_id.fetch_add(1, Ordering::Relaxed); // invalidate any pending scheduled sends
            }
            Ok(Control::Resume) => {
                #[cfg(debug_assertions)]
                println!("[{}] Resuming meta data", now_string());
                *paused = false;
                ui_sched_id.fetch_add(1, Ordering::Relaxed); // invalidate timers from before pause

                // Snap UI to the track that matches buffered playback time.
                let lag = lag_ms.load(Ordering::Relaxed);
                #[cfg(debug_assertions)]
                if let Some(t) = pick_track_for_playback(&history, lag) {
                    println!("[{}] ui snap: {} - {}", now_string(), t.artist, t.title);
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
                if let Err(err) = ws.send(Message::Text(r#"{"op":9}"#.into())) {
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
            Err(err) => return Err(Box::new(err)),
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
                #[cfg(debug_assertions)]
                println!("[{}] Gateway heartbeat", now_string());
            }
            (OP_DISPATCH, Some(EVENT_TRACK_UPDATE)) => {
                if let Some(info) = parse_track_info(&env.d) {
                    #[cfg(debug_assertions)]
                    println!(
                        "[{}] live track update: {} - {} (duration={})",
                        now_string(),
                        info.artist,
                        info.title,
                        info.duration_secs
                    );
                    history.push_back(info);

                    if !*paused {
                        let lag = lag_ms.load(Ordering::Relaxed);
                        let my_id = ui_sched_id.fetch_add(1, Ordering::Relaxed) + 1;
                        #[cfg(debug_assertions)]
                        println!(
                            "[{}] ui {} scheduled: {} - {} (lag_ms={})",
                            now_string(),
                            my_id,
                            history.back().unwrap().artist,
                            history.back().unwrap().title,
                            lag
                        );
                        // Schedule the *new* track to appear when playback reaches it
                        schedule_ui_switch(
                            sender.clone(),
                            history.back().unwrap().clone(),
                            lag,
                            ui_sched_id.clone(),
                            my_id,
                        );
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
        Err(err) => Err(Box::new(err)),
    }
}

/// Extract artist(s) + title from the gateway payload.
fn parse_track_info(d: &Value) -> Option<TrackInfo> {
    let payload: GatewaySongPayload = serde_json::from_value(d.clone()).ok()?;
    let Song {
        title,
        artists,
        albums,
        duration,
    } = payload.song;

    let start_time_utc = parse_rfc3339_system_time(&payload.start_time)?;
    let duration_secs = duration.unwrap_or(0);

    let title = title.unwrap_or_else(|| "unknown title".to_owned());

    let artist = if artists.is_empty() {
        "Unknown artist".to_owned()
    } else {
        artists
            .iter()
            .filter_map(|a| a.name.as_deref())
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .join(", ")
    };

    let album_cover = albums
        .first()
        .and_then(|album| album.image.as_deref())
        .map(|name| format!("{ALBUM_COVER_BASE}{name}"));

    let artist_image = artists
        .first()
        .and_then(|a| a.image.as_deref())
        .map(|name| format!("{ARTIST_IMAGE_BASE}{name}"));

    Some(TrackInfo {
        artist,
        title,
        album_cover,
        artist_image,
        start_time_utc,
        duration_secs,
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
