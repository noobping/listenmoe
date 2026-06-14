use std::cell::RefCell;
use std::error::Error;
#[cfg(feature = "experimental")]
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicU32;
use std::sync::{mpsc, Arc};
use std::thread;
#[cfg(feature = "experimental")]
use std::time::{SystemTime, UNIX_EPOCH};

use crate::station::Station;

mod clock;
pub use clock::PlaybackClock;
#[cfg(feature = "experimental")]
mod store;
mod stream;
mod viz;

type DynError = Box<dyn Error + Send + Sync + 'static>;
type Result<T> = std::result::Result<T, DynError>;

const N_BARS: usize = 48;

#[derive(Debug, Clone, Copy)]
enum Control {
    Stop,
    #[cfg(feature = "experimental")]
    Pause,
    #[cfg(feature = "experimental")]
    Resume,
}

#[derive(Debug)]
enum State {
    Stopped,
    #[cfg(feature = "experimental")]
    Paused {
        tx: mpsc::Sender<Control>,
    },
    Playing {
        tx: mpsc::Sender<Control>,
    },
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
        let was_playing_or_paused = match inner.state {
            State::Playing { .. } => true,
            #[cfg(feature = "experimental")]
            State::Paused { .. } => true,
            State::Stopped => false,
        };
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

    #[cfg(feature = "experimental")]
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
            #[cfg(feature = "experimental")]
            State::Paused { tx } => {
                let _ = tx.send(Control::Resume);
                inner.state = State::Playing { tx: tx.clone() };
                return;
            }
            State::Stopped => {
                let (tx, rx) = mpsc::channel::<Control>();
                let station = inner.station;
                #[cfg(feature = "experimental")]
                let root = timeshift_root(station);

                inner.state = State::Playing { tx: tx.clone() };

                // detached worker thread; will exit on Stop or error
                thread::spawn(move || {
                    #[cfg(feature = "experimental")]
                    let result =
                        stream::run_listenmoe_stream(station, rx, spectrum_bits, clock, root);
                    #[cfg(not(feature = "experimental"))]
                    let result = stream::run_listenmoe_stream(station, rx, spectrum_bits, clock);

                    if let Err(err) = result {
                        eprintln!("stream error: {err}");
                    }
                });
            }
        }
    }

    fn stop_inner(inner: &mut Inner, clock: &Arc<PlaybackClock>) {
        match &inner.state {
            State::Playing { tx } => {
                let _ = tx.send(Control::Stop);
            }
            #[cfg(feature = "experimental")]
            State::Paused { tx } => {
                let _ = tx.send(Control::Stop);
            }
            State::Stopped => {}
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

#[cfg(feature = "experimental")]
fn timeshift_root(station: Station) -> PathBuf {
    let mut root = dirs_next::cache_dir().unwrap_or_else(std::env::temp_dir);
    root.push(listen_cache_namespace());
    root.push("timeshift");
    root.push(station.name());
    root.push(format!("session-{}", unique_session_id()));
    root
}

#[cfg(feature = "experimental")]
fn listen_cache_namespace() -> String {
    std::env::var("LISTENMOE_APP_ID").unwrap_or_else(|_| {
        if cfg!(debug_assertions) {
            "io.github.noobping.listenmoe.Devel".to_string()
        } else {
            "io.github.noobping.listenmoe".to_string()
        }
    })
}

#[cfg(feature = "experimental")]
fn unique_session_id() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(all(test, feature = "experimental"))]
mod tests {
    use std::sync::atomic::AtomicU32;
    use std::sync::{mpsc, Arc};

    use super::{Control, Inner, Listen, PlaybackClock, State};
    use crate::station::Station;

    #[test]
    fn resume_from_paused_preserves_playback_cursor() {
        let (tx, rx) = mpsc::channel();
        let mut inner = Inner {
            station: Station::Jpop,
            state: State::Paused { tx },
        };
        let clock = Arc::new(PlaybackClock::new());
        clock.set_live_head_ms(9_000);
        clock.set_playback_cursor_ms(1_000);
        let spectrum_bits = Arc::new((0..48).map(|_| AtomicU32::new(0)).collect());

        Listen::start_inner(&mut inner, spectrum_bits, clock.clone());

        assert_eq!(clock.playback_cursor_ms(), 1_000);
        assert!(matches!(inner.state, State::Playing { .. }));
        assert!(matches!(
            rx.recv().expect("missing control"),
            Control::Resume
        ));
    }
}
