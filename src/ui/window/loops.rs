use crate::log::{is_verbose, now_string};
use crate::ui::discord::Discord;

use adw::gtk::Button;
use adw::{
    glib,
    gtk::{
        self,
        gdk::{gdk_pixbuf::Pixbuf, Texture},
        gio::{Cancellable, MemoryInputStream},
        prelude::WidgetExt,
        ApplicationWindow, Picture, Popover,
    },
    prelude::PopoverExt,
    StyleManager, WindowTitle,
};
use gettextrs::gettext;
use std::time::Instant;
use std::{
    sync::{atomic::AtomicU32, atomic::Ordering, mpsc, Arc},
    thread,
    time::Duration,
};

use super::super::{
    controls::{MediaControlEvent, NowPlaying},
    cover,
    viz::VizHandle,
};
use super::state::{
    CoverFetchResult, MetadataSetter, RuntimeState, SharedTrack, UiEvent, UiResetReason,
};

const COVER_MAX_SIZE: i32 = 250;
const APP_NAME: &str = "Listen Moe";
const VIZ_FRAME_INTERVAL: Duration = Duration::from_millis(16);
const VIZ_DEAD_ZONE: f32 = 0.0008;
const VIZ_RISE_LERP: f32 = 0.28;
const VIZ_FALL_LERP: f32 = 0.18;
const VIZ_MAX_RISE_PER_FRAME: f32 = 0.040;
const VIZ_MAX_FALL_PER_FRAME: f32 = 0.028;

