use crate::preferences;
use crate::station::Station;
use adw::gtk::{self, prelude::*};
use adw::prelude::*;
use gettextrs::gettext;
use std::{cell::RefCell, rc::Rc};

pub fn show_preferences_window(parent: &gtk::ApplicationWindow) {
    let options = Rc::new(RefCell::new(
        preferences::load_ui_options().unwrap_or_default(),
    ));

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
    station_dropdown.set_selected(match options.borrow().station {
        Station::Kpop => 1,
        Station::Jpop => 0,
    });
    {
        let options = options.clone();
        station_dropdown.connect_selected_notify(move |dropdown| {
            let mut opts = options.borrow_mut();
            opts.station = if dropdown.selected() == 1 {
                Station::Kpop
            } else {
                Station::Jpop
            };
            if let Err(err) = preferences::save_ui_options(*opts) {
                eprintln!("{err}");
            }
        });
    }
    station_row.add_suffix(&station_dropdown);
    station_row.set_activatable_widget(Some(&station_dropdown));
    group.add(&station_row);

    let autoplay_row = adw::SwitchRow::builder()
        .title(gettext("Autoplay"))
        .subtitle(gettext("Start playing automatically on launch"))
        .active(options.borrow().autoplay)
        .build();
    {
        let options = options.clone();
        autoplay_row.connect_active_notify(move |row| {
            let mut opts = options.borrow_mut();
            opts.autoplay = row.is_active();
            if let Err(err) = preferences::save_ui_options(*opts) {
                eprintln!("{err}");
            }
        });
    }
    group.add(&autoplay_row);

    let stop_row = adw::SwitchRow::builder()
        .title(gettext("Use stop instead of pause"))
        .subtitle(gettext("Use stop behavior for the main playback button"))
        .active(options.borrow().stop_instead_pause)
        .build();
    {
        let options = options.clone();
        stop_row.connect_active_notify(move |row| {
            let mut opts = options.borrow_mut();
            opts.stop_instead_pause = row.is_active();
            if let Err(err) = preferences::save_ui_options(*opts) {
                eprintln!("{err}");
            }
        });
    }
    group.add(&stop_row);

    let discord_row = adw::SwitchRow::builder()
        .title(gettext("Discord Rich Presence"))
        .subtitle(gettext("Enable Discord Rich Presence at runtime"))
        .active(options.borrow().discord_enabled)
        .build();
    let options = options.clone();
    discord_row.connect_active_notify(move |row| {
        let mut opts = options.borrow_mut();
        opts.discord_enabled = row.is_active();
        if let Err(err) = preferences::save_ui_options(*opts) {
            eprintln!("{err}");
        }
    });
    group.add(&discord_row);

    window.present();
}
