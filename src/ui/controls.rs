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

    let player = Rc::new(player);
    ctx.spawn_local(player.clone().run());

    let controls = Rc::new(MediaControls { player });

    Ok((controls, rx))
}
