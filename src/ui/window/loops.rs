use crate::listen::PlaybackClock;
use crate::log::{is_verbose, now_string};
use crate::lyrics::{self, LyricCue, LyricsRequest, LyricsResult};
use crate::ui::discord::Discord;
use crate::ui::ScrollingWindowTitle as WindowTitle;

#[cfg(target_os = "linux")]
use crate::{listen::Listen, volume::VolumeEvent};

use crate::locale::gettext;
use adw::{
    glib,
    gtk::{
        self,
        gdk::{gdk_pixbuf::Pixbuf, Texture},
        gio::{Cancellable, MemoryInputStream, SimpleAction},
        prelude::WidgetExt,
        ApplicationWindow, Picture, Popover, Stack,
    },
    prelude::PopoverExt,
    StyleManager,
};
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use std::{
    sync::{atomic::AtomicU32, atomic::Ordering, mpsc, Arc},
    thread,
    time::Duration,
};

#[cfg(target_os = "linux")]
use super::super::controls::MediaControls;
#[cfg(target_os = "linux")]
use super::super::volume::VolumeUi;
use super::super::{
    controls::{MediaControlEvent, NowPlaying},
    cover,
    karaoke::KaraokeView,
    progress::TitlebarProgress,
    viz::VizHandle,
};
#[cfg(target_os = "linux")]
use super::state::SharedVolumeState;
use super::state::{
    CoverFetchResult, MetadataSetter, RuntimeState, SharedTitle, SharedTrack, UiEvent,
    UiResetReason,
};

const COVER_MAX_SIZE: i32 = 250;
const APP_NAME: &str = "Listen Moe";
const VIZ_FRAME_INTERVAL: Duration = Duration::from_millis(16);
const VIZ_DEAD_ZONE: f32 = 0.0008;
const VIZ_RISE_LERP: f32 = 0.28;
const VIZ_FALL_LERP: f32 = 0.18;
const VIZ_MAX_RISE_PER_FRAME: f32 = 0.040;
const VIZ_MAX_FALL_PER_FRAME: f32 = 0.028;

type LyricsFetchResult = (u64, LyricsResult);

enum LyricsLoadState {
    Idle,
    Loading,
    Ready(Vec<LyricCue>),
    Missing,
    Error,
}

pub(super) struct UiUpdateLoopCtx {
    pub(super) window: ApplicationWindow,
    pub(super) win_title: WindowTitle,
    pub(super) normal_title: SharedTitle,
    pub(super) playback_playing: Rc<Cell<bool>>,
    pub(super) update_title_override: Rc<Cell<bool>>,
    pub(super) art_picture: Picture,
    pub(super) art_popover: Popover,
    pub(super) art_stack: Stack,
    pub(super) karaoke_view: KaraokeView,
    pub(super) karaoke_action: SimpleAction,
    pub(super) style_manager: StyleManager,
    pub(super) css_provider: gtk::CssProvider,
    pub(super) ui_rx: mpsc::Receiver<UiEvent>,
    pub(super) cover_tx: mpsc::Sender<CoverFetchResult>,
    pub(super) cover_rx: mpsc::Receiver<CoverFetchResult>,
    pub(super) ctrl_rx: Option<mpsc::Receiver<MediaControlEvent>>,
    pub(super) current_track: SharedTrack,
    pub(super) metadata_setter: MetadataSetter,
    pub(super) playback_clock: Arc<PlaybackClock>,
    pub(super) titlebar_progress: TitlebarProgress,
    #[cfg(target_os = "linux")]
    pub(super) volume_ui: VolumeUi,
    #[cfg(target_os = "linux")]
    pub(super) volume_event_rx: mpsc::Receiver<VolumeEvent>,
    #[cfg(target_os = "linux")]
    pub(super) volume_state: SharedVolumeState,
    #[cfg(target_os = "linux")]
    pub(super) volume_radio: Rc<Listen>,
    #[cfg(target_os = "linux")]
    pub(super) volume_controls: Option<Rc<MediaControls>>,
    pub(super) discord_enabled: bool,
}

