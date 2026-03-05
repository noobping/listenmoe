use crate::preferences;
use adw::gtk::{self, gio, prelude::*};
use adw::prelude::*;
use gettextrs::gettext;

const KEY_STATION: &str = "default-station";
const KEY_AUTOPLAY: &str = "autoplay";
const KEY_STOP_INSTEAD_PAUSE: &str = "stop-instead-pause";
const KEY_DISCORD_ENABLED: &str = "discord-enabled";

pub fn show_preferences_window(parent: &gtk::ApplicationWindow) {
    let Some(settings) = preferences::settings() else {
        eprintln!("Could not open preferences: missing GSettings schema");
        return;
    };

    let window = adw::PreferencesWindow::new();
    window.set_title(Some(&gettext("Preferences")));
    window.set_transient_for(Some(parent));
    window.set_modal(true);
    window.set_hide_on_close(true);
    window.set_default_size(360, 370);

    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    group.set_title(&gettext("Startup Defaults"));
    page.add(&group);
    window.add(&page);

    let station_row = adw::ActionRow::builder()
        .title(gettext("Default station"))
        .build();
    let station_choices = [gettext("J-POP"), gettext("K-POP")];
    let station_dropdown =
        gtk::DropDown::from_strings(&[station_choices[0].as_str(), station_choices[1].as_str()]);
    station_dropdown.set_selected(match settings.string(KEY_STATION).as_str() {
        "kpop" => 1,
        _ => 0,
    });
    {
        let settings = settings.clone();
        station_dropdown.connect_selected_notify(move |dropdown| {
            let station = if dropdown.selected() == 1 {
                "kpop"
            } else {
                "jpop"
            };
            if settings.set_string(KEY_STATION, station).is_ok() {
                gio::Settings::sync();
            }
        });
    }
    station_row.add_suffix(&station_dropdown);
    station_row.set_activatable_widget(Some(&station_dropdown));
    group.add(&station_row);

    let autoplay_row = adw::SwitchRow::builder()
        .title(gettext("Autoplay"))
        .subtitle(gettext("Start playing automatically on launch"))
        .active(settings.boolean(KEY_AUTOPLAY))
        .build();
    {
        let settings = settings.clone();
        autoplay_row.connect_active_notify(move |row| {
            if settings.set_boolean(KEY_AUTOPLAY, row.is_active()).is_ok() {
                gio::Settings::sync();
            }
        });
    }
    group.add(&autoplay_row);

    let stop_row = adw::SwitchRow::builder()
        .title(gettext("Use stop instead of pause"))
        .subtitle(gettext("Use stop behavior for the main playback button"))
        .active(settings.boolean(KEY_STOP_INSTEAD_PAUSE))
        .build();
    {
        let settings = settings.clone();
        stop_row.connect_active_notify(move |row| {
            if settings
                .set_boolean(KEY_STOP_INSTEAD_PAUSE, row.is_active())
                .is_ok()
            {
                gio::Settings::sync();
            }
        });
    }
    group.add(&stop_row);

    let discord_row = adw::SwitchRow::builder()
        .title(gettext("Discord Rich Presence"))
        .subtitle(gettext("Enable Discord Rich Presence at runtime"))
        .active(settings.boolean(KEY_DISCORD_ENABLED))
        .build();
    discord_row.connect_active_notify(move |row| {
        if settings
            .set_boolean(KEY_DISCORD_ENABLED, row.is_active())
            .is_ok()
        {
            gio::Settings::sync();
        }
    });
    group.add(&discord_row);

    window.present();
}
