use serde::Deserialize;
use serde_json::Value;

use crate::meta::track::{ALBUM_COVER_BASE, ARTIST_IMAGE_BASE};

#[derive(Debug, Deserialize)]
pub(super) struct GatewayHello {
    pub heartbeat: u64,
}

#[derive(Debug, Deserialize)]
pub(super) struct GatewaySongPayload {
    pub song: Song,
    #[serde(rename = "startTime")]
    pub start_time: String,
    #[serde(rename = "lastPlayed", default)]
    pub last_played: Vec<Song>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Song {
    pub title: Option<String>,
    #[serde(default)]
    pub artists: Vec<Artist>,
    #[serde(default)]
    pub albums: Vec<Album>,
    pub duration: Option<u32>,
}

impl Song {
    pub fn display_title(self: &Self) -> String {
        self.title
            .clone()
            .unwrap_or_else(|| "unknown title".to_owned())
    }

    pub fn display_artist(self: &Self) -> String {
        if self.artists.is_empty() {
            return "Unknown artist".to_owned();
        }

        self.artists
            .iter()
            .filter_map(|a| a.name.as_deref())
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn display_album(self: &Self) -> String {
        self.albums
            .first()
            .and_then(|album| album.name.as_deref())
            .map(str::to_owned)
            .unwrap_or_default()
    }

    pub fn album_cover_url(self: &Self) -> Option<String> {
        self.albums
            .first()
            .and_then(|album| album.image.as_deref())
            .as_cdn_url(ALBUM_COVER_BASE)
    }

    pub fn artist_image_url(self: &Self) -> Option<String> {
        self.artists
            .first()
            .and_then(|artist| artist.image.as_deref())
            .as_cdn_url(ARTIST_IMAGE_BASE)
    }

    pub fn duration_secs(self: &Self) -> u32 {
        self.duration.unwrap_or(0)
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct Artist {
    pub name: Option<String>,
    pub image: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Album {
    pub name: Option<String>,
    pub image: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GatewayEnvelope {
    pub op: u8,
    #[serde(default)]
    pub t: Option<String>,
    #[serde(default)]
    pub d: Value,
}

trait CdnImageExt {
    fn as_cdn_url(self, base: &str) -> Option<String>;
}

impl CdnImageExt for Option<&str> {
    fn as_cdn_url(self, base: &str) -> Option<String> {
        self.map(|name| format!("{base}{name}"))
    }
}

pub(super) const OP_HELLO: u8 = 0;
pub(super) const OP_DISPATCH: u8 = 1;
pub(super) const OP_HEARTBEAT_ACK: u8 = 10;
pub(super) const EVENT_TRACK_UPDATE: &str = "TRACK_UPDATE";
pub(super) const EVENT_TRACK_UPDATE_REQUEST: &str = "TRACK_UPDATE_REQUEST";
