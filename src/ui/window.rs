use crate::listen::Listen;
use crate::meta::{Meta, TrackInfo};
use crate::station::Station;

use adw::{
    glib,
    gtk::{
        self,
        gdk::{gdk_pixbuf::Pixbuf, Texture},
        gio::{Cancellable, MemoryInputStream, Menu},
        prelude::WidgetExt,
        ApplicationWindow, Button, GestureClick, HeaderBar, MenuButton, Orientation, Picture,
        Popover,
    },
    prelude::*,
    Application, StyleManager, WindowTitle,
};
use gettextrs::gettext;
use std::{
    sync::{atomic::Ordering, mpsc},
    thread,
    time::Duration,
};

use super::controls::MediaControlEvent;
use super::{actions, cover, viz};

const COVER_MAX_SIZE: i32 = 250;
const APP_NAME: &str = "Listen Moe";
const APP_ID: &str = "io.github.noobping.listenmoe";

pub fn build_ui(app: &Application) {
    let station = Station::Jpop;
    let radio = Listen::new(station);
    let spectrum_bits = radio.spectrum_bars();
    let (tx, rx) = mpsc::channel::<TrackInfo>();
    let meta = Meta::new(station, tx, radio.lag_ms());
    let (cover_tx, cover_rx) = mpsc::channel::<Result<Vec<u8>, String>>();
    let win_title = WindowTitle::new(APP_NAME, &gettext("J-POP and K-POP radio"));

    let play_button = Button::from_icon_name("media-playback-start-symbolic");
    play_button.set_action_name(Some("win.play"));
    let pause_button = Button::from_icon_name("media-playback-pause-symbolic");
    pause_button.set_action_name(Some("win.pause"));
    pause_button.set_visible(false);

    let height = 50;
    let window = ApplicationWindow::builder()
        .application(app)
        .title(APP_NAME)
        .icon_name(APP_ID)
        .default_width(300)
        .default_height(height)
        .resizable(false)
        .build();

    window.add_css_class("cover-tint");
    let style_manager = StyleManager::default();
    style_manager.set_color_scheme(adw::ColorScheme::Default);
    let css_provider = cover::install_css_provider();

    let (controls, ctrl_rx) = actions::build_actions(
        &window,
        &app,
        &win_title,
        &play_button,
        &pause_button,
        &radio,
        &meta,
    );
    let set_metadata = {
        let controls = controls.clone();
        move |title: String, artist: String, art_url: Option<&str>| {
            if let Some(c) = controls.as_ref() {
                c.set_metadata(title.as_str(), artist.as_str(), APP_NAME, art_url);
            }
        }
    };

    // Build UI
    let menu = Menu::new();
    actions::populate_menu(&window, &play_button, &menu, &radio, &meta);
    let more_button = MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("Main Menu")
        .menu_model(&menu)
        .build();
    let buttons = gtk::Box::new(Orientation::Horizontal, 0);
    buttons.append(&more_button);
    buttons.append(&play_button);
    buttons.append(&pause_button);
    let header = HeaderBar::new();
    header.pack_start(&buttons);
    header.set_title_widget(Some(&win_title));
    header.set_show_title_buttons(false);
    header.add_css_class("cover-tint");
    header.set_height_request(height);

    let art_picture = Picture::builder()
        .can_shrink(true)
        .focusable(false)
        .sensitive(false)
        .build();
    let art_popover = Popover::builder()
        .has_arrow(true)
        .position(gtk::PositionType::Bottom)
        .autohide(true)
        .child(&art_picture)
        .build();
    art_popover.set_parent(&header);
    art_popover.add_css_class("cover-tint");
    let title_click = GestureClick::new();
    {
        let picture = art_picture.clone();
        let art = art_popover.clone();
        title_click.connect_released(move |_, _, _, _| {
            if art.is_visible() {
                art.popdown();
            } else if picture.paintable().is_some() {
                art.popup();
            }
        });
    }
    win_title.add_controller(title_click);
    let close_any_click = GestureClick::new();
    {
        let art = art_popover.clone();
        close_any_click.connect_released(move |_, _, _, _| {
            art.popdown();
        });
    }
    art_popover.add_controller(close_any_click);

    let close_btn = Button::from_icon_name("window-close-symbolic");
    close_btn.set_action_name(Some("win.quit"));
    header.pack_end(&close_btn);

    let overlay = gtk::Overlay::new();
    overlay.add_css_class("titlebar-tint");
    overlay.set_height_request(height);

    // Create bars visualizer and add it behind headerbar
    let (viz, viz_handle) = viz::make_bars_visualizer(48, height);
    overlay.set_child(Some(&viz));

    header.add_css_class("viz-transparent");
    header.add_css_class("cover-tint");
    overlay.add_overlay(&header);
    window.set_titlebar(Some(&overlay));

    // Tiny dummy content so GTK can shrink the window
    let dummy = gtk::Box::new(Orientation::Vertical, 0);
    dummy.set_height_request(0);
    dummy.set_vexpand(false);
    window.set_child(Some(&dummy));

    // Poll the channels on the GTK main thread and update the UI.
    {
        let win = win_title.clone();
        let art_popover = art_popover.clone();
        let art_picture = art_picture.clone();
        let cover_rx = cover_rx;
        let cover_tx = cover_tx.clone();
        let window = window.clone();
        let set_metadata = set_metadata.clone();

        let clear_art_ui = |art_picture: &gtk::Picture,
                            art_popover: &gtk::Popover,
                            style_manager: &adw::StyleManager,
                            css_provider: &gtk::CssProvider| {
            // Clear old cover so it doesn't stick around
            art_picture.set_paintable(None::<&adw::gdk::Paintable>);

            // Reset the rest of the UI state
            art_popover.popdown();
            style_manager.set_color_scheme(adw::ColorScheme::Default);
            cover::apply_cover_tint_css_clear(css_provider);
        };

        glib::timeout_add_local(Duration::from_millis(100), move || {
            if let Some(ctrl_rx) = &ctrl_rx {
                for event in ctrl_rx.try_iter() {
                    let _ = match event {
                        MediaControlEvent::Play => adw::prelude::WidgetExt::activate_action(
                            &window,
                            "win.play",
                            None::<&glib::Variant>,
                        ),
                        MediaControlEvent::Pause => adw::prelude::WidgetExt::activate_action(
                            &window,
                            "win.pause",
                            None::<&glib::Variant>,
                        ),
                        MediaControlEvent::Stop => adw::prelude::WidgetExt::activate_action(
                            &window,
                            "win.stop",
                            None::<&glib::Variant>,
                        ),
                        MediaControlEvent::Toggle => adw::prelude::WidgetExt::activate_action(
                            &window,
                            "win.toggle",
                            None::<&glib::Variant>,
                        ),
                        MediaControlEvent::Next => adw::prelude::WidgetExt::activate_action(
                            &window,
                            "win.next_station",
                            None::<&glib::Variant>,
                        ),
                        MediaControlEvent::Previous => adw::prelude::WidgetExt::activate_action(
                            &window,
                            "win.prev_station",
                            None::<&glib::Variant>,
                        ),
                    };
                }
            }

            for info in rx.try_iter() {
                win.set_title(&info.artist);
                win.set_subtitle(&info.title);

                let cover_url = info
                    .album_cover
                    .as_ref()
                    .or(info.artist_image.as_ref())
                    .map(|s| s.as_str());

                set_metadata(info.title.clone(), info.artist.clone(), cover_url.clone());

                if let Some(url) = info.album_cover.as_ref().or(info.artist_image.as_ref()) {
                    let tx = cover_tx.clone();
                    let url = url.to_string();
                    thread::spawn(move || {
                        let result =
                            cover::fetch_cover_bytes_blocking(&url).map_err(|e| e.to_string());
                        let _ = tx.send(result);
                    });
                } else {
                    clear_art_ui(&art_picture, &art_popover, &style_manager, &css_provider);
                }
            }

            for result in cover_rx.try_iter() {
                match result {
                    Ok(bytes_vec) => {
                        let bytes = glib::Bytes::from_owned(bytes_vec);
                        let stream = MemoryInputStream::from_bytes(&bytes);
                        match Pixbuf::from_stream_at_scale(
                            &stream,
                            COVER_MAX_SIZE,
                            COVER_MAX_SIZE,
                            true,
                            None::<&Cancellable>,
                        ) {
                            Ok(pixbuf) => {
                                let texture = Texture::for_pixbuf(&pixbuf);
                                art_picture.set_paintable(Some(&texture));

                                let (r, g, b) = cover::avg_rgb_from_pixbuf(&pixbuf);
                                let (r, g, b) = cover::boost_saturation(r, g, b, 1.15);
                                let cover_is_light = cover::is_light_color(r, g, b);

                                style_manager.set_color_scheme(if cover_is_light {
                                    adw::ColorScheme::ForceLight
                                } else {
                                    adw::ColorScheme::ForceDark
                                });

                                cover::apply_color(&css_provider, (r, g, b), cover_is_light);
                            }
                            Err(err) => {
                                eprintln!("Failed to decode cover pixbuf: {err}");
                                clear_art_ui(
                                    &art_picture,
                                    &art_popover,
                                    &style_manager,
                                    &css_provider,
                                );
                            }
                        }
                    }
                    Err(err) => {
                        eprintln!("Failed to load cover bytes: {err}");
                        clear_art_ui(&art_picture, &art_popover, &style_manager, &css_provider);
                    }
                }
            }

            glib::ControlFlow::Continue
        });
    }

    // music animation
    {
        let viz = viz.clone();
        let handle = viz_handle.clone();
        let spectrum_bits = spectrum_bits.clone();

        // UI-side smoothing (optional)
        let mut smooth = vec![0.0f32; spectrum_bits.len()];

        glib::timeout_add_local(Duration::from_millis(33), move || {
            let mut bars = vec![0.0f32; spectrum_bits.len()];
            for i in 0..bars.len() {
                bars[i] = f32::from_bits(spectrum_bits[i].load(Ordering::Relaxed)).clamp(0.0, 1.0);
            }

            for i in 0..bars.len() {
                smooth[i] = smooth[i] * 0.70 + bars[i] * 0.30;
            }

            handle.set_values(&smooth);
            viz.queue_draw();
            glib::ControlFlow::Continue
        });
    }

    window.present();
}
