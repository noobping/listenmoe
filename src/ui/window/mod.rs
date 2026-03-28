mod layout;
mod loops;
mod state;

use crate::listen::Listen;
use crate::meta::Meta;
use crate::station::Station;

use adw::prelude::GtkWindowExt;
use adw::Application;
use std::{cell::RefCell, rc::Rc, sync::mpsc};

use super::actions;
use super::controls::NowPlaying;
use layout::WindowLayout;
use loops::UiUpdateLoopCtx;
use state::{CoverFetchResult, MetadataSetter, SharedTrack};
pub use state::{UiEvent, UiResetReason};

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

    let (ui_tx, ui_rx) = mpsc::channel::<UiEvent>();
    let meta = Meta::new(station, ui_tx.clone(), radio.playback_clock());
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
        &ui_tx,
        &current_track,
        options.stop_instead_pause,
    );

    actions::populate_menu(&window, &play_button, &menu, &radio, &meta);

    let metadata_setter: MetadataSetter = {
        let controls = controls.clone();
        Rc::new(move |now_playing: Option<NowPlaying>| {
            if let Some(c) = controls.as_ref() {
                c.set_metadata(now_playing);
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
        ui_rx,
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
