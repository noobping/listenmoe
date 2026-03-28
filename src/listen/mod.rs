use std::cell::RefCell;
use std::error::Error;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicU32;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::station::Station;

mod clock;
pub use clock::PlaybackClock;
mod store;
pub use store::RETENTION_MS;
mod stream;
mod viz;

type DynError = Box<dyn Error + Send + Sync + 'static>;
type Result<T> = std::result::Result<T, DynError>;

const N_BARS: usize = 48;

#[derive(Debug, Clone, Copy)]
enum Control {
    Stop,
    Pause,
    Resume,
}

#[derive(Debug)]
enum State {
    Stopped,
    Paused { tx: mpsc::Sender<Control> },
    Playing { tx: mpsc::Sender<Control> },
}

#[derive(Debug)]
struct Inner {
    station: Station,
    state: State,
}

#[derive(Debug)]
pub struct Listen {
    inner: RefCell<Inner>,
    clock: Arc<PlaybackClock>,
    spectrum_bits: Arc<Vec<AtomicU32>>,
}

impl Listen {
    pub fn new(station: Station) -> Rc<Self> {
        Rc::new(Self {
            inner: RefCell::new(Inner {
                station,
                state: State::Stopped,
            }),
            clock: Arc::new(PlaybackClock::new()),
            spectrum_bits: Arc::new((0..N_BARS).map(|_| AtomicU32::new(0)).collect()),
        })
    }

    pub fn spectrum_bars(&self) -> Arc<Vec<AtomicU32>> {
        self.spectrum_bits.clone()
    }

    pub fn playback_clock(&self) -> Arc<PlaybackClock> {
        self.clock.clone()
    }

    pub fn get_station(&self) -> Station {
        self.inner.borrow().station
    }

    pub fn set_station(&self, station: Station) {
        let mut inner = self.inner.borrow_mut();
        let was_playing_or_paused =
            matches!(inner.state, State::Playing { .. } | State::Paused { .. });
        if was_playing_or_paused {
            Self::stop_inner(&mut inner, &self.clock);
        }
        inner.station = station;
        if was_playing_or_paused {
            Self::start_inner(&mut inner, self.spectrum_bits.clone(), self.clock.clone());
        }
    }

    pub fn start(&self) {
        let mut inner = self.inner.borrow_mut();
        Self::start_inner(&mut inner, self.spectrum_bits.clone(), self.clock.clone());
    }

    pub fn pause(&self) {
        let mut inner = self.inner.borrow_mut();
        match &inner.state {
            State::Playing { tx } => {
                let _ = tx.send(Control::Pause);
                inner.state = State::Paused { tx: tx.clone() };
            }
            _ => {}
        }
    }

    pub fn stop(&self) {
        let mut inner = self.inner.borrow_mut();
        Self::stop_inner(&mut inner, &self.clock);
    }

    fn start_inner(
        inner: &mut Inner,
        spectrum_bits: Arc<Vec<AtomicU32>>,
        clock: Arc<PlaybackClock>,
    ) {
        match &inner.state {
            State::Playing { .. } => {
                // already playing
                return;
            }
            State::Paused { tx } => {
                let _ = tx.send(Control::Resume);
                inner.state = State::Playing { tx: tx.clone() };
                return;
            }
            State::Stopped => {
                let (tx, rx) = mpsc::channel::<Control>();
                let station = inner.station;
                let root = timeshift_root(station);

                inner.state = State::Playing { tx: tx.clone() };

                // detached worker thread; will exit on Stop or error
                thread::spawn(move || {
                    if let Err(err) =
                        stream::run_listenmoe_stream(station, rx, spectrum_bits, clock, root)
                    {
                        eprintln!("stream error: {err}");
                    }
                });
            }
        }
    }

    fn stop_inner(inner: &mut Inner, clock: &Arc<PlaybackClock>) {
        if let State::Playing { tx } | State::Paused { tx } = &inner.state {
            let _ = tx.send(Control::Stop);
        }
        inner.state = State::Stopped;
        clock.reset();
    }
}

impl Drop for Listen {
    fn drop(&mut self) {
        let mut inner = self.inner.borrow_mut();
        Self::stop_inner(&mut inner, &self.clock);
    }
}

fn timeshift_root(station: Station) -> PathBuf {
    let mut root = dirs_next::cache_dir().unwrap_or_else(std::env::temp_dir);
    root.push(listen_cache_namespace());
    root.push("timeshift");
    root.push(station.name());
    root.push(format!("session-{}", unique_session_id()));
    root
}

fn listen_cache_namespace() -> String {
    std::env::var("LISTENMOE_APP_ID").unwrap_or_else(|_| {
        if cfg!(debug_assertions) {
            "io.github.noobping.listenmoe.Devel".to_string()
        } else {
            "io.github.noobping.listenmoe".to_string()
        }
    })
}

fn unique_session_id() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}
