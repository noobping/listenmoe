use crate::station::Station;
use crate::ui::UiOptions;
use adw::gtk::gio::{self, prelude::*};

const SHARED_SCHEMA_ID: &str = "io.github.noobping.listenmoe";
const KEY_STATION: &str = "default-station";
const KEY_AUTOPLAY: &str = "autoplay";
const KEY_STOP_INSTEAD_PAUSE: &str = "stop-instead-pause";
const KEY_DISCORD_ENABLED: &str = "discord-enabled";

pub fn settings() -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    if let Some(schema) = source.lookup(SHARED_SCHEMA_ID, true) {
        return Some(gio::Settings::new_full(
            &schema,
            None::<&gio::SettingsBackend>,
            None::<&str>,
        ));
    }
    None
}

pub fn load_ui_options() -> Option<UiOptions> {
    let settings = settings()?;

    let station = match settings.string(KEY_STATION).as_str() {
        "kpop" => Station::Kpop,
        _ => Station::Jpop,
    };

    Some(UiOptions {
        station,
        autoplay: settings.boolean(KEY_AUTOPLAY),
        stop_instead_pause: settings.boolean(KEY_STOP_INSTEAD_PAUSE),
        discord_enabled: settings.boolean(KEY_DISCORD_ENABLED),
    })
}

pub fn save_ui_options(options: UiOptions) -> Result<(), String> {
    let settings = settings().ok_or_else(|| {
        format!(
            "Could not find installed GSettings schema '{SHARED_SCHEMA_ID}'. Preferences were not saved."
        )
    })?;

    settings
        .set_string(KEY_STATION, options.station.name())
        .map_err(|err| format!("Failed to save station preference: {err}"))?;
    settings
        .set_boolean(KEY_AUTOPLAY, options.autoplay)
        .map_err(|err| format!("Failed to save autoplay preference: {err}"))?;
    settings
        .set_boolean(KEY_STOP_INSTEAD_PAUSE, options.stop_instead_pause)
        .map_err(|err| format!("Failed to save stop behavior preference: {err}"))?;
    settings
        .set_boolean(KEY_DISCORD_ENABLED, options.discord_enabled)
        .map_err(|err| format!("Failed to save Discord preference: {err}"))?;

    gio::Settings::sync();
    Ok(())
}
