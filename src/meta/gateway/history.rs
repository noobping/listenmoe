use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

use crate::meta::error::MetaResult;
use crate::meta::track::TrackInfo;
use crate::station::Station;

use super::model::Song;
use super::parse::build_track_info;

const GRAPHQL_URL: &str = "https://listen.moe/graphql";
const HISTORY_QUERY: &str = r#"
  query playStatistics($offset: Int!, $count: Int!, $kpop: Boolean) {
    playStatistics(offset: $offset, count: $count, kpop: $kpop) {
      songs {
        song {
          title
          duration
          artists { name image }
          albums { name image }
        }
        createdAt
      }
    }
  }
"#;

pub(super) const RECENT_HISTORY_COUNT: usize = 8;

pub(super) struct PlayHistoryClient {
    client: Client,
}

impl PlayHistoryClient {
    pub(super) fn new() -> MetaResult<Self> {
        let client = Client::builder().timeout(Duration::from_secs(10)).build()?;
        Ok(Self { client })
    }

    pub(super) fn fetch_recent_tracks(
        &self,
        station: Station,
        count: usize,
    ) -> MetaResult<Vec<TrackInfo>> {
        let response = self
            .client
            .post(GRAPHQL_URL)
            .header(
                reqwest::header::USER_AGENT,
                format!("{}/{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_string(&PlayHistoryRequest {
                query: HISTORY_QUERY,
                variables: PlayHistoryVariables {
                    offset: 0,
                    count,
                    kpop: matches!(station, Station::Kpop),
                },
            })?)
            .send()?
            .error_for_status()?;

        parse_history_tracks_json(&response.text()?)
    }
}

fn parse_history_tracks_json(raw: &str) -> MetaResult<Vec<TrackInfo>> {
    let response: PlayHistoryResponse = serde_json::from_str(raw)?;
    let mut tracks = response
        .data
        .play_statistics
        .songs
        .into_iter()
        .filter_map(|entry| {
            let song = entry.song?;
            let start_time_ms = entry.created_at.as_ms()?;
            Some(build_track_info(&song, start_time_ms))
        })
        .collect::<Vec<_>>();
    tracks.sort_by_key(|track| track.start_time_ms);
    Ok(tracks)
}

#[derive(Debug, Serialize)]
struct PlayHistoryRequest<'a> {
    query: &'a str,
    variables: PlayHistoryVariables,
}

#[derive(Debug, Serialize)]
struct PlayHistoryVariables {
    offset: usize,
    count: usize,
    kpop: bool,
}

#[derive(Debug, Deserialize)]
struct PlayHistoryResponse {
    data: PlayHistoryData,
}

#[derive(Debug, Deserialize)]
struct PlayHistoryData {
    #[serde(rename = "playStatistics")]
    play_statistics: PlayStatistics,
}

#[derive(Debug, Deserialize)]
struct PlayStatistics {
    songs: Vec<PlayHistoryEntry>,
}

#[derive(Debug, Deserialize)]
struct PlayHistoryEntry {
    song: Option<Song>,
    #[serde(rename = "createdAt")]
    created_at: CreatedAt,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CreatedAt {
    String(String),
    Number(u64),
}

impl CreatedAt {
    fn as_ms(&self) -> Option<u64> {
        match self {
            Self::String(value) => value.parse().ok(),
            Self::Number(value) => Some(*value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_history_tracks_json;

    #[test]
    fn parses_recent_history_start_timestamps() {
        let raw = r#"{
          "data": {
            "playStatistics": {
              "songs": [
                {
                  "song": {
                    "title": "Current",
                    "duration": 180,
                    "artists": [{ "name": "Artist", "image": "artist.jpg" }],
                    "albums": [{ "name": "Album", "image": "album.jpg" }]
                  },
                  "createdAt": "1774866158963"
                },
                {
                  "song": {
                    "title": "Previous",
                    "duration": 120,
                    "artists": [{ "name": "Prev Artist", "image": null }],
                    "albums": [{ "name": "Prev Album", "image": null }]
                  },
                  "createdAt": 1774866038963
                }
              ]
            }
          }
        }"#;

        let tracks = parse_history_tracks_json(raw).expect("history should parse");
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].title, "Previous");
        assert_eq!(tracks[0].start_time_ms, 1_774_866_038_963);
        assert_eq!(tracks[1].title, "Current");
        assert_eq!(tracks[1].start_time_ms, 1_774_866_158_963);
    }
}
