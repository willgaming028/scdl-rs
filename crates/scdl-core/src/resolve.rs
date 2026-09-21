//! Turning what the user asked for into a concrete list of tracks.
//!
//! A single URL can mean a track, a playlist, or a user; and for a user, one of
//! several collections selected by a CLI flag. This module normalises all of
//! that into `Vec<Track>` with playlist context attached where it applies.

use crate::client::{Client, SearchKind, UserCollection};
use crate::error::{Error, Result};
use crate::model::{Entity, Playlist, PlaylistContext, Track};

/// What the user asked to download.
#[derive(Debug, Clone)]
pub enum Target {
    /// A soundcloud.com URL.
    Url(String),
    /// A search query; the first result is used.
    Search(String),
    /// The authenticated user's own profile.
    Me,
}

/// Which of a user's collections to pull, when the target resolves to a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Selector {
    /// Default: the user's own uploads.
    #[default]
    Tracks,
    /// `-a`: everything on their profile, reposts included.
    All,
    /// `-f`: their likes.
    Likes,
    /// `-C`: tracks they commented on.
    Comments,
    /// `-p`: their playlists.
    Playlists,
    /// `-r`: their reposts.
    Reposts,
}

impl Selector {
    fn collection(self) -> UserCollection {
        match self {
            Selector::Tracks => UserCollection::Tracks,
            Selector::All => UserCollection::All,
            Selector::Likes => UserCollection::Likes,
            Selector::Comments => UserCollection::Comments,
            Selector::Playlists => UserCollection::Sets,
            Selector::Reposts => UserCollection::Reposts,
        }
    }

