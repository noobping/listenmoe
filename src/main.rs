#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod http_source;
mod listen;
mod locale;
#[cfg(debug_assertions)]
mod log;
mod meta;
mod station;
mod ui;

#[cfg(debug_assertions)]
const APP_ID: &str = "io.github.noobping.listenmoe_beta";
#[cfg(not(debug_assertions))]
const APP_ID: &str = "io.github.noobping.listenmoe";
#[cfg(target_os = "windows")]
const RESOURCE_ID: &str = "/io/github/noobping/listenmoe";
#[cfg(target_os = "windows")]
use adw::gtk::{gdk::Display, IconTheme};
use adw::prelude::*;
use adw::Application;
use adw::gtk::gio::ApplicationFlags;

fn parse_ui_options() -> (ui::UiOptions, Vec<String>) {
    let mut options = ui::UiOptions::default();
    let mut passthrough_args = Vec::new();

    let mut args = std::env::args_os();
    if let Some(program) = args.next() {
        passthrough_args.push(program.to_string_lossy().into_owned());
    }

    let mut parse_flags = true;
    for arg in args {
        let arg = arg.to_string_lossy().into_owned();
        if !parse_flags {
            passthrough_args.push(arg);
            continue;
        }

        match arg.as_str() {
            "--" => {
                parse_flags = false;
                passthrough_args.push(arg);
            }
            "-a" | "--autoplay" => {
                options.autoplay = true;
            }
            "-j" | "--jpop" => {
                options.station = station::Station::Jpop;
            }
            "-k" | "--kpop" => {
                options.station = station::Station::Kpop;
            }
            "-s" | "--stop" => {
                options.stop_instead_pause = true;
            }
            "--no-discord" => {
                options.discord_enabled = false;
            }
            _ => {
                passthrough_args.push(arg);
            }
        }
    }

    (options, passthrough_args)
}

fn main() {
    let (ui_options, app_args) = parse_ui_options();
    locale::init_i18n();
    let app_id = std::env::var("LISTENMOE_APP_ID").unwrap_or_else(|_| APP_ID.to_string());
    let app_flags = match std::env::var("LISTENMOE_APP_NON_UNIQUE") {
        Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") => ApplicationFlags::NON_UNIQUE,
        _ => ApplicationFlags::empty(),
    };

    // Register resources compiled into the binary. If this fails, the app cannot find its assets.
    #[cfg(target_os = "windows")]
    adw::gtk::gio::resources_register_include!("compiled.gresource")
        .expect("Failed to register resources");

    // Initialize libadwaita/GTK. This must be called before any UI code.
    adw::init().expect("Failed to initialize libadwaita");

    // Load the icon theme from the embedded resources so that icons resolve correctly even outside a installed environment.
    #[cfg(target_os = "windows")]
    if let Some(display) = Display::default() {
        let theme = IconTheme::for_display(&display);
        theme.add_resource_path(RESOURCE_ID);
    }

    // Create the GTK application. The application ID must be unique and corresponds to the desktop file name.
    let app = Application::builder()
        .application_id(app_id.as_str())
        .flags(app_flags)
        .build();
    app.connect_activate(move |app| ui::build_ui(app, ui_options)); // Build the UI when the application is activated.
    app.run_with_args(&app_args); // Run the application. This function does not return until the last window is closed.
}
