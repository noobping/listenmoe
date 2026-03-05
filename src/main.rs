#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod http_source;
mod listen;
mod locale;
mod log;
mod meta;
mod preferences;
mod station;
mod ui;

#[cfg(debug_assertions)]
const APP_ID: &str = "io.github.noobping.listenmoe_beta";
#[cfg(not(debug_assertions))]
const APP_ID: &str = "io.github.noobping.listenmoe";
#[cfg(target_os = "windows")]
const RESOURCE_ID: &str = "/io/github/noobping/listenmoe";
use adw::gtk::gio::ApplicationFlags;
#[cfg(target_os = "windows")]
use adw::gtk::{gdk::Display, IconTheme};
use adw::prelude::*;
use adw::Application;
use std::process::ExitCode;

const APP_NAME: &str = env!("CARGO_PKG_NAME");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

enum CliAction {
    Run {
        ui_options: ui::UiOptions,
        app_args: Vec<String>,
        verbose: bool,
        save_preferences: bool,
    },
    Help,
    Version,
}

fn help_text() -> String {
    format!(
        "{name} {version}
{description}

Usage:
  {name} [OPTIONS] [-- GTK_ARGS...]

Short flags can be combined (for example: -ja or -avk)

Options:
  -a, --autoplay    Start playing automatically on launch
  -j, --jpop        Use J-POP as default station
  -k, --kpop        Use K-POP as default station
  -p, --preferences Save current startup flags as defaults
  -s, --stop        Use stop behavior instead of pause
      --no-discord  Disable Discord Rich Presence at runtime
  -v, --verbose     Print extra startup diagnostics
  -h, --help        Show this help and exit
      --version     Show version and exit",
        name = APP_NAME,
        version = APP_VERSION,
        description = env!("CARGO_PKG_DESCRIPTION"),
    )
}

fn parse_cli(default_options: ui::UiOptions) -> Result<CliAction, String> {
    let mut options = default_options;
    let mut passthrough_args = Vec::new();
    let mut verbose = false;
    let mut save_preferences = false;

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
            _ if arg.starts_with('-') && !arg.starts_with("--") && arg.len() > 2 => {
                let cluster = &arg[1..];
                let recognized_cluster = cluster.chars().all(|short_flag| {
                    matches!(short_flag, 'a' | 'j' | 'k' | 'p' | 's' | 'v' | 'h')
                });

                if !recognized_cluster {
                    passthrough_args.push(arg);
                    continue;
                }

                for short_flag in cluster.chars() {
                    match short_flag {
                        'a' => {
                            options.autoplay = true;
                        }
                        'j' => {
                            options.station = station::Station::Jpop;
                        }
                        'k' => {
                            options.station = station::Station::Kpop;
                        }
                        's' => {
                            options.stop_instead_pause = true;
                        }
                        'p' => {
                            save_preferences = true;
                        }
                        'v' => {
                            verbose = true;
                        }
                        'h' => {
                            return Ok(CliAction::Help);
                        }
                        _ => unreachable!("cluster was pre-validated"),
                    }
                }
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
            "-p" | "--preferences" => {
                save_preferences = true;
            }
            "--no-discord" => {
                options.discord_enabled = false;
            }
            "-v" | "--verbose" => {
                verbose = true;
            }
            "-h" | "--help" => {
                return Ok(CliAction::Help);
            }
            "--version" => {
                return Ok(CliAction::Version);
            }
            _ => {
                passthrough_args.push(arg);
            }
        }
    }

    Ok(CliAction::Run {
        ui_options: options,
        app_args: passthrough_args,
        verbose,
        save_preferences,
    })
}

fn run() -> Result<(), String> {
    let app_id = std::env::var("LISTENMOE_APP_ID").unwrap_or_else(|_| APP_ID.to_string());
    let default_ui_options = preferences::load_ui_options(app_id.as_str()).unwrap_or_default();

    let (ui_options, app_args, verbose, save_preferences) = match parse_cli(default_ui_options)? {
        CliAction::Help => {
            println!("{}", help_text());
            return Ok(());
        }
        CliAction::Version => {
            println!("{APP_NAME} {APP_VERSION}");
            return Ok(());
        }
        CliAction::Run {
            ui_options,
            app_args,
            verbose,
            save_preferences,
        } => (ui_options, app_args, verbose, save_preferences),
    };

    log::set_verbose(verbose);

    if save_preferences {
        preferences::save_ui_options(app_id.as_str(), ui_options)?;
    }

    if log::is_verbose() {
        println!(
            "Starting {APP_NAME} {APP_VERSION} with station={:?}, autoplay={}, stop_instead_pause={}, discord_enabled={}",
            ui_options.station,
            ui_options.autoplay,
            ui_options.stop_instead_pause,
            ui_options.discord_enabled
        );
    }

    locale::init_i18n();
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

    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}
