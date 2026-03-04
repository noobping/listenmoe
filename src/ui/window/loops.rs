#[cfg(all(debug_assertions, feature = "discord"))]
use crate::log::now_string;
use crate::meta::TrackInfo;
#[cfg(feature = "discord")]
use crate::ui::discord::Discord;

use adw::{
    glib,
    gtk::{
        self,
        gdk::{gdk_pixbuf::Pixbuf, Texture},
        gio::{Cancellable, MemoryInputStream},
        prelude::WidgetExt,
        ApplicationWindow, Button, Picture, Popover,
    },
    prelude::PopoverExt,
    StyleManager, WindowTitle,
};
#[cfg(feature = "discord")]
use std::time::Instant;
use std::{
    sync::{atomic::AtomicU32, atomic::Ordering, mpsc, Arc},
    thread,
    time::Duration,
};

use super::super::{controls::MediaControlEvent, cover, viz::VizHandle};
use super::state::{CoverFetchResult, MetadataSetter, RuntimeState, SharedTrack};

const COVER_MAX_SIZE: i32 = 250;

pub(super) struct UiUpdateLoopCtx {
    pub(super) window: ApplicationWindow,
    pub(super) win_title: WindowTitle,
    pub(super) pause_button: Button,
    pub(super) art_picture: Picture,
    pub(super) art_popover: Popover,
    pub(super) style_manager: StyleManager,
    pub(super) css_provider: gtk::CssProvider,
    pub(super) track_rx: mpsc::Receiver<TrackInfo>,
    pub(super) cover_tx: mpsc::Sender<CoverFetchResult>,
    pub(super) cover_rx: mpsc::Receiver<CoverFetchResult>,
    pub(super) ctrl_rx: Option<mpsc::Receiver<MediaControlEvent>>,
    pub(super) current_track: SharedTrack,
    pub(super) metadata_setter: MetadataSetter,
    #[cfg(feature = "discord")]
    pub(super) discord_enabled: bool,
}

