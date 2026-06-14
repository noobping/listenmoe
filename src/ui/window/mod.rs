mod layout;
mod loops;
mod state;

use crate::listen::Listen;
use crate::meta::Meta;
use crate::station::Station;

#[cfg(target_os = "windows")]
use adw::prelude::ApplicationExt;
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
    #[cfg(feature = "experimental")]
    pub pause_resume_enabled: bool,
    pub discord_enabled: bool,
}

impl UiOptions {
    pub fn pause_resume_enabled(&self) -> bool {
        #[cfg(feature = "experimental")]
        {
            self.pause_resume_enabled
        }
        #[cfg(not(feature = "experimental"))]
        {
            false
        }
    }

    #[cfg(feature = "experimental")]
    pub fn set_pause_resume_enabled(&mut self, enabled: bool) {
        self.pause_resume_enabled = enabled;
    }
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            station: Station::Jpop,
            autoplay: false,
            #[cfg(feature = "experimental")]
            pause_resume_enabled: false,
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
        normal_title,
        playback_playing,
        update_active,
        update_title_override,
        play_button,
        pause_button,
        #[cfg(target_os = "windows")]
        update_button,
        #[cfg(target_os = "windows")]
        update_progress_area,
        #[cfg(target_os = "windows")]
        update_progress,
        menu,
        art_picture,
        art_popover,
        style_manager,
        css_provider,
        viz,
        viz_handle,
    } = layout::build_window_layout(app, options.pause_resume_enabled());

    let (controls, ctrl_rx) = actions::build_actions(
        &window,
        app,
        &win_title,
        &play_button,
        &pause_button,
        &playback_playing,
        &update_active,
        &update_title_override,
        &normal_title,
        &radio,
        &meta,
        &ui_tx,
        &current_track,
        options.pause_resume_enabled(),
    );

    #[cfg(target_os = "windows")]
    let updater: Option<crate::updater::UpdaterController> = {
        let updater = crate::updater::register_window(
            app,
            &window,
            crate::updater::UpdateUi {
                win_title: win_title.clone(),
                normal_title: normal_title.clone(),
                playback_playing: playback_playing.clone(),
                update_active: update_active.clone(),
                update_title_override: update_title_override.clone(),
                play_button: play_button.clone(),
                pause_button: pause_button.clone(),
                update_button: update_button.clone(),
                update_progress_area: update_progress_area.clone(),
                update_progress: update_progress.clone(),
            },
        );

        if let Some(updater) = updater.clone() {
            app.connect_shutdown(move |_| updater.shutdown());
        }

        updater
    };

    actions::populate_menu(&window, &playback_playing, &menu, &radio, &meta);

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
        normal_title,
        playback_playing,
        update_title_override,
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
    #[cfg(target_os = "windows")]
    if let Some(updater) = updater {
        updater.after_window_presented();
    }
    if options.autoplay {
        actions::activate_window_action(&window, "win.play");
    }
}

#[cfg(test)]
mod tests {
    use super::UiOptions;

    #[test]
    fn defaults_to_stop_behavior() {
        assert!(!UiOptions::default().pause_resume_enabled());
    }
}