pub(super) fn spawn_ui_update_loop(ctx: UiUpdateLoopCtx) {
    let UiUpdateLoopCtx {
        window,
        win_title,
        normal_title,
        playback_playing,
        update_title_override,
        art_picture,
        art_popover,
        art_stack,
        karaoke_view,
        karaoke_action,
        style_manager,
        css_provider,
        ui_rx,
        cover_tx,
        cover_rx,
        ctrl_rx,
        current_track,
        metadata_setter,
        playback_clock,
        titlebar_progress,
        #[cfg(target_os = "linux")]
        volume_ui,
        #[cfg(target_os = "linux")]
        volume_event_rx,
        #[cfg(target_os = "linux")]
        volume_state,
        #[cfg(target_os = "linux")]
        volume_radio,
        #[cfg(target_os = "linux")]
        volume_controls,
        discord_enabled,
    } = ctx;

    let mut runtime = RuntimeState::new(current_track);
    let (lyrics_tx, lyrics_rx) = mpsc::channel::<LyricsFetchResult>();
    let mut lyrics_generation = 0_u64;
    let mut lyrics_request: Option<LyricsRequest> = None;
    let mut lyrics_start_time_ms: Option<u64> = None;
    let mut lyrics_state = LyricsLoadState::Idle;
    let mut rendered_cue_index: Option<Option<usize>> = None;
    let mut karaoke_was_visible = false;

    let mut discord = Discord::new(discord_enabled);
    let mut was_playing = playback_playing.get();
    let mut last_track: Option<(String, String)> = None;
    let mut next_discord_refresh = Instant::now();
    const DISCORD_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
    const DISCORD_RETRY_INTERVAL: Duration = Duration::from_millis(500);

    glib::timeout_add_local(Duration::from_millis(100), move || {
        if let Some(ctrl_rx) = &ctrl_rx {
            for event in ctrl_rx.try_iter() {
                #[cfg(target_os = "linux")]
                if let MediaControlEvent::SetVolume(percent) = event {
                    volume_ui.request_percent(percent);
                    continue;
                }

                let _ = adw::prelude::WidgetExt::activate_action(
                    &window,
                    event.action_name(),
                    None::<&glib::Variant>,
                );
            }
        }

        #[cfg(target_os = "linux")]
        for event in volume_event_rx.try_iter() {
            let update = volume_state.borrow_mut().apply_backend_event(event);
            volume_ui.set_percent_silent(update.display_percent);
            volume_radio.set_volume_percent(update.software_percent);
            if let Some(controls) = volume_controls.as_ref() {
                controls.set_volume_percent(update.display_percent);
            }
        }

        let is_playing = playback_playing.get();
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
                    let title = APP_NAME.to_string();
                    let subtitle = gettext("Connecting...");
                    *normal_title.borrow_mut() = (title.clone(), subtitle.clone());
                    if !update_title_override.get() {
                        win_title.set_title(&title);
                        win_title.set_subtitle(&subtitle);
                    }
                    runtime.clear_track();
                    runtime.set_latest_cover_url(None);
                    clear_art_ui(
                        &art_picture,
                        &art_popover,
                        &art_stack,
                        &style_manager,
                        &css_provider,
                    );
                    art_popover.popdown();
                    art_stack.set_visible_child_name("art");
                    karaoke_action.set_enabled(false);
                    lyrics_generation = lyrics_generation.wrapping_add(1);
                    lyrics_request = None;
                    lyrics_start_time_ms = None;
                    lyrics_state = LyricsLoadState::Idle;
                    rendered_cue_index = None;
                    karaoke_was_visible = false;
                    karaoke_view.show_loading();
                    (metadata_setter)(None);
                    let _ = discord.clear();
                    last_track = None;
                }
                UiEvent::Reset(reason) => {
                    reset_ui_state(
                        &win_title,
                        &normal_title,
                        &update_title_override,
                        &mut runtime,
                        &metadata_setter,
                    );
                    clear_art_ui(
                        &art_picture,
                        &art_popover,
                        &art_stack,
                        &style_manager,
                        &css_provider,
                    );
                    art_popover.popdown();
                    art_stack.set_visible_child_name("art");
                    karaoke_action.set_enabled(false);
                    lyrics_generation = lyrics_generation.wrapping_add(1);
                    lyrics_request = None;
                    lyrics_start_time_ms = None;
                    lyrics_state = LyricsLoadState::Idle;
                    rendered_cue_index = None;
                    karaoke_was_visible = false;
                    karaoke_view.show_loading();
                    let _ = discord.clear();
                    last_track = None;
                    if reason == UiResetReason::Stopped {
                        next_discord_refresh = Instant::now();
                    }
                }
                UiEvent::TrackChanged(info) => {
                    *normal_title.borrow_mut() = (info.artist.clone(), info.title.clone());
                    if !update_title_override.get() {
                        win_title.set_title(&info.artist);
                        win_title.set_subtitle(&info.title);
                    }
                    runtime.set_track(&info);
                    lyrics_generation = lyrics_generation.wrapping_add(1);
                    lyrics_request = Some(LyricsRequest::from_track(&info));
                    lyrics_start_time_ms = Some(info.start_time_ms);
                    lyrics_state = LyricsLoadState::Idle;
                    rendered_cue_index = None;
                    karaoke_view.show_loading();
                    karaoke_action.set_enabled(true);

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
                        clear_art_ui(
                            &art_picture,
                            &art_popover,
                            &art_stack,
                            &style_manager,
                            &css_provider,
                        );
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
                        clear_art_ui(
                            &art_picture,
                            &art_popover,
                            &art_stack,
                            &style_manager,
                            &css_provider,
                        );
                    }
                }
                Err(err) => {
                    eprintln!("Failed to load cover bytes: {err}");
                    clear_art_ui(
                        &art_picture,
                        &art_popover,
                        &art_stack,
                        &style_manager,
                        &css_provider,
                    );
                }
            }
        }

        for (generation, result) in lyrics_rx.try_iter() {
            if generation != lyrics_generation {
                continue;
            }

            rendered_cue_index = None;
            match result {
                LyricsResult::Found(cues) => {
                    lyrics_state = LyricsLoadState::Ready(cues);
                }
                LyricsResult::Missing => {
                    lyrics_state = LyricsLoadState::Missing;
                    karaoke_view.show_missing();
                }
                LyricsResult::Error(error) => {
                    eprintln!("Failed to load lyrics: {error}");
                    lyrics_state = LyricsLoadState::Error;
                    karaoke_view.show_error();
                }
            }
        }

        let live_now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let cursor_ms = progress_cursor_ms(&playback_clock, live_now_ms);
        titlebar_progress.set_track_fraction(runtime.progress_fraction(cursor_ms));

        let karaoke_is_visible = art_popover.is_visible()
            && art_stack.visible_child_name().as_deref() == Some("karaoke");

        // Reopening after a transient error is the passive view's retry action.
        // Any click within the popover still closes it through layout's existing
        // popover-wide gesture handler.
        if karaoke_is_visible
            && !karaoke_was_visible
            && matches!(lyrics_state, LyricsLoadState::Error)
        {
            lyrics_state = LyricsLoadState::Idle;
            karaoke_view.show_loading();
        }

        if karaoke_is_visible && matches!(lyrics_state, LyricsLoadState::Idle) {
            if let Some(request) = lyrics_request.clone() {
                let tx = lyrics_tx.clone();
                let generation = lyrics_generation;
                lyrics_state = LyricsLoadState::Loading;
                karaoke_view.show_loading();
                thread::spawn(move || {
                    let result = lyrics::fetch(&request);
                    let _ = tx.send((generation, result));
                });
            }
        }

        if karaoke_is_visible {
            if let LyricsLoadState::Ready(cues) = &lyrics_state {
                let active_index = lyrics_start_time_ms
                    .and_then(|start_time_ms| active_lyric_index(cues, start_time_ms, cursor_ms));

                if rendered_cue_index != Some(active_index) {
                    let (previous, current, next) = lyric_context(cues, active_index);
                    karaoke_view.show_lyrics(previous, current, next);
                    rendered_cue_index = Some(active_index);
                }
            }
        }

        karaoke_was_visible = karaoke_is_visible;

        glib::ControlFlow::Continue
    });
}

