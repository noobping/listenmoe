use std::io::{Read, Write};
use std::sync::mpsc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
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
use crate::meta::track::TrackInfo;
use crate::station::Station;
use crate::ui::UiEvent;

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
    rx: &mpsc::Receiver<Control>,
    paused_flag: Arc<AtomicBool>,
    clock: Arc<PlaybackClock>,
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
            Ok(Control::Stop) | Err(mpsc::TryRecvError::Disconnected) => break,
            Ok(Control::Pause) => {
                debug_gateway!("Pausing meta data");
                *paused = true;
                paused_flag.store(true, Ordering::Relaxed);
            }
            Ok(Control::Resume) => {
                debug_gateway!("Resuming meta data");
                *paused = false;
                paused_flag.store(false, Ordering::Relaxed);
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

                    let mut tracks = batch.history;
                    tracks.push(batch.current);
                    timeline.insert_tracks(tracks)?;
                    let retention_floor = clock.live_head_ms().saturating_sub(RETENTION_MS);
                    let _ = timeline.prune_before(retention_floor)?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn current_ui_cursor_ms(clock: &PlaybackClock) -> Option<u64> {
    match clock.playback_cursor_ms() {
        0 => {
            let live_head_ms = clock.live_head_ms();
            (live_head_ms != 0).then_some(live_head_ms)
        }
        cursor_ms => Some(cursor_ms),
    }
}

pub(super) fn sync_ui_track(
    sender: &mpsc::Sender<UiEvent>,
    timeline: &TimelineStore,
    clock: &PlaybackClock,
    last_ui_track: &mut Option<TrackInfo>,
) {
    let Some(cursor_ms) = current_ui_cursor_ms(clock) else {
        return;
    };
    let Some(track) = timeline.track_for_cursor(cursor_ms) else {
        *last_ui_track = None;
        return;
    };

    if last_ui_track.as_ref() == Some(&track) {
        return;
    }

    debug_gateway!("ui sync: {} - {}", track.artist, track.title);
    let _ = sender.send(UiEvent::TrackChanged(track.clone()));
    *last_ui_track = Some(track);
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

#[cfg(test)]
mod tests {
    use super::sync_ui_track;
    use crate::listen::PlaybackClock;
    use crate::meta::timeline::TimelineStore;
    use crate::meta::track::TrackInfo;
    use crate::ui::UiEvent;
    use std::path::PathBuf;
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn temp_file(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("listenmoe-{name}-{unique}.json"))
    }

    fn track(title: &str, start_time_ms: u64, duration_secs: u32) -> TrackInfo {
        TrackInfo {
            artist: "artist".into(),
            title: title.into(),
            album: "album".into(),
            album_cover: None,
            artist_image: None,
            start_time_ms,
            duration_secs,
        }
    }

    #[test]
    fn sync_ui_track_follows_playback_cursor() {
        let path = temp_file("meta-schedule");
        let timeline = Arc::new(TimelineStore::new(path.clone()));
        timeline
            .insert_tracks([track("current", 0, 1), track("next", 1_000, 1)])
            .expect("insert failed");

        let clock = Arc::new(PlaybackClock::new());
        clock.set_playback_cursor_ms(500);

        let (tx, rx) = mpsc::channel();
        let mut last_ui_track = None;
        sync_ui_track(&tx, &timeline, &clock, &mut last_ui_track);

        let event = rx
            .recv_timeout(Duration::from_millis(50))
            .expect("missing current track");
        match event {
            UiEvent::TrackChanged(track) => assert_eq!(track.title, "current"),
            other => panic!("unexpected event: {other:?}"),
        }

        sync_ui_track(&tx, &timeline, &clock, &mut last_ui_track);
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());

        clock.set_playback_cursor_ms(1_000);
        sync_ui_track(&tx, &timeline, &clock, &mut last_ui_track);
        let event = rx
            .recv_timeout(Duration::from_millis(50))
            .expect("missing track change after cursor advance");

        match event {
            UiEvent::TrackChanged(track) => assert_eq!(track.title, "next"),
            other => panic!("unexpected event: {other:?}"),
        }

        let _ = std::fs::remove_file(path);
    }
}
