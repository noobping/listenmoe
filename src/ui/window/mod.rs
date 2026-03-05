mod layout;
mod loops;
mod state;

use crate::listen::Listen;
use crate::meta::{Meta, TrackInfo};
use crate::station::Station;

use adw::prelude::GtkWindowExt;
use adw::Application;
use std::{cell::RefCell, rc::Rc, sync::mpsc};

use super::actions;
use layout::WindowLayout;
use loops::UiUpdateLoopCtx;
use state::{CoverFetchResult, MetadataSetter, SharedTrack};

const APP_NAME: &str = "Listen Moe";

#[derive(Debug, Clone, Copy)]
pub struct UiOptions {
    pub station: Station,
    pub autoplay: bool,
    pub stop_instead_pause: bool,
    pub discord_enabled: bool,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            station: Station::Jpop,
            autoplay: false,
            stop_instead_pause: false,
            discord_enabled: true,
        }
    }
}

pub fn build_ui(app: &Application, options: UiOptions) {
    let station = options.station;
    let radio = Listen::new(station);
    let spectrum_bits = radio.spectrum_bars();

    let (track_tx, track_rx) = mpsc::channel::<TrackInfo>();
    let meta = Meta::new(station, track_tx, radio.lag_ms());
    let (cover_tx, cover_rx) = mpsc::channel::<CoverFetchResult>();
    let current_track: SharedTrack = Rc::new(RefCell::new(None));

    let WindowLayout {
        window,
        win_title,
        play_button,
        pause_button,
        menu,
        art_picture,
        art_popover,
        style_manager,
        css_provider,
        viz,
        viz_handle,
    } = layout::build_window_layout(app, options.stop_instead_pause);

    let (controls, ctrl_rx) = actions::build_actions(
        &window,
        app,
        &win_title,
        &play_button,
        &pause_button,
        &radio,
        &meta,
        &current_track,
        options.stop_instead_pause,
    );

    actions::populate_menu(&window, &play_button, &menu, &radio, &meta);

    let metadata_setter: MetadataSetter = {
        let controls = controls.clone();
        Rc::new(move |title: &str, artist: &str, art_url: Option<&str>| {
            if let Some(c) = controls.as_ref() {
                c.set_metadata(title, artist, APP_NAME, art_url);
            }
        })
    };

    loops::spawn_ui_update_loop(UiUpdateLoopCtx {
        window: window.clone(),
        win_title: win_title.clone(),
        pause_button: pause_button.clone(),
        art_picture,
        art_popover,
        style_manager,
        css_provider,
        track_rx,
        cover_tx,
        cover_rx,
        ctrl_rx,
        current_track,
        metadata_setter,
        discord_enabled: options.discord_enabled,
    });

    loops::spawn_viz_loop(viz, viz_handle, spectrum_bits);

    window.present();
    if options.autoplay {
        actions::activate_window_action(&window, "win.play");
    }
}