fn progress_cursor_ms(clock: &PlaybackClock, live_now_ms: u64) -> u64 {
    if clock.is_live_playback() {
        live_now_ms
    } else {
        clock.playback_cursor_ms()
    }
}

fn lyric_context(
    cues: &[LyricCue],
    active_index: Option<usize>,
) -> (Option<&str>, &str, Option<&str>) {
    let Some(index) = active_index else {
        return (None, "", cues.first().map(|cue| cue.text.as_str()));
    };

    let previous = index
        .checked_sub(1)
        .and_then(|previous| cues.get(previous))
        .map(|cue| cue.text.as_str());
    let current = cues
        .get(index)
        .map(|cue| cue.text.as_str())
        .unwrap_or_default();
    let next = index
        .checked_add(1)
        .and_then(|next| cues.get(next))
        .map(|cue| cue.text.as_str());
    (previous, current, next)
}

fn active_lyric_index(cues: &[LyricCue], track_start_ms: u64, cursor_ms: u64) -> Option<usize> {
    cursor_ms
        .checked_sub(track_start_ms)
        .and_then(|elapsed_ms| lyrics::active_cue_index(cues, elapsed_ms))
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
    art_stack: &Stack,
    style_manager: &StyleManager,
    css_provider: &gtk::CssProvider,
) {
    art_picture.set_paintable(None::<&adw::gdk::Paintable>);
    if art_stack.visible_child_name().as_deref() == Some("art") {
        art_popover.popdown();
    }
    style_manager.set_color_scheme(adw::ColorScheme::Default);
    cover::apply_cover_tint_css_clear(css_provider);
}