pub(super) struct UiUpdateLoopCtx {
    pub(super) window: ApplicationWindow,
    pub(super) win_title: WindowTitle,
    pub(super) pause_button: Button,
    pub(super) art_picture: Picture,
    pub(super) art_popover: Popover,
    pub(super) style_manager: StyleManager,
    pub(super) css_provider: gtk::CssProvider,
    pub(super) ui_rx: mpsc::Receiver<UiEvent>,
    pub(super) cover_tx: mpsc::Sender<CoverFetchResult>,
    pub(super) cover_rx: mpsc::Receiver<CoverFetchResult>,
    pub(super) ctrl_rx: Option<mpsc::Receiver<MediaControlEvent>>,
    pub(super) current_track: SharedTrack,
    pub(super) metadata_setter: MetadataSetter,
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
        ui_rx,
        cover_tx,
        cover_rx,
        ctrl_rx,
        current_track,
        metadata_setter,
        discord_enabled,
    } = ctx;

    let mut runtime = RuntimeState::new(current_track);

    let mut discord = Discord::new(discord_enabled);
    let mut was_playing = pause_button.is_visible();
    let mut last_track: Option<(String, String)> = None;
    let mut next_discord_refresh = Instant::now();
    const DISCORD_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
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

        let is_playing = pause_button.is_visible();
        if was_playing && !is_playing {
            let _ = discord.clear();
            last_track = None;
        }
        was_playing = is_playing;

        if is_playing && Instant::now() >= next_discord_refresh {
            if let Some((artist, title)) = last_track.as_ref() {
                let retry_after = if discord.set(artist, title).is_ok() {
                    DISCORD_REFRESH_INTERVAL
                } else {
                    DISCORD_RETRY_INTERVAL
                };
                next_discord_refresh = Instant::now() + retry_after;
            }
        }

        for event in ui_rx.try_iter() {
            match event {
                UiEvent::Connecting => {
                    win_title.set_title(APP_NAME);
                    win_title.set_subtitle(&gettext("Connecting..."));
                    runtime.clear_track();
                    runtime.set_latest_cover_url(None);
                    clear_art_ui(&art_picture, &art_popover, &style_manager, &css_provider);
                    (metadata_setter)(None);
                    let _ = discord.clear();
                    last_track = None;
                }
                UiEvent::Reset(reason) => {
                    reset_ui_state(
                        &win_title,
                        &art_picture,
                        &art_popover,
                        &style_manager,
                        &css_provider,
                        &mut runtime,
                        &metadata_setter,
                    );
                    let _ = discord.clear();
                    last_track = None;
                    if reason == UiResetReason::Stopped {
                        next_discord_refresh = Instant::now();
                    }
                }
                UiEvent::TrackChanged(info) => {
                    win_title.set_title(&info.artist);
                    win_title.set_subtitle(&info.title);
                    runtime.set_track(&info);

                    if discord.is_enabled() && is_verbose() {
                        println!(
                            "[{}] Update discord: {} {}",
                            now_string(),
                            &info.artist,
                            &info.title
                        );
                    }
                    last_track = Some((info.artist.clone(), info.title.clone()));
                    let retry_after = if discord.set(&info.artist, &info.title).is_ok() {
                        DISCORD_REFRESH_INTERVAL
                    } else {
                        DISCORD_RETRY_INTERVAL
                    };
                    next_discord_refresh = Instant::now() + retry_after;

                    let cover_url = info.album_cover.as_deref().or(info.artist_image.as_deref());
                    (metadata_setter)(Some(NowPlaying {
                        title: info.title.clone(),
                        artist: info.artist.clone(),
                        album: info.album.clone(),
                        art_url: cover_url.map(str::to_owned),
                    }));
                    runtime.set_latest_cover_url(cover_url);

                    if let Some(url) = cover_url {
                        let tx = cover_tx.clone();
                        let url = url.to_string();
                        thread::spawn(move || {
                            let result =
                                cover::fetch_cover_bytes_blocking(&url).map_err(|e| e.to_string());
                            let _ = tx.send((url, result));
                        });
                    } else {
                        clear_art_ui(&art_picture, &art_popover, &style_manager, &css_provider);
                    }
                }
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
    let mut bars = vec![0.0f32; spectrum_bits.len()];
    let mut smooth = vec![0.0f32; spectrum_bits.len()];

    glib::timeout_add_local(VIZ_FRAME_INTERVAL, move || {
        for i in 0..bars.len() {
            bars[i] = f32::from_bits(spectrum_bits[i].load(Ordering::Relaxed)).clamp(0.0, 1.0);
        }

        for i in 0..bars.len() {
            let delta = bars[i] - smooth[i];
            if delta.abs() <= VIZ_DEAD_ZONE {
                continue;
            }

            let step = if delta.is_sign_positive() {
                (delta * VIZ_RISE_LERP).min(VIZ_MAX_RISE_PER_FRAME)
            } else {
                (delta * VIZ_FALL_LERP).max(-VIZ_MAX_FALL_PER_FRAME)
            };
            smooth[i] = (smooth[i] + step).clamp(0.0, 1.0);
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

fn reset_ui_state(
    win_title: &WindowTitle,
    art_picture: &Picture,
    art_popover: &Popover,
    style_manager: &StyleManager,
    css_provider: &gtk::CssProvider,
    runtime: &mut RuntimeState,
    metadata_setter: &MetadataSetter,
) {
    win_title.set_title(APP_NAME);
    win_title.set_subtitle(&gettext("J-POP and K-POP radio"));
    runtime.clear_track();
    runtime.set_latest_cover_url(None);
    clear_art_ui(art_picture, art_popover, style_manager, css_provider);
    (metadata_setter)(None);
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

#[cfg(test)]
mod tests {
    use super::super::state::{RuntimeState, SharedTrack};
    use crate::meta::TrackInfo;
    use std::{cell::RefCell, rc::Rc};

    #[test]
    fn runtime_state_clear_drops_current_track_and_cover() {
        let current_track: SharedTrack = Rc::new(RefCell::new(None));
        let mut runtime = RuntimeState::new(current_track.clone());
        runtime.set_track(&TrackInfo {
            artist: "artist".into(),
            title: "title".into(),
            album: "album".into(),
            album_cover: Some("cover".into()),
            artist_image: None,
            start_time_ms: 1_000,
            duration_secs: 10,
        });
        runtime.set_latest_cover_url(Some("cover"));
        runtime.clear_track();
        runtime.set_latest_cover_url(None);

        assert!(current_track.borrow().is_none());
        assert!(!runtime.is_latest_cover("cover"));
    }
}
