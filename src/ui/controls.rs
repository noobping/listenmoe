use adw::glib;
use mpris_server::{Metadata, PlaybackStatus, Player};
use std::{cell::RefCell, rc::Rc, sync::mpsc};

#[derive(Debug, Clone, Copy)]
pub enum MediaControlEvent {
    Play,
    Pause,
    Stop,
    Toggle,
    Next,
    Previous,
}

pub struct MediaControls {
    player: Rc<Player>,
    track_n: Rc<RefCell<u64>>,
}

impl MediaControls {
    pub fn set_playback(&self, status: PlaybackStatus) {
        let player = self.player.clone();
        glib::MainContext::default().spawn_local(async move {
            let _ = player.set_playback_status(status).await;
        });
    }

    pub fn set_metadata(&self, title: &str, artist: &str, album: &str, art_url: Option<&str>) {
        let player = self.player.clone();
        let track_n = self.track_n.clone();
        let title = title.to_string();
        let artist = artist.to_string();
        let album = album.to_string();
        let art_url = art_url.map(str::to_string);

        glib::MainContext::default().spawn_local(async move {
            *track_n.borrow_mut() += 1;

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

    let controls = Rc::new(MediaControls {
        player,
        track_n: Rc::new(RefCell::new(0)),
    });

    Ok((controls, rx))
}
