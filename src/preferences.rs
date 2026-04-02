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
const KEY_PAUSE_RESUME_ENABLED: &str = "pause-resume-enabled";
const LEGACY_KEY_STOP_INSTEAD_PAUSE: &str = "stop-instead-pause";
const KEY_DISCORD_ENABLED: &str = "discord-enabled";
const FALLBACK_CONFIG_FILE: &str = "preferences.json";

#[derive(Debug, Clone, Copy, Serialize)]
struct StoredUiOptions {
    station: StoredStation,
    autoplay: bool,
    pause_resume_enabled: bool,
    stop_instead_pause: bool,
    discord_enabled: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct StoredUiOptionsOnDisk {
    #[serde(default)]
    station: Option<StoredStation>,
    #[serde(default)]
    autoplay: Option<bool>,
    #[serde(default)]
    pause_resume_enabled: Option<bool>,
    #[serde(default)]
    stop_instead_pause: Option<bool>,
    #[serde(default)]
    discord_enabled: Option<bool>,
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
            pause_resume_enabled: options.pause_resume_enabled,
            stop_instead_pause: !options.pause_resume_enabled,
            discord_enabled: options.discord_enabled,
        }
    }
}

impl StoredUiOptionsOnDisk {
    fn into_ui_options(self) -> UiOptions {
        let defaults = UiOptions::default();
        UiOptions {
            station: match self.station.unwrap_or(match defaults.station {
                Station::Kpop => StoredStation::Kpop,
                Station::Jpop => StoredStation::Jpop,
            }) {
                StoredStation::Kpop => Station::Kpop,
                StoredStation::Jpop => Station::Jpop,
            },
            autoplay: self.autoplay.unwrap_or(defaults.autoplay),
            pause_resume_enabled: resolve_pause_resume_enabled(
                self.pause_resume_enabled,
                self.stop_instead_pause,
                defaults.pause_resume_enabled,
            ),
            discord_enabled: self.discord_enabled.unwrap_or(defaults.discord_enabled),
        }
    }

    fn migrated_from_legacy_stop_flag(&self) -> bool {
        self.pause_resume_enabled.is_none() && self.stop_instead_pause.is_some()
    }
}

fn resolve_pause_resume_enabled(
    pause_resume_enabled: Option<bool>,
    legacy_stop_instead_pause: Option<bool>,
    default: bool,
) -> bool {
    pause_resume_enabled.unwrap_or_else(|| {
        legacy_stop_instead_pause
            .map(|value| !value)
            .unwrap_or(default)
    })
}

fn load_pause_resume_enabled_from_settings(settings: &gio::Settings) -> (bool, bool) {
    let pause_resume_enabled = settings
        .user_value(KEY_PAUSE_RESUME_ENABLED)
        .map(|_| settings.boolean(KEY_PAUSE_RESUME_ENABLED));
    let legacy_stop_instead_pause = settings
        .user_value(LEGACY_KEY_STOP_INSTEAD_PAUSE)
        .map(|_| settings.boolean(LEGACY_KEY_STOP_INSTEAD_PAUSE));
    let migrated_from_legacy =
        pause_resume_enabled.is_none() && legacy_stop_instead_pause.is_some();

    (
        resolve_pause_resume_enabled(
            pause_resume_enabled,
            legacy_stop_instead_pause,
            UiOptions::default().pause_resume_enabled,
        ),
        migrated_from_legacy,
    )
}

fn migrate_legacy_gsettings_pause_resume_preference(
    settings: &gio::Settings,
    pause_resume_enabled: bool,
) {
    if settings
        .set_boolean(KEY_PAUSE_RESUME_ENABLED, pause_resume_enabled)
        .is_ok()
    {
        gio::Settings::sync();
    }
}

fn migrate_legacy_fallback_preferences(options: UiOptions) {
    let _ = save_ui_options(options);
}

fn default_stored_station() -> StoredStation {
    match UiOptions::default().station {
        Station::Kpop => StoredStation::Kpop,
        Station::Jpop => StoredStation::Jpop,
    }
}

impl Default for StoredUiOptionsOnDisk {
    fn default() -> Self {
        Self {
            station: Some(default_stored_station()),
            autoplay: Some(UiOptions::default().autoplay),
            pause_resume_enabled: Some(UiOptions::default().pause_resume_enabled),
            stop_instead_pause: Some(!UiOptions::default().pause_resume_enabled),
            discord_enabled: Some(UiOptions::default().discord_enabled),
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
        let (pause_resume_enabled, migrated_from_legacy) =
            load_pause_resume_enabled_from_settings(&settings);
        if migrated_from_legacy {
            migrate_legacy_gsettings_pause_resume_preference(&settings, pause_resume_enabled);
        }

        return Some(UiOptions {
            station,
            autoplay: settings.boolean(KEY_AUTOPLAY),
            pause_resume_enabled,
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
    let stored: StoredUiOptionsOnDisk = serde_json::from_str(&raw).ok()?;
    let migrated_from_legacy = stored.migrated_from_legacy_stop_flag();
    let options = stored.into_ui_options();
    if migrated_from_legacy {
        migrate_legacy_fallback_preferences(options);
    }
    Some(options)
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
            .set_boolean(KEY_PAUSE_RESUME_ENABLED, options.pause_resume_enabled)
            .map_err(|err| format!("Failed to save pause/resume preference: {err}"))?;
        settings
            .set_boolean(LEGACY_KEY_STOP_INSTEAD_PAUSE, !options.pause_resume_enabled)
            .map_err(|err| format!("Failed to save legacy stop behavior preference: {err}"))?;
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

#[cfg(test)]
mod tests {
    use super::{resolve_pause_resume_enabled, StoredUiOptions, StoredUiOptionsOnDisk};
    use crate::station::Station;
    use crate::ui::UiOptions;

    #[test]
    fn legacy_stop_flag_maps_to_pause_resume_enabled() {
        assert!(resolve_pause_resume_enabled(None, Some(false), false));
        assert!(!resolve_pause_resume_enabled(None, Some(true), true));
    }

    #[test]
    fn explicit_pause_resume_value_wins_over_legacy_flag() {
        assert!(!resolve_pause_resume_enabled(
            Some(false),
            Some(false),
            true
        ));
        assert!(resolve_pause_resume_enabled(Some(true), Some(true), false));
    }

    #[test]
    fn legacy_fallback_json_deserializes_into_pause_resume_enabled() {
        let stored: StoredUiOptionsOnDisk = serde_json::from_str(
            r#"{
                "station": "jpop",
                "autoplay": true,
                "stop_instead_pause": false,
                "discord_enabled": true
            }"#,
        )
        .expect("legacy preferences should deserialize");

        let options = stored.into_ui_options();

        assert!(options.pause_resume_enabled);
        assert!(options.autoplay);
        assert!(matches!(options.station, Station::Jpop));
    }

    #[test]
    fn stored_preferences_include_legacy_stop_flag_for_compatibility() {
        let stored = serde_json::to_value(StoredUiOptions::from(UiOptions {
            station: Station::Kpop,
            autoplay: false,
            pause_resume_enabled: true,
            discord_enabled: false,
        }))
        .expect("preferences should serialize");

        assert_eq!(
            stored.get("pause_resume_enabled").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            stored.get("stop_instead_pause").and_then(|v| v.as_bool()),
            Some(false)
        );
    }
}
