use std::cell::RefCell;
use std::error::Error;
use std::rc::Rc;
use std::sync::atomic::AtomicU32;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc,
};
use std::thread;
use std::time::Instant;

use crate::station::Station;

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
    lag_ms: Arc<AtomicU64>,
    pause_started: RefCell<Option<Instant>>,
    spectrum_bits: Arc<Vec<AtomicU32>>,
}

impl Listen {
    pub fn new(station: Station) -> Rc<Self> {
        Rc::new(Self {
            inner: RefCell::new(Inner {
                station,
                state: State::Stopped,
            }),
            lag_ms: Arc::new(AtomicU64::new(0)),
            pause_started: RefCell::new(None),
            spectrum_bits: Arc::new((0..N_BARS).map(|_| AtomicU32::new(0)).collect()),
        })
    }

    pub fn spectrum_bars(&self) -> Arc<Vec<AtomicU32>> {
        self.spectrum_bits.clone()
    }

    pub fn lag_ms(&self) -> Arc<AtomicU64> {
        self.lag_ms.clone()
    }

    pub fn get_station(&self) -> Station {
        self.inner.borrow().station
    }

    pub fn set_station(&self, station: Station) {
        let mut inner = self.inner.borrow_mut();
        let was_playing_or_paused =
            matches!(inner.state, State::Playing { .. } | State::Paused { .. });
        if was_playing_or_paused {
            Self::stop_inner(&mut inner);
        }
        inner.station = station;
        if was_playing_or_paused {
            Self::start_inner(&mut inner, self.spectrum_bits.clone());
        }
    }

    pub fn start(&self) {
        if matches!(self.inner.borrow().state, State::Paused { .. }) {
            if let Some(t0) = self.pause_started.borrow_mut().take() {
                let add = t0.elapsed().as_millis() as u64;
                self.lag_ms.fetch_add(add, Ordering::Relaxed);
            }
        }
        let mut inner = self.inner.borrow_mut();
        Self::start_inner(&mut inner, self.spectrum_bits.clone());
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
        *self.pause_started.borrow_mut() = Some(Instant::now());
    }

    pub fn stop(&self) {
        let mut inner = self.inner.borrow_mut();
        Self::stop_inner(&mut inner);
    }

    fn start_inner(inner: &mut Inner, spectrum_bits: Arc<Vec<AtomicU32>>) {
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

                inner.state = State::Playing { tx: tx.clone() };

                // detached worker thread; will exit on Stop or error
                thread::spawn(move || {
                    if let Err(err) = stream::run_listenmoe_stream(station, rx, spectrum_bits) {
                        eprintln!("stream error: {err}");
                    }
                });
            }
        }
    }

    fn stop_inner(inner: &mut Inner) {
        if let State::Playing { tx } | State::Paused { tx } = &inner.state {
            let _ = tx.send(Control::Stop);
        }
        inner.state = State::Stopped;
    }
}

impl Drop for Listen {
    fn drop(&mut self) {
        let mut inner = self.inner.borrow_mut();
        Self::stop_inner(&mut inner);
    }
}
