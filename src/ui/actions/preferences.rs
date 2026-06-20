use crate::locale::gettext;
use crate::preferences;
use crate::station::Station;
use adw::gtk::{self, prelude::*};
use adw::prelude::*;
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
    #[cfg(feature = "experimental")]
    window.set_default_size(360, 450);
    #[cfg(not(feature = "experimental"))]
    window.set_default_size(360, 330);

    let page = adw::PreferencesPage::new();
    let startup_group = adw::PreferencesGroup::new();
    startup_group.set_title(&gettext("Startup Defaults"));
    page.add(&startup_group);

    #[cfg(feature = "experimental")]
    let experimental_group = {
        let group = adw::PreferencesGroup::new();
        group.set_title(&gettext("Experimental Features"));
        page.add(&group);
        group
    };
    window.add(&page);

    let station_choices = [gettext("J-POP"), gettext("K-POP")];
    let station_model =
        gtk::StringList::new(&[station_choices[0].as_str(), station_choices[1].as_str()]);
    let station_row = adw::ComboRow::builder()
        .title(gettext("Default station"))
        .model(&station_model)
        .selected(match options.borrow().station {
            Station::Kpop => 1,
            Station::Jpop => 0,
        })
        .build();
    {
        let options = options.clone();
        station_row.connect_selected_notify(move |row| {
            let mut opts = options.borrow_mut();
            opts.station = if row.selected() == 1 {
                Station::Kpop
            } else {
                Station::Jpop
            };
            if let Err(err) = preferences::save_ui_options(*opts) {
                eprintln!("{err}");
            }
        });
    }
    startup_group.add(&station_row);

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
    startup_group.add(&autoplay_row);

    #[cfg(feature = "experimental")]
    {
        let pause_resume_row = adw::SwitchRow::builder()
            .title(gettext("Enable pause and resume"))
            .subtitle(gettext("Use pause and resume for the main playback button"))
            .active(options.borrow().pause_resume_enabled())
            .build();
        {
            let options = options.clone();
            pause_resume_row.connect_active_notify(move |row| {
                let mut opts = options.borrow_mut();
                opts.set_pause_resume_enabled(row.is_active());
                if let Err(err) = preferences::save_ui_options(*opts) {
                    eprintln!("{err}");
                }
            });
        }
        experimental_group.add(&pause_resume_row);
    }

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
    startup_group.add(&discord_row);

    window.present();
}
