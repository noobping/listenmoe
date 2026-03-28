use adw::glib;
use adw::gtk::{
    self,
    gio::SimpleAction,
    prelude::{ActionMapExt, GtkApplicationExt, GtkWindowExt},
    ApplicationWindow, Button,
};
use adw::{Application, WindowTitle};
use mpris_server::PlaybackStatus;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;

use super::controls::{build_controls, MediaControlEvent, MediaControls};
use crate::listen::Listen;
use crate::meta::Meta;
use crate::ui::UiEvent;

mod context;
mod menu;
mod preferences;
mod shortcuts;
use adw::prelude::AdwDialogExt;
use context::ActionCtx;
use gettextrs::gettext;
pub use menu::populate_menu;
use preferences::show_preferences_window;
use shortcuts::install_shortcuts_overlay;

const APP_NAME: &str = "Listen Moe";
#[cfg(debug_assertions)]
const APP_ID: &str = "io.github.noobping.listenmoe.Devel";
#[cfg(not(debug_assertions))]
const APP_ID: &str = "io.github.noobping.listenmoe";

type PlaybackSetter = Rc<dyn Fn(PlaybackStatus)>;
type TransportAction = fn(&ActionCtx, &dyn Fn(PlaybackStatus));
type CtxAction = fn(&ActionCtx);

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
    ui_tx: &mpsc::Sender<UiEvent>,
    current_track: &Rc<RefCell<Option<(String, String)>>>,
    stop_instead_pause: bool,
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

    let ctx = ActionCtx::new(
        window,
        win_title,
        play_button,
        pause_button,
        radio,
        meta,
        ui_tx,
        current_track,
        stop_instead_pause,
    );
    add_transport_actions(window, &ctx, &set_playback);
    add_window_actions(window, &ctx);
    install_shortcuts_overlay(window);
    add_accels(app);

    (controls, ctrl_rx)
}

fn add_transport_actions(
    window: &ApplicationWindow,
    ctx: &ActionCtx,
    set_playback: &PlaybackSetter,
) {
    for (name, handler) in [
        ("play", ActionCtx::play as TransportAction),
        ("pause", ActionCtx::pause as TransportAction),
        ("stop", ActionCtx::stop as TransportAction),
    ] {
        let ctx = ctx.clone();
        let set_playback = set_playback.clone();
        register_window_action(window, name, move || handler(&ctx, &*set_playback));
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
        register_window_action(window, "preferences", move || {
            show_preferences_window(&ctx.window)
        });
    }

    for (name, handler) in [
        ("toggle", ActionCtx::toggle as CtxAction),
        ("copy", ActionCtx::copy_current_track as CtxAction),
        ("next_station", ActionCtx::next_station as CtxAction),
        ("prev_station", ActionCtx::prev_station as CtxAction),
    ] {
        let ctx = ctx.clone();
        register_window_action(window, name, move || handler(&ctx));
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
        ("win.about", &[]),
        ("win.show-help-overlay", &["F1", "<primary>question"]),
        ("win.preferences", &["<primary>comma"]),
        ("win.copy", &["<primary>c"]),
        ("win.jpop", &["<primary>j"]),
        ("win.kpop", &["<primary>k"]),
        ("win.quit", &["<primary>q"]),
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
