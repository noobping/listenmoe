use crate::locale::gettext;
use adw::gtk::{self, ApplicationWindow};
#[cfg(target_os = "windows")]
use gtk::glib::variant::ToVariant;
use std::{cell::Cell, rc::Rc};

use crate::listen::Listen;
use crate::meta::Meta;
use crate::station::Station;

use super::{activate_window_action, register_window_action};

pub fn populate_menu(
    window: &ApplicationWindow,
    playback_playing: &Rc<Cell<bool>>,
    menu: &gtk::gio::Menu,
    radio: &Rc<Listen>,
    meta: &Rc<Meta>,
) {
    menu.append(Some(&gettext("Copy current track")), Some("win.copy"));

    for station in [Station::Jpop, Station::Kpop] {
        register_station_action(station, playback_playing, window, radio, meta);
        menu.append(
            Some(
                gettext("Play %s")
                    .replace("%s", station.display_name())
                    .as_str(),
            ),
            Some(&format!("win.{}", station.name())),
        );
    }

    menu.append(
        Some(&gettext("Keyboard Shortcuts")),
        Some("win.show-help-overlay"),
    );
    #[cfg(target_os = "windows")]
    {
        append_hidden_when_disabled(menu, &gettext("Check for updates"), "win.check-for-updates");
        append_hidden_when_disabled(menu, &gettext("Cancel update"), "win.cancel-update");
    }
    menu.append(Some(&gettext("Preferences")), Some("win.preferences"));
    menu.append(Some(&gettext("About")), Some("win.about"));
    menu.append(Some(&gettext("Quit")), Some("win.quit"));
}

#[cfg(target_os = "windows")]
fn append_hidden_when_disabled(menu: &gtk::gio::Menu, label: &str, detailed_action: &str) {
    let item = gtk::gio::MenuItem::new(Some(label), Some(detailed_action));
    item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
    menu.append_item(&item);
}

fn register_station_action(
    station: Station,
    playback_playing: &Rc<Cell<bool>>,
    window: &ApplicationWindow,
    radio: &Rc<Listen>,
    meta: &Rc<Meta>,
) {
    let radio = radio.clone();
    let meta = meta.clone();
    let win = window.clone();
    let playback_playing = playback_playing.clone();

    register_window_action(window, station.name(), move || {
        radio.set_station(station);
        meta.set_station(station);
        if !playback_playing.get() {
            activate_window_action(&win, "win.play");
        }
    });
}