pub(super) fn spawn_ui_update_loop(ctx: UiUpdateLoopCtx) {
    let UiUpdateLoopCtx {
        window,
        win_title,
        pause_button,
        art_picture,
        art_popover,
        style_manager,
        css_provider,
        track_rx,
        cover_tx,
        cover_rx,
        ctrl_rx,
        current_track,
        metadata_setter,
        #[cfg(feature = "discord")]
        discord_enabled,
    } = ctx;

    let mut runtime = RuntimeState::new(current_track);

    #[cfg(feature = "discord")]
    let mut discord = if discord_enabled {
        Some(Discord::new())
    } else {
        None
    };
    #[cfg(feature = "discord")]
    let mut was_playing = pause_button.is_visible();
    #[cfg(feature = "discord")]
    let mut last_track: Option<(String, String)> = None;
    #[cfg(feature = "discord")]
    let mut next_discord_refresh = Instant::now();
    #[cfg(feature = "discord")]
    const DISCORD_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
    #[cfg(feature = "discord")]
    const DISCORD_RETRY_INTERVAL: Duration = Duration::from_millis(500);

    glib::timeout_add_local(Duration::from_millis(100), move || {
        if let Some(ctrl_rx) = &ctrl_rx {
            for event in ctrl_rx.try_iter() {
                let _ = adw::prelude::WidgetExt::activate_action(
                    &window,
                    event.action_name(),
                    None::<&glib::Variant>,
                );
            }
        }

        #[cfg(feature = "discord")]
        {
            let is_playing = pause_button.is_visible();
            if was_playing && !is_playing {
                if let Some(discord) = discord.as_mut() {
                    let _ = discord.clear();
                }
                last_track = None;
            }
            was_playing = is_playing;

            if is_playing && Instant::now() >= next_discord_refresh {
                if let (Some((artist, title)), Some(discord)) =
                    (last_track.as_ref(), discord.as_mut())
                {
                    let retry_after = if discord.set(artist, title).is_ok() {
                        DISCORD_REFRESH_INTERVAL
                    } else {
                        DISCORD_RETRY_INTERVAL
                    };
                    next_discord_refresh = Instant::now() + retry_after;
                }
            }
        }

        for info in track_rx.try_iter() {
            let TrackInfo {
                artist,
                title,
                album_cover,
                artist_image,
                ..
            } = info;

            win_title.set_title(&artist);
            win_title.set_subtitle(&title);
            runtime.set_track(&artist, &title);

            #[cfg(all(debug_assertions, feature = "discord"))]
            if discord.is_some() {
                println!("[{}] Update discord: {} {}", now_string(), &artist, &title);
            }
            #[cfg(feature = "discord")]
            if let Some(discord) = discord.as_mut() {
                last_track = Some((artist.clone(), title.clone()));
                let retry_after = if discord.set(&artist, &title).is_ok() {
                    DISCORD_REFRESH_INTERVAL
                } else {
                    DISCORD_RETRY_INTERVAL
                };
                next_discord_refresh = Instant::now() + retry_after;
            }

            let cover_url = album_cover.as_deref().or(artist_image.as_deref());
            (metadata_setter)(&title, &artist, cover_url);
            runtime.set_latest_cover_url(cover_url);

            if let Some(url) = cover_url {
                let tx = cover_tx.clone();
                let url = url.to_string();
                thread::spawn(move || {
                    let result = cover::fetch_cover_bytes_blocking(&url).map_err(|e| e.to_string());
                    let _ = tx.send((url, result));
                });
            } else {
                clear_art_ui(&art_picture, &art_popover, &style_manager, &css_provider);
            }
        }

        for (url, result) in cover_rx.try_iter() {
            if !runtime.is_latest_cover(&url) {
                continue;
            }

            match result {
                Ok(bytes_vec) => {
                    if let Err(err) =
                        apply_cover_bytes(bytes_vec, &art_picture, &style_manager, &css_provider)
                    {
                        eprintln!("Failed to decode cover pixbuf: {err}");
                        clear_art_ui(&art_picture, &art_popover, &style_manager, &css_provider);
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

pub(super) fn spawn_viz_loop(
    viz: gtk::DrawingArea,
    viz_handle: VizHandle,
    spectrum_bits: Arc<Vec<AtomicU32>>,
) {
    let mut smooth = vec![0.0f32; spectrum_bits.len()];

    glib::timeout_add_local(Duration::from_millis(33), move || {
        let mut bars = vec![0.0f32; spectrum_bits.len()];
        for i in 0..bars.len() {
            bars[i] = f32::from_bits(spectrum_bits[i].load(Ordering::Relaxed)).clamp(0.0, 1.0);
        }

        for i in 0..bars.len() {
            smooth[i] = smooth[i] * 0.70 + bars[i] * 0.30;
        }

        viz_handle.set_values(&smooth);
        viz.queue_draw();
        glib::ControlFlow::Continue
    });
}

fn clear_art_ui(
    art_picture: &Picture,
    art_popover: &Popover,
    style_manager: &StyleManager,
    css_provider: &gtk::CssProvider,
) {
    art_picture.set_paintable(None::<&adw::gdk::Paintable>);
    art_popover.popdown();
    style_manager.set_color_scheme(adw::ColorScheme::Default);
    cover::apply_cover_tint_css_clear(css_provider);
}

fn apply_cover_bytes(
    bytes_vec: Vec<u8>,
    art_picture: &Picture,
    style_manager: &StyleManager,
    css_provider: &gtk::CssProvider,
) -> Result<(), String> {
    let bytes = glib::Bytes::from_owned(bytes_vec);
    let stream = MemoryInputStream::from_bytes(&bytes);
    let pixbuf = Pixbuf::from_stream_at_scale(
        &stream,
        COVER_MAX_SIZE,
        COVER_MAX_SIZE,
        true,
        None::<&Cancellable>,
    )
    .map_err(|e| e.to_string())?;

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

    cover::apply_color(css_provider, (r, g, b), cover_is_light);
    Ok(())
}
