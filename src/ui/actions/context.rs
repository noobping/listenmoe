use adw::gtk::{gdk::Display, prelude::WidgetExt, ApplicationWindow, Button};
use adw::prelude::DisplayExt;
use adw::WindowTitle;
use gettextrs::gettext;
use mpris_server::PlaybackStatus;
use std::cell::RefCell;
use std::rc::Rc;

use crate::listen::Listen;
use crate::meta::Meta;
use crate::station::Station;

use super::activate_window_action;

const APP_NAME: &str = "Listen Moe";

#[derive(Clone)]
pub(super) struct ActionCtx {
    pub(super) window: ApplicationWindow,
    win_title: WindowTitle,
    play_button: Button,
    pause_button: Button,
    radio: Rc<Listen>,
    meta: Rc<Meta>,
    current_track: Rc<RefCell<Option<(String, String)>>>,
    stop_instead_pause: bool,
}

impl ActionCtx {
    pub(super) fn new(
        window: &ApplicationWindow,
        win_title: &WindowTitle,
        play_button: &Button,
        pause_button: &Button,
        radio: &Rc<Listen>,
        meta: &Rc<Meta>,
        current_track: &Rc<RefCell<Option<(String, String)>>>,
        stop_instead_pause: bool,
    ) -> Self {
        Self {
            window: window.clone(),
            win_title: win_title.clone(),
            play_button: play_button.clone(),
            pause_button: pause_button.clone(),
            radio: radio.clone(),
            meta: meta.clone(),
            current_track: current_track.clone(),
            stop_instead_pause,
        }
    }

    fn set_idle_ui(&self) {
        self.pause_button.set_visible(false);
        self.play_button.set_visible(true);
        self.win_title.set_title(APP_NAME);
        self.win_title
            .set_subtitle(&gettext("J-POP and K-POP radio"));
        *self.current_track.borrow_mut() = None;
    }

    pub(super) fn play(&self, set_playback: &dyn Fn(PlaybackStatus)) {
        self.win_title.set_title(APP_NAME);
        self.win_title.set_subtitle("Connecting...");
        *self.current_track.borrow_mut() = None;
        self.meta.start();
        self.radio.start();
        self.play_button.set_visible(false);
        self.pause_button.set_visible(true);
        set_playback(PlaybackStatus::Playing);
    }

    pub(super) fn pause(&self, set_playback: &dyn Fn(PlaybackStatus)) {
        if self.stop_instead_pause {
            self.stop(set_playback);
            return;
        }
        self.meta.pause();
        self.radio.pause();
        self.set_idle_ui();
        set_playback(PlaybackStatus::Paused);
    }

    pub(super) fn stop(&self, set_playback: &dyn Fn(PlaybackStatus)) {
        self.meta.stop();
        self.radio.stop();
        self.set_idle_ui();
        set_playback(PlaybackStatus::Stopped);
    }

    pub(super) fn toggle(&self) {
        if self.play_button.is_visible() {
            activate_window_action(&self.window, "win.play");
        } else if self.pause_button.is_visible() {
            if self.stop_instead_pause {
                activate_window_action(&self.window, "win.stop");
            } else {
                activate_window_action(&self.window, "win.pause");
            }
        }
    }

    pub(super) fn copy_current_track(&self) {
        let text = {
            let track = self.current_track.borrow();
            let Some((artist, title)) = track.as_ref() else {
                return;
            };

            match (artist.is_empty(), title.is_empty()) {
                (true, true) => return,
                (true, false) => title.clone(),
                (false, true) => artist.clone(),
                (false, false) => format!("{artist}, {title}"),
            }
        };

        if let Some(display) = Display::default() {
            display.clipboard().set_text(&text);
        }
    }

    pub(super) fn next_station(&self) {
        if self.play_button.is_visible() {
            activate_window_action(&self.window, "win.play");
            return;
        }

        let next = other_station(self.radio.get_station());
        self.radio.set_station(next);
        self.meta.set_station(next);
    }

    pub(super) fn prev_station(&self) {
        if self.play_button.is_visible() {
            return;
        }

        let prev = other_station(self.radio.get_station());
        self.radio.set_station(prev);
        self.meta.set_station(prev);
    }
}

fn other_station(station: Station) -> Station {
    match station {
        Station::Jpop => Station::Kpop,
        Station::Kpop => Station::Jpop,
    }
}