    pub fn label(self) -> &'static str {
        self.collection().label()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ResolveOptions {
    pub selector: Selector,
    /// `-o N`: skip the first `N - 1` tracks. 1-indexed, as scdl documents it.
    pub offset: Option<usize>,
    /// Stop after this many tracks.
    pub limit: Option<usize>,
    /// `--no-playlist`: when a URL resolves to a playlist, refuse rather than
    /// downloading it.
    pub no_playlist: bool,
}

/// The outcome of resolving a target.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub tracks: Vec<Track>,
    /// Set when everything came from one playlist, for the subfolder name.
    pub playlist: Option<PlaylistSummary>,
    /// Human-readable description of what was resolved, for the UI.
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct PlaylistSummary {
    pub id: i64,
    pub title: String,
    pub uploader: String,
}

/// Resolve a target into tracks.
pub async fn resolve(client: &Client, target: &Target, opts: &ResolveOptions) -> Result<Resolved> {
    let entity = match target {
        Target::Url(url) => {
            let normalized = crate::client::normalize_soundcloud_url(url)?;
            client.resolve(&normalized).await?
        }
        Target::Search(query) => {
            let mut results = client.search(SearchKind::All, query, 1).await?;
            if results.is_empty() {
                return Err(Error::NoSearchResults(query.clone()));
            }
            results.remove(0)
        }
        Target::Me => {
            let me = client.me().await?;
            Entity::User(Box::new(me))
        }
    };

    let mut resolved = match entity {
        Entity::Track(t) => Resolved {
            label: format!("{} — {}", t.artist(), t.title_or_untitled()),
            tracks: vec![*t],
            playlist: None,
        },

        Entity::Playlist(p) => {
            if opts.no_playlist {
                return Err(Error::UnsupportedUrl(format!(
                    "{} is a playlist and --no-playlist was given",
                    p.title_or_untitled()
                )));
            }
            let mut p = *p;
            client.hydrate_playlist(&mut p).await?;
            playlist_to_resolved(p)
        }

        Entity::User(u) => {
            let user_id =
                u.id.ok_or_else(|| Error::UnsupportedUrl("user record had no id".to_string()))?;
            let name = u.display_name().to_string();

            if opts.selector == Selector::Playlists {
                // Each playlist expands into its tracks, keeping album context.
                let playlists = client
                    .user_collection_playlists(user_id, UserCollection::Sets)
                    .await?;
                let mut tracks = Vec::new();
                for mut p in playlists {
                    let id = p.id;
                    if client.hydrate_playlist(&mut p).await.is_err() {
                        continue;
                    }
                    let _ = id;
                    tracks.extend(playlist_to_resolved(p).tracks);
                }
                Resolved {
                    label: format!("{name} (playlists)"),
                    tracks,
                    playlist: None,
                }
            } else {
                let collection = opts.selector.collection();
                let tracks = client
                    .user_collection_tracks(user_id, collection, None)
                    .await?;
                Resolved {
                    label: format!("{name} ({})", collection.label()),
                    tracks,
                    playlist: None,
                }
            }
        }
    };

    apply_offset_and_limit(&mut resolved, opts);
    Ok(resolved)
}

fn playlist_to_resolved(p: Playlist) -> Resolved {
    let total = p.tracks.len();
    let title = p.title_or_untitled().to_string();
    let uploader = p.uploader().to_string();
    let id = p.id;

    let tracks = p
        .tracks
        .into_iter()
        .enumerate()
        .map(|(i, mut t)| {
            t.playlist_context = Some(PlaylistContext {
                id,
                title: title.clone(),
                uploader: uploader.clone(),
                index: i + 1,
                total,
            });
            t
        })
        .collect();

    Resolved {
        label: format!("{title} — {total} tracks"),
        tracks,
        playlist: Some(PlaylistSummary {
            id,
            title,
            uploader,
        }),
    }
}

/// Apply `-o` and any limit.
///
/// Playlist context indices are deliberately *not* renumbered: track 20 of a
/// playlist stays track 20 in its tags even when the run starts there.
fn apply_offset_and_limit(resolved: &mut Resolved, opts: &ResolveOptions) {
    if let Some(offset) = opts.offset {
        let skip = offset.saturating_sub(1);
        if skip >= resolved.tracks.len() {
            resolved.tracks.clear();
        } else {
            resolved.tracks.drain(..skip);
        }
    }
    if let Some(limit) = opts.limit {
        resolved.tracks.truncate(limit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::User;

    fn playlist(n: usize) -> Playlist {
        let tracks: Vec<Track> = (0..n)
            .map(|i| {
                let mut t: Track = serde_json::from_str(r#"{"id": 0}"#).unwrap();
                t.id = i as i64 + 1;
                t.title = Some(format!("Track {}", i + 1));
                t
            })
            .collect();

        Playlist {
            id: 77,
            title: Some("The Lost Ship".into()),
            permalink: None,
            permalink_url: None,
            description: None,
            duration: None,
            genre: None,
            tag_list: None,
            license: None,
            created_at: None,
            last_modified: None,
            release_date: None,
            published_at: None,
            artwork_url: None,
            user: Some(User {
                username: Some("pandadub".into()),
                ..Default::default()
            }),
            track_count: Some(n as i64),
            likes_count: None,
            reposts_count: None,
            set_type: None,
            is_album: None,
            secret_token: None,
            sharing: None,
            kind: None,
            tracks,
        }
    }

    #[test]
    fn playlist_context_is_one_indexed_with_totals() {
        let r = playlist_to_resolved(playlist(3));
        assert_eq!(r.tracks.len(), 3);
        let ctx = r.tracks[0].playlist_context.as_ref().unwrap();
        assert_eq!(ctx.index, 1);
        assert_eq!(ctx.total, 3);
        assert_eq!(ctx.title, "The Lost Ship");
        assert_eq!(ctx.uploader, "pandadub");
        assert_eq!(r.tracks[2].playlist_context.as_ref().unwrap().index, 3);
    }

    #[test]
    fn offset_is_one_indexed() {
        let mut r = playlist_to_resolved(playlist(5));
        apply_offset_and_limit(
            &mut r,
            &ResolveOptions {
                offset: Some(3),
                ..Default::default()
            },
        );
        assert_eq!(r.tracks.len(), 3);
        assert_eq!(r.tracks[0].title.as_deref(), Some("Track 3"));
    }

    #[test]
    fn offset_of_one_is_a_noop() {
        let mut r = playlist_to_resolved(playlist(5));
        apply_offset_and_limit(
            &mut r,
            &ResolveOptions {
                offset: Some(1),
                ..Default::default()
            },
        );
        assert_eq!(r.tracks.len(), 5);
    }

    #[test]
    fn offset_past_the_end_yields_nothing_rather_than_panicking() {
        let mut r = playlist_to_resolved(playlist(3));
        apply_offset_and_limit(
            &mut r,
            &ResolveOptions {
                offset: Some(99),
                ..Default::default()
            },
        );
        assert!(r.tracks.is_empty());
    }

    #[test]
    fn offset_preserves_original_track_numbers_for_tagging() {
        // Track 3 of 5 must still tag as 3/5 when the run starts at 3.
        let mut r = playlist_to_resolved(playlist(5));
        apply_offset_and_limit(
            &mut r,
            &ResolveOptions {
                offset: Some(3),
                ..Default::default()
            },
        );
        let ctx = r.tracks[0].playlist_context.as_ref().unwrap();
        assert_eq!(ctx.index, 3);
        assert_eq!(ctx.total, 5);
    }

    #[test]
    fn limit_truncates() {
        let mut r = playlist_to_resolved(playlist(10));
        apply_offset_and_limit(
            &mut r,
            &ResolveOptions {
                limit: Some(4),
                ..Default::default()
            },
        );
        assert_eq!(r.tracks.len(), 4);
    }

    #[test]
    fn selector_maps_to_the_right_api_collection() {
        assert_eq!(Selector::All.collection(), UserCollection::All);
        assert_eq!(Selector::Tracks.collection(), UserCollection::Tracks);
        assert_eq!(Selector::Likes.collection(), UserCollection::Likes);
        assert_eq!(Selector::Comments.collection(), UserCollection::Comments);
        assert_eq!(Selector::Playlists.collection(), UserCollection::Sets);
        assert_eq!(Selector::Reposts.collection(), UserCollection::Reposts);
    }
}
