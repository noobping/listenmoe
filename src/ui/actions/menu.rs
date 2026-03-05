use adw::gtk::{self, prelude::WidgetExt, ApplicationWindow, Button};
use gettextrs::gettext;
use std::rc::Rc;

use crate::listen::Listen;
use crate::meta::Meta;
use crate::station::Station;

use super::{activate_window_action, register_window_action};

pub fn populate_menu(
    window: &ApplicationWindow,
    play_button: &Button,
    menu: &gtk::gio::Menu,
    radio: &Rc<Listen>,
    meta: &Rc<Meta>,
) {
    menu.append(Some(&gettext("Copy current track")), Some("win.copy"));

    for station in [Station::Jpop, Station::Kpop] {
        register_station_action(station, play_button, window, radio, meta);
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
    menu.append(Some(&gettext("Preferences")), Some("win.preferences"));
    menu.append(Some(&gettext("About")), Some("win.about"));
    menu.append(Some(&gettext("Quit")), Some("win.quit"));
}

fn register_station_action(
    station: Station,
    play_button: &Button,
    window: &ApplicationWindow,
    radio: &Rc<Listen>,
    meta: &Rc<Meta>,
) {
    let radio = radio.clone();
    let meta = meta.clone();
    let win = window.clone();
    let play = play_button.clone();

    register_window_action(window, station.name(), move || {
        radio.set_station(station);
        meta.set_station(station);
        if play.is_visible() {
            activate_window_action(&win, "win.play");
        }
    });
}
