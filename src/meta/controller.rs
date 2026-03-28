use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{atomic::AtomicU64, Arc};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::listen::PlaybackClock;
use crate::station::Station;
use crate::ui::UiEvent;

use super::gateway::run_meta_loop;
use super::timeline::TimelineStore;

#[derive(Debug)]
pub enum Control {
    Stop,
    Pause,
    Resume,
}

#[derive(Debug)]
enum State {
    Stopped,
    Running { tx: mpsc::Sender<Control> },
}

#[derive(Debug)]
struct Inner {
    station: Station,
    state: State,
    sender: mpsc::Sender<UiEvent>,
    clock: Arc<PlaybackClock>,
    ui_sched_id: Arc<AtomicU64>,
}

#[derive(Debug)]
pub struct Meta {
    inner: RefCell<Inner>,
}

impl Meta {
    pub fn new(
        station: Station,
        sender: mpsc::Sender<UiEvent>,
        clock: Arc<PlaybackClock>,
    ) -> Rc<Self> {
        Rc::new(Self {
            inner: RefCell::new(Inner {
                station,
                state: State::Stopped,
                sender,
                clock,
                ui_sched_id: Arc::new(AtomicU64::new(0)),
            }),
        })
    }

    pub fn set_station(&self, station: Station) {
        let mut inner = self.inner.borrow_mut();
        let was_running = matches!(inner.state, State::Running { .. });
        if was_running {
            Self::stop_inner(&mut inner);
        }
        inner.station = station;
        if was_running {
            Self::start_inner(&mut inner);
        }
    }

    pub fn start(&self) {
        let tx_opt = {
            let inner = self.inner.borrow();
            match &inner.state {
                State::Running { tx } => Some(tx.clone()),
                State::Stopped => None,
            }
        };
        if let Some(tx) = tx_opt {
            let _ = tx.send(Control::Resume);
            return;
        }
        // stopped: actually start thread
        let mut inner = self.inner.borrow_mut();
        Self::start_inner(&mut inner);
    }

    pub fn pause(&self) {
        let inner = self.inner.borrow();
        if let State::Running { tx } = &inner.state {
            let _ = tx.send(Control::Pause);
        }
    }

    pub fn stop(&self) {
        let mut inner = self.inner.borrow_mut();
        Self::stop_inner(&mut inner);
    }

    fn start_inner(inner: &mut Inner) {
        match inner.state {
            State::Running { .. } => return,
            State::Stopped => {
                let (tx, rx) = mpsc::channel::<Control>();
                let station = inner.station;
                let sender = inner.sender.clone();
                let clock = inner.clock.clone();
                let ui_sched_id = inner.ui_sched_id.clone();
                let timeline = Arc::new(TimelineStore::new(timeline_path(station)));
                if let Err(err) = timeline.clear() {
                    eprintln!("Failed to clear metadata timeline: {err}");
                }

                inner.state = State::Running { tx: tx.clone() };

                thread::spawn(move || {
                    if let Err(err) =
                        run_meta_loop(station, sender, rx, clock, ui_sched_id, timeline.clone())
                    {
                        eprintln!("Gateway error in metadata loop: {err}");
                    }
                    if let Err(err) = timeline.clear() {
                        eprintln!("Failed to clear metadata timeline: {err}");
                    }
                });
            }
        }
    }

    fn stop_inner(inner: &mut Inner) {
        if let State::Running { tx } = &inner.state {
            let _ = tx.send(Control::Stop);
        }
        inner.state = State::Stopped;
    }
}

impl Drop for Meta {
    fn drop(&mut self) {
        let mut inner = self.inner.borrow_mut();
        Self::stop_inner(&mut inner);
    }
}

fn timeline_path(station: Station) -> PathBuf {
    let mut root = dirs_next::cache_dir().unwrap_or_else(std::env::temp_dir);
    root.push(std::env::var("LISTENMOE_APP_ID").unwrap_or_else(|_| {
        if cfg!(debug_assertions) {
            "io.github.noobping.listenmoe.Devel".to_string()
        } else {
            "io.github.noobping.listenmoe".to_string()
        }
    }));
    root.push("timeshift");
    root.push(station.name());
    root.push(format!("session-{}", unique_session_id()));
    root.push("timeline.json");
    root
}

fn unique_session_id() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}
