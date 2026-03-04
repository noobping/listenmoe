use adw::glib;
use adw::gtk::{
    self,
    gio::SimpleAction,
    prelude::{ActionMapExt, GtkApplicationExt, GtkWindowExt},
    ApplicationWindow, Button,
};
use adw::{Application, WindowTitle};
use mpris_server::PlaybackStatus;
use std::rc::Rc;
use std::sync::mpsc;

use super::controls::{build_controls, MediaControlEvent, MediaControls};
use crate::listen::Listen;
use crate::meta::Meta;

mod context;
mod menu;
use adw::prelude::AdwDialogExt;
use context::ActionCtx;
use gettextrs::gettext;
pub use menu::populate_menu;

const APP_NAME: &str = "Listen Moe";
#[cfg(debug_assertions)]
const APP_ID: &str = "io.github.noobping.listenmoe_beta";
#[cfg(not(debug_assertions))]
const APP_ID: &str = "io.github.noobping.listenmoe";

type PlaybackSetter = Rc<dyn Fn(PlaybackStatus)>;

fn make_action<F>(name: &str, f: F) -> SimpleAction
where
    F: Fn() + 'static,
{
    let action = SimpleAction::new(name, None);
    action.connect_activate(move |_, _| f());
    action
}

pub(super) fn register_window_action<F>(window: &ApplicationWindow, name: &str, f: F)
where
    F: Fn() + 'static,
{
    window.add_action(&make_action(name, f));
}

pub(super) fn activate_window_action(window: &ApplicationWindow, action: &str) {
    let _ = adw::prelude::WidgetExt::activate_action(window, action, None::<&glib::Variant>);
}

pub fn build_actions(
    window: &ApplicationWindow,
    app: &Application,
    win_title: &WindowTitle,
    play_button: &Button,
    pause_button: &Button,
    radio: &Rc<Listen>,
    meta: &Rc<Meta>,
) -> (
    Option<Rc<MediaControls>>,
    Option<mpsc::Receiver<MediaControlEvent>>,
) {
    let (controls, ctrl_rx) = match build_controls(APP_ID, APP_NAME, APP_ID) {
        Ok((controls, ctrl_rx)) => (Some(controls), Some(ctrl_rx)),
        Err(e) => {
            eprintln!("Media control unavailable: {e}");
            (None, None)
        }
    };

    let set_playback: PlaybackSetter = {
        let controls = controls.clone();
        Rc::new(move |status| {
            if let Some(c) = controls.as_ref() {
                c.set_playback(status);
            }
        })
    };

    let ctx = ActionCtx::new(window, win_title, play_button, pause_button, radio, meta);
    add_transport_actions(window, &ctx, &set_playback);
    add_window_actions(window, &ctx);
    add_accels(app);

    (controls, ctrl_rx)
}

fn add_transport_actions(
    window: &ApplicationWindow,
    ctx: &ActionCtx,
    set_playback: &PlaybackSetter,
) {
    {
        let ctx = ctx.clone();
        let set_playback = set_playback.clone();
        register_window_action(window, "play", move || ctx.play(&*set_playback));
    }

    {
        let ctx = ctx.clone();
        let set_playback = set_playback.clone();
        register_window_action(window, "pause", move || ctx.pause(&*set_playback));
    }

    {
        let ctx = ctx.clone();
        let set_playback = set_playback.clone();
        register_window_action(window, "stop", move || ctx.stop(&*set_playback));
    }
}

fn add_window_actions(window: &ApplicationWindow, ctx: &ActionCtx) {
    {
        let ctx = ctx.clone();
        register_window_action(window, "quit", move || ctx.window.close());
    }

    {
        let ctx = ctx.clone();
        register_window_action(window, "about", move || show_about_dialog(&ctx.window));
    }

    {
        let ctx = ctx.clone();
        register_window_action(window, "toggle", move || ctx.toggle());
    }

    {
        let ctx = ctx.clone();
        register_window_action(window, "copy", move || ctx.copy_current_track());
    }

    {
        let ctx = ctx.clone();
        register_window_action(window, "next_station", move || ctx.next_station());
    }

    {
        let ctx = ctx.clone();
        register_window_action(window, "prev_station", move || ctx.prev_station());
    }
}

fn show_about_dialog(window: &ApplicationWindow) {
    let authors: Vec<_> = env!("CARGO_PKG_AUTHORS").split(':').collect();
    let homepage = option_env!("CARGO_PKG_HOMEPAGE").unwrap_or("");
    let issues = format!("{}/issues", env!("CARGO_PKG_REPOSITORY"));
    let comments =
        gettext("It is time to ditch other radios. Stream and metadata provided by LISTEN.moe.");
    let version = env!("CARGO_PKG_VERSION");
    #[cfg(debug_assertions)]
    let version = format!("{}-beta", version);

    let about = adw::AboutDialog::builder()
        .application_name(APP_NAME)
        .application_icon(APP_ID)
        .version(version)
        .developers(&authors[..])
        .translator_credits(gettext("AI translation (GPT-5.2); reviewed by nobody"))
        .website(homepage)
        .issue_url(issues)
        .support_url(format!("{}discord", homepage))
        .license_type(gtk::License::MitX11)
        .comments(comments)
        .build();
    about.present(Some(window));
}

fn add_accels(app: &Application) {
    const ACCELS: &[(&str, &[&str])] = &[
        ("win.about", &["F1"]),
        ("win.copy", &["<primary>c"]),
        ("win.jpop", &["<primary>j"]),
        ("win.kpop", &["<primary>k"]),
        ("win.quit", &["<primary>q", "Escape"]),
        ("win.prev_station", &["<primary>z", "XF86AudioPrev"]),
        (
            "win.next_station",
            &["<primary>y", "<primary><shift>z", "XF86AudioNext"],
        ),
        (
            "win.toggle",
            &["<primary>p", "space", "Return", "<primary>s"],
        ),
        ("win.play", &["XF86AudioPlay"]),
        ("win.stop", &["XF86AudioStop"]),
        ("win.pause", &["XF86AudioPause"]),
    ];

    for (action, keys) in ACCELS {
        app.set_accels_for_action(action, keys);
    }
}
