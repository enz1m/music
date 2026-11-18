//! Music's enzui-center functions, behind the `center` feature.
//!
//! Library lookup and transport only: list tracks, what is playing, play a
//! track, pause or resume. Library editing, playlists and folders are not
//! exposed. Reads are sensitive (a library and listening state are the
//! user's); every mutation is an ordinary [`Message`] through
//! [`MusicApp::update`]. Track ids are the per-scan ids the rest of the app
//! uses, valid for the running process.

use enzui_agent::center::{
    AppFunction, AppFunctionRegistry, AppMetadata, CenterFailure, CenterHost, CenterOutcome,
    CenterProvision, CenterTurn, ConnectorEvent, ConsentScope, FunctionRisk, FunctionSpec,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::*;

/// The app id the center credential is enrolled under.
pub(crate) const CENTER_APP_ID: &str = "io.github.enzui.Music";
/// The most tracks one listing returns.
pub(crate) const LIST_LIMIT_MAX: usize = 200;

/// What an admitted call asks Music to do.
#[derive(Debug, PartialEq)]
pub(crate) enum CenterCall {
    ListTracks { query: Option<String>, limit: usize },
    NowPlaying,
    PlayTrack { track_id: u64 },
    SetPlaying { playing: bool },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrackArgs {
    track_id: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlayingArgs {
    playing: bool,
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": properties,
    })
}

fn track_schema() -> Value {
    object(
        json!({
            "track_id": {"type": "integer"},
            "title": {"type": "string"},
            "artist": {"type": "string"},
            "album": {"type": "string"},
            "duration_secs": {"type": "number"}
        }),
        &["track_id", "title", "artist", "album", "duration_secs"],
    )
}

/// Music's registered functions.
pub(crate) fn registry() -> AppFunctionRegistry<CenterCall> {
    let once_or_session = vec![ConsentScope::Once, ConsentScope::Session];
    let list = FunctionSpec::new(
        "list_tracks",
        "List tracks",
        "Lists library tracks in library order, optionally only those matching a search query.",
        object(
            json!({
                "query": {"type": "string", "maxLength": 200},
                "limit": {"type": "integer", "minimum": 1, "maximum": LIST_LIMIT_MAX}
            }),
            &[],
        ),
        object(
            json!({
                "tracks": {"type": "array", "items": track_schema()},
                "total": {"type": "integer"}
            }),
            &["tracks", "total"],
        ),
        FunctionRisk::SensitiveRead,
        once_or_session.clone(),
    );
    let now_playing = FunctionSpec::new(
        "now_playing",
        "Now playing",
        "Reports whether playback is running and which library track is current.",
        object(json!({}), &[]),
        object(
            json!({
                "playing": {"type": "boolean"},
                "track": {"anyOf": [track_schema(), {"type": "null"}]},
                "position_secs": {"type": "number"}
            }),
            &["playing", "track", "position_secs"],
        ),
        FunctionRisk::SensitiveRead,
        once_or_session.clone(),
    );
    let play = FunctionSpec::new(
        "play_track",
        "Play a track",
        "Starts playing the library track with this id.",
        object(
            json!({"track_id": {"type": "integer", "minimum": 0}}),
            &["track_id"],
        ),
        object(
            json!({"track_id": {"type": "integer"}, "playing": {"type": "boolean"}}),
            &["track_id", "playing"],
        ),
        FunctionRisk::Mutating,
        once_or_session.clone(),
    );
    let set_playing = FunctionSpec::new(
        "set_playing",
        "Pause or resume",
        "Pauses (false) or resumes (true) playback of the current track.",
        object(json!({"playing": {"type": "boolean"}}), &["playing"]),
        object(json!({"playing": {"type": "boolean"}}), &["playing"]),
        FunctionRisk::Mutating,
        once_or_session,
    );
    AppFunctionRegistry::from_functions(vec![
        AppFunction::typed(list, |args: ListArgs| CenterCall::ListTracks {
            query: args.query.filter(|q| !q.trim().is_empty()),
            limit: args.limit.unwrap_or(50).clamp(1, LIST_LIMIT_MAX),
        })
        .expect("list_tracks spec"),
        AppFunction::new(now_playing, |_| CenterCall::NowPlaying).expect("now_playing spec"),
        AppFunction::typed(play, |args: TrackArgs| CenterCall::PlayTrack {
            track_id: args.track_id,
        })
        .expect("play_track spec"),
        AppFunction::typed(set_playing, |args: PlayingArgs| CenterCall::SetPlaying {
            playing: args.playing,
        })
        .expect("set_playing spec"),
    ])
    .expect("the music registry is valid")
}

pub(crate) fn metadata() -> AppMetadata {
    AppMetadata {
        id: CENTER_APP_ID.into(),
        name: "Music".into(),
        description: "Local music library and player".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    }
}

fn track_json(track: &domain::Track) -> Value {
    json!({
        "track_id": track.id,
        "title": track.title,
        "artist": track.artist,
        "album": track.album,
        "duration_secs": track.duration_secs,
    })
}

impl MusicApp {
    /// Connect to enzui-center when this build opted in and a credential is
    /// enrolled for [`CENTER_APP_ID`]; otherwise stay unexposed.
    pub(crate) fn start_center(&mut self) {
        let Some(provision) = CenterProvision::discover(CENTER_APP_ID) else {
            return;
        };
        match CenterHost::spawn(provision, metadata(), registry()) {
            Ok(host) => self.center = Some(host),
            Err(error) => eprintln!("music-app: center connector disabled: {error}"),
        }
    }

    pub(crate) fn center_subscription(&self) -> Subscription<Message> {
        self.center
            .as_ref()
            .map_or_else(Subscription::none, |host| {
                host.subscription(Message::Center)
            })
    }

    /// Final shutdown: refuse new calls, cancel live ones, join the connector.
    pub(crate) fn shutdown_center(&mut self) {
        if let Some(host) = self.center.take() {
            host.shutdown();
        }
    }

    pub(crate) fn center_event(&mut self, event: ConnectorEvent) -> Task<Message> {
        let Some(host) = self.center.as_mut() else {
            return Task::none();
        };
        let CenterTurn::Call(call, request) = host.receive_with_ends(event).turn else {
            return Task::none();
        };
        let (task, outcome) = self.apply_center_call(request);
        if let Some(host) = self.center.as_mut() {
            host.answer(call, outcome);
        }
        task
    }

    fn apply_center_call(&mut self, request: CenterCall) -> (Task<Message>, CenterOutcome) {
        match request {
            CenterCall::ListTracks { query, limit } => {
                let ids: Vec<u64> = match &query {
                    Some(query) => self.library.search(query).tracks,
                    None => self.library.track_order.clone(),
                };
                let tracks: Vec<Value> = ids
                    .iter()
                    .filter_map(|id| self.library.tracks.get(id))
                    .take(limit)
                    .map(track_json)
                    .collect();
                (
                    Task::none(),
                    Ok(json!({ "tracks": tracks, "total": ids.len() })),
                )
            }
            CenterCall::NowPlaying => {
                let track = self
                    .playback
                    .current_track_id
                    .and_then(|id| self.library.tracks.get(&id))
                    .map_or(Value::Null, track_json);
                (
                    Task::none(),
                    Ok(json!({
                        "playing": self.playback.playing,
                        "track": track,
                        "position_secs": self.playback.position_secs,
                    })),
                )
            }
            CenterCall::PlayTrack { track_id } => {
                if !self.library.tracks.contains_key(&track_id) {
                    return (
                        Task::none(),
                        Err(CenterFailure::new("not_found", "no track has that id")),
                    );
                }
                let task = self.update(Message::PlayTrack(track_id));
                let outcome = if self.playback.current_track_id == Some(track_id) {
                    Ok(json!({ "track_id": track_id, "playing": self.playback.playing }))
                } else {
                    Err(CenterFailure::new(
                        "unavailable",
                        "the track could not be started",
                    ))
                };
                (task, outcome)
            }
            CenterCall::SetPlaying { playing } => {
                if self.playback.current_track_id.is_none()
                    && self.playback.current_stream.is_none()
                {
                    return (
                        Task::none(),
                        Err(CenterFailure::new(
                            "unavailable",
                            "nothing is loaded to play",
                        )),
                    );
                }
                let task = if self.playback.playing == playing {
                    Task::none()
                } else {
                    self.update(Message::TogglePlayPause)
                };
                let outcome = if self.playback.playing == playing {
                    Ok(json!({ "playing": playing }))
                } else {
                    Err(CenterFailure::new("unavailable", "playback did not change"))
                };
                (task, outcome)
            }
        }
    }
}
