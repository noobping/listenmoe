use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::AtomicU32;
use std::sync::{mpsc, Arc};
use std::thread;

use crate::station::Station;

use super::{stream, PlaybackClock, N_BARS};

#[derive(Debug, Clone, Copy)]
pub(in crate::listen) enum Control {
    Stop,
}

#[derive(Debug)]
enum State {
    Stopped,
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
        let was_playing = matches!(inner.state, State::Playing { .. });
        if was_playing {
            Self::stop_inner(&mut inner, &self.clock);
        }
        inner.station = station;
        if was_playing {
            Self::start_inner(&mut inner, self.spectrum_bits.clone(), self.clock.clone());
        }
    }

    pub fn start(&self) {
        let mut inner = self.inner.borrow_mut();
        Self::start_inner(&mut inner, self.spectrum_bits.clone(), self.clock.clone());
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
                return;
            }
            State::Stopped => {
                let (tx, rx) = mpsc::channel::<Control>();
                let station = inner.station;

                inner.state = State::Playing { tx: tx.clone() };

                thread::spawn(move || {
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
