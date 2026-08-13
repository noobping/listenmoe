use adw::glib;
use mpris_server::{Metadata, PlaybackStatus, Player};
use std::{rc::Rc, sync::mpsc};

#[derive(Debug, Clone, Copy)]
pub enum MediaControlEvent {
    Play,
    Pause,
    Stop,
    Toggle,
    Next,
    Previous,
    #[cfg(target_os = "linux")]
    SetVolume(u8),
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
            #[cfg(target_os = "linux")]
            Self::SetVolume(_) => unreachable!("volume events are handled separately"),
        }
    }
}

pub struct MediaControls {
    player: Rc<Player>,
}

#[derive(Debug, Clone)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: Option<String>,
}

impl MediaControls {
    pub fn set_playback(&self, status: PlaybackStatus) {
        let player = self.player.clone();
        glib::MainContext::default().spawn_local(async move {
            let _ = player.set_playback_status(status).await;
        });
    }

    pub fn set_metadata(&self, now_playing: Option<NowPlaying>) {
        let player = self.player.clone();

        glib::MainContext::default().spawn_local(async move {
            let metadata = if let Some(now_playing) = now_playing {
                let mut b = Metadata::builder()
                    .title(now_playing.title)
                    .artist([now_playing.artist])
                    .album(now_playing.album);

                if let Some(url) = now_playing.art_url {
                    b = b.art_url(url);
                }

                b.build()
            } else {
                Metadata::builder().build()
            };

            let _ = player.set_metadata(metadata).await;
        });
    }

    #[cfg(target_os = "linux")]
    pub fn set_volume_percent(&self, percent: u8) {
        let player = self.player.clone();
        glib::MainContext::default().spawn_local(async move {
            let volume = f64::from(percent.min(100)) / 100.0;
            let _ = player.set_volume(volume).await;
        });
    }
}

#[cfg(target_os = "linux")]
fn mpris_volume_to_percent(volume: f64) -> u8 {
    if volume.is_nan() {
        return 0;
    }

    (volume.clamp(0.0, 1.0) * 100.0).round() as u8
}

pub fn build_controls(
    bus_suffix: &str,
    identity: &str,
    desktop_entry: &str,
) -> Result<(Rc<MediaControls>, mpsc::Receiver<MediaControlEvent>), mpris_server::zbus::Error> {
    let (tx, rx) = mpsc::channel();

    let ctx = glib::MainContext::default();
    let player = ctx.block_on(async {
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
    })?;

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

    #[cfg(target_os = "linux")]
    {
        let tx = tx.clone();
        player.connect_set_volume(move |_, volume| {
            let _ = tx.send(MediaControlEvent::SetVolume(mpris_volume_to_percent(
                volume,
            )));
        });
    }

    let player = Rc::new(player);
    ctx.spawn_local(player.clone().run());

    let controls = Rc::new(MediaControls { player });

    Ok((controls, rx))
}

#[cfg(test)]
mod tests {
    use super::NowPlaying;

    #[test]
    fn now_playing_keeps_album_and_art() {
        let now_playing = NowPlaying {
            title: "title".into(),
            artist: "artist".into(),
            album: "album".into(),
            art_url: Some("https://example.test/cover.jpg".into()),
        };

        assert_eq!(now_playing.album, "album");
        assert_eq!(
            now_playing.art_url.as_deref(),
            Some("https://example.test/cover.jpg")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mpris_volume_is_clamped_and_rounded_to_percent() {
        use super::mpris_volume_to_percent;

        assert_eq!(mpris_volume_to_percent(-0.1), 0);
        assert_eq!(mpris_volume_to_percent(0.0), 0);
        assert_eq!(mpris_volume_to_percent(0.504), 50);
        assert_eq!(mpris_volume_to_percent(0.505), 51);
        assert_eq!(mpris_volume_to_percent(1.0), 100);
        assert_eq!(mpris_volume_to_percent(1.5), 100);
        assert_eq!(mpris_volume_to_percent(f64::NAN), 0);
    }
}
