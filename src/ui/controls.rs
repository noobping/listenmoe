#[cfg(target_os = "windows")]
use std::{rc::Rc, sync::mpsc};

#[derive(Debug, Clone, Copy)]
pub enum MediaControlEvent {
    Play,
    Pause,
    Stop,
    Toggle,
    Next,
    Previous,
}

impl MediaControlEvent {
    pub fn action_name(self) -> &'static str {
        match self {
            Self::Play => "win.play",
            Self::Pause => "win.pause",
            Self::Stop => "win.stop",
            Self::Toggle => "win.toggle",
            Self::Next => "win.next_station",
            Self::Previous => "win.prev_station",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::{MediaControlEvent, PlaybackStatus};
    use adw::{glib, gtk::ApplicationWindow};
    use mpris_server::{Metadata, PlaybackStatus as MprisPlaybackStatus, Player};
    use std::{rc::Rc, sync::mpsc};

    pub struct MediaControls {
        player: Rc<Player>,
    }

    impl MediaControls {
        pub fn set_playback(&self, status: PlaybackStatus) {
            let player = self.player.clone();
            let status = match status {
                PlaybackStatus::Playing => MprisPlaybackStatus::Playing,
                PlaybackStatus::Paused => MprisPlaybackStatus::Paused,
                PlaybackStatus::Stopped => MprisPlaybackStatus::Stopped,
            };

            glib::MainContext::default().spawn_local(async move {
                let _ = player.set_playback_status(status).await;
            });
        }

        pub fn set_metadata(&self, title: &str, artist: &str, album: &str, art_url: Option<&str>) {
            let player = self.player.clone();
            let title = title.to_string();
            let artist = artist.to_string();
            let album = album.to_string();
            let art_url = art_url.map(str::to_string);

            glib::MainContext::default().spawn_local(async move {
                let mut b = Metadata::builder()
                    .title(title)
                    .artist([artist])
                    .album(album);

                if let Some(url) = art_url {
                    b = b.art_url(url);
                }

                let _ = player.set_metadata(b.build()).await;
            });
        }
    }

    pub fn build_controls(
        _window: &ApplicationWindow,
        bus_suffix: &str,
        identity: &str,
        desktop_entry: &str,
    ) -> Result<(Rc<MediaControls>, mpsc::Receiver<MediaControlEvent>), String> {
        let (tx, rx) = mpsc::channel();

        let ctx = glib::MainContext::default();
        let player = ctx
            .block_on(async {
                Player::builder(bus_suffix)
                    .identity(identity)
                    .desktop_entry(desktop_entry)
                    .can_control(true)
                    .can_play(true)
                    .can_pause(true)
                    .can_go_next(true)
                    .can_go_previous(true)
                    .build()
                    .await
            })
            .map_err(|e| e.to_string())?;

        macro_rules! connect_media_events {
            ($player:expr, $tx:expr, $($method:ident => $event:ident),+ $(,)?) => {
                $(
                    {
                        let tx = $tx.clone();
                        $player.$method(move |_| {
                            let _ = tx.send(MediaControlEvent::$event);
                        });
                    }
                )+
            };
        }
        connect_media_events!(player, tx,
            connect_play => Play,
            connect_pause => Pause,
            connect_stop => Stop,
            connect_play_pause => Toggle,
            connect_next => Next,
            connect_previous => Previous,
        );

        let player = Rc::new(player);
        ctx.spawn_local(player.clone().run());

        let controls = Rc::new(MediaControls { player });

        Ok((controls, rx))
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::{MediaControlEvent, PlaybackStatus};
    use adw::glib::translate::ToGlibPtr;
    use adw::gtk::{prelude::NativeExt, ApplicationWindow};
    use gdk4_win32_sys::{gdk_win32_surface_get_handle, GdkWin32Surface};
    use souvlaki::{
        MediaControlEvent as OsMediaControlEvent, MediaControls as OsMediaControls, MediaMetadata,
        MediaPlayback, PlatformConfig,
    };
    use std::{cell::RefCell, ffi::c_void, rc::Rc, sync::mpsc};

    pub struct MediaControls {
        controls: RefCell<OsMediaControls>,
    }

    impl MediaControls {
        pub fn set_playback(&self, status: PlaybackStatus) {
            let playback = match status {
                PlaybackStatus::Playing => MediaPlayback::Playing { progress: None },
                PlaybackStatus::Paused => MediaPlayback::Paused { progress: None },
                PlaybackStatus::Stopped => MediaPlayback::Stopped,
            };
            let _ = self.controls.borrow_mut().set_playback(playback);
        }

        pub fn set_metadata(&self, title: &str, artist: &str, album: &str, art_url: Option<&str>) {
            let _ = self.controls.borrow_mut().set_metadata(MediaMetadata {
                title: Some(title),
                artist: Some(artist),
                album: Some(album),
                cover_url: art_url,
                duration: None,
            });
        }
    }

    pub fn build_controls(
        window: &ApplicationWindow,
        bus_suffix: &str,
        identity: &str,
        _desktop_entry: &str,
    ) -> Result<(Rc<MediaControls>, mpsc::Receiver<MediaControlEvent>), String> {
        let (tx, rx) = mpsc::channel();

        let surface = window
            .surface()
            .ok_or_else(|| "Window surface unavailable for media controls".to_string())?;
        let raw_surface = <adw::gtk::gdk::Surface as ToGlibPtr<
            *mut adw::gtk::gdk::ffi::GdkSurface,
        >>::to_glib_none(&surface)
        .0;
        let hwnd = unsafe {
            gdk_win32_surface_get_handle(raw_surface as *mut GdkWin32Surface) as *mut c_void
        };
        let hwnd = if hwnd.is_null() { None } else { Some(hwnd) };

        let mut controls = OsMediaControls::new(PlatformConfig {
            dbus_name: bus_suffix,
            display_name: identity,
            hwnd,
        })
        .map_err(|e| e.to_string())?;

        controls
            .attach({
                let tx = tx.clone();
                move |event| {
                    let mapped = match event {
                        OsMediaControlEvent::Play => Some(MediaControlEvent::Play),
                        OsMediaControlEvent::Pause => Some(MediaControlEvent::Pause),
                        OsMediaControlEvent::Stop => Some(MediaControlEvent::Stop),
                        OsMediaControlEvent::Toggle => Some(MediaControlEvent::Toggle),
                        OsMediaControlEvent::Next => Some(MediaControlEvent::Next),
                        OsMediaControlEvent::Previous => Some(MediaControlEvent::Previous),
                        _ => None,
                    };
                    if let Some(event) = mapped {
                        let _ = tx.send(event);
                    }
                }
            })
            .map_err(|e| e.to_string())?;

        Ok((
            Rc::new(MediaControls {
                controls: RefCell::new(controls),
            }),
            rx,
        ))
    }
}

pub use imp::{build_controls, MediaControls};