fn reset_ui_state(
    win_title: &WindowTitle,
    normal_title: &SharedTitle,
    update_title_override: &Rc<Cell<bool>>,
    runtime: &mut RuntimeState,
    metadata_setter: &MetadataSetter,
) {
    let subtitle = gettext("J-POP and K-POP radio");
    *normal_title.borrow_mut() = (APP_NAME.to_string(), subtitle.clone());
    if !update_title_override.get() {
        win_title.set_title(APP_NAME);
        win_title.set_subtitle(&subtitle);
    }
    runtime.clear_track();
    runtime.set_latest_cover_url(None);
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
    use super::{active_lyric_index, lyric_context, progress_cursor_ms};
    use crate::listen::PlaybackClock;
    use crate::lyrics::LyricCue;
    use crate::meta::TrackInfo;
    use std::{cell::RefCell, rc::Rc};

    #[test]
    fn runtime_state_clear_drops_current_track_and_cover() {
        let current_track: SharedTrack = Rc::new(RefCell::new(None));
        let mut runtime = RuntimeState::new(current_track.clone());
        runtime.set_track(&TrackInfo {
            artist: "artist".into(),
            primary_artist: "artist".into(),
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

    #[test]
    fn live_progress_uses_wall_time_while_buffered_progress_uses_its_cursor() {
        let clock = PlaybackClock::new();
        clock.set_playback_cursor_ms(12_000);

        assert_eq!(progress_cursor_ms(&clock, 99_000), 12_000);

        clock.set_live_playback(true);
        assert_eq!(progress_cursor_ms(&clock, 99_000), 99_000);
    }

    #[test]
    fn karaoke_uses_track_relative_time_and_neighboring_cues() {
        let cues = vec![
            LyricCue {
                start_ms: 1_000,
                text: "First".into(),
            },
            LyricCue {
                start_ms: 2_000,
                text: "Second".into(),
            },
            LyricCue {
                start_ms: 3_000,
                text: "Third".into(),
            },
        ];

        assert_eq!(active_lyric_index(&cues, 10_000, 9_999), None);
        let active = active_lyric_index(&cues, 10_000, 12_500);
        assert_eq!(active, Some(1));
        assert_eq!(
            lyric_context(&cues, active),
            (Some("First"), "Second", Some("Third"))
        );
        assert_eq!(lyric_context(&cues, None), (None, "", Some("First")));
    }
}
