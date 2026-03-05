use crate::station::Station;
use crate::ui::UiOptions;
use adw::gtk::gio::{self, prelude::*};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

const SHARED_SCHEMA_ID: &str = "io.github.noobping.listenmoe";
const KEY_STATION: &str = "default-station";
const KEY_AUTOPLAY: &str = "autoplay";
const KEY_STOP_INSTEAD_PAUSE: &str = "stop-instead-pause";
const KEY_DISCORD_ENABLED: &str = "discord-enabled";
const FALLBACK_CONFIG_FILE: &str = "preferences.json";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct StoredUiOptions {
    station: StoredStation,
    autoplay: bool,
    stop_instead_pause: bool,
    discord_enabled: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum StoredStation {
    Jpop,
    Kpop,
}

impl Default for StoredUiOptions {
    fn default() -> Self {
        Self::from(UiOptions::default())
    }
}

impl From<UiOptions> for StoredUiOptions {
    fn from(options: UiOptions) -> Self {
        Self {
            station: match options.station {
                Station::Kpop => StoredStation::Kpop,
                Station::Jpop => StoredStation::Jpop,
            },
            autoplay: options.autoplay,
            stop_instead_pause: options.stop_instead_pause,
            discord_enabled: options.discord_enabled,
        }
    }
}

impl From<StoredUiOptions> for UiOptions {
    fn from(options: StoredUiOptions) -> Self {
        Self {
            station: match options.station {
                StoredStation::Kpop => Station::Kpop,
                StoredStation::Jpop => Station::Jpop,
            },
            autoplay: options.autoplay,
            stop_instead_pause: options.stop_instead_pause,
            discord_enabled: options.discord_enabled,
        }
    }
}

fn fallback_config_path() -> Option<PathBuf> {
    let app_id = std::env::var("LISTENMOE_APP_ID").unwrap_or_else(|_| SHARED_SCHEMA_ID.to_string());
    let mut path = dirs_next::config_dir()?;
    path.push(app_id);
    path.push(FALLBACK_CONFIG_FILE);
    Some(path)
}

fn settings() -> Option<gio::Settings> {
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
    if let Some(settings) = settings() {
        let station = match settings.string(KEY_STATION).as_str() {
            "kpop" => Station::Kpop,
            _ => Station::Jpop,
        };

        return Some(UiOptions {
            station,
            autoplay: settings.boolean(KEY_AUTOPLAY),
            stop_instead_pause: settings.boolean(KEY_STOP_INSTEAD_PAUSE),
            discord_enabled: settings.boolean(KEY_DISCORD_ENABLED),
        });
    }

    let path = fallback_config_path()?;
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            let defaults = UiOptions::default();
            let _ = save_ui_options(defaults);
            return Some(defaults);
        }
        Err(_) => return None,
    };
    let stored: StoredUiOptions = serde_json::from_str(&raw).ok()?;
    Some(stored.into())
}

pub fn save_ui_options(options: UiOptions) -> Result<(), String> {
    if let Some(settings) = settings() {
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
        return Ok(());
    }

    let path = fallback_config_path()
        .ok_or_else(|| "Could not resolve config directory for preferences".to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("Failed to create preferences directory: {err}"))?;
    }

    let serialized = serde_json::to_string_pretty(&StoredUiOptions::from(options))
        .map_err(|err| format!("Failed to serialize preferences: {err}"))?;
    fs::write(&path, serialized).map_err(|err| {
        format!(
            "Failed to write fallback preferences file '{}': {err}",
            path.display()
        )
    })?;

    Ok(())
}
