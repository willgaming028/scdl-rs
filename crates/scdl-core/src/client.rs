//! SoundCloud v2 API client.
//!
//! SoundCloud has no public v2 API and no way to register for a key. The web
//! player authenticates with a `client_id` baked into its JavaScript bundles, so
//! that is what we do too: scrape <https://soundcloud.com/>, walk its `<script
//! src>` tags and pull the first 32-character `client_id` we find. The id rotates
//! every few weeks, so [`Client::call_api`] treats a 401/403 as "the id went
//! stale", refreshes once, and retries.

use std::sync::Arc;
use std::time::Duration;

use regex::Regex;
use serde::de::DeserializeOwned;
use tokio::sync::RwLock;
use url::Url;

use crate::error::{Error, Result};
use crate::model::{Entity, Page, Playlist, Track, User};

pub const API_V2: &str = "https://api-v2.soundcloud.com/";
pub const WEB_BASE: &str = "https://soundcloud.com/";
const AUTH_VERIFY: &str = "https://api-auth.soundcloud.com/connect/session";

/// Chrome UA. SoundCloud serves a stripped-down page to unrecognised agents, and
/// the stripped-down page does not contain the script bundles we need.
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
     Chrome/120.0.0.0 Safari/537.36";

/// The API caps `linked_partitioning` pages at 200 items.
/// <https://developers.soundcloud.com/blog/offset-pagination-deprecated>
pub const MAX_PAGE_LIMIT: usize = 200;

/// `GET /tracks?ids=` takes at most 50 ids per request.
pub const ID_BATCH_SIZE: usize = 50;

/// Which of a user's collections to enumerate. The path templates mirror the
/// endpoints the web player itself calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserCollection {
    /// Everything on the user's profile, reposts included.
    All,
    /// Own uploads only.
    Tracks,
    Albums,
    /// Playlists/sets the user created.
    Sets,
    Reposts,
    Likes,
    Spotlight,
    Comments,
}

impl UserCollection {
    fn path(self, user_id: i64) -> String {
        match self {
            UserCollection::All => format!("stream/users/{user_id}"),
            UserCollection::Tracks => format!("users/{user_id}/tracks"),
            UserCollection::Albums => format!("users/{user_id}/albums"),
            UserCollection::Sets => format!("users/{user_id}/playlists"),
            UserCollection::Reposts => format!("stream/users/{user_id}/reposts"),
            UserCollection::Likes => format!("users/{user_id}/likes"),
            UserCollection::Spotlight => format!("users/{user_id}/spotlight"),
            UserCollection::Comments => format!("users/{user_id}/comments"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            UserCollection::All => "all",
            UserCollection::Tracks => "tracks",
            UserCollection::Albums => "albums",
            UserCollection::Sets => "sets",
            UserCollection::Reposts => "reposts",
            UserCollection::Likes => "likes",
            UserCollection::Spotlight => "spotlight",
            UserCollection::Comments => "comments",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    All,
    Tracks,
    Users,
    Playlists,
    Albums,
}

impl SearchKind {
    fn endpoint(self) -> &'static str {
        match self {
            SearchKind::All => "search",
            SearchKind::Tracks => "search/tracks",
            SearchKind::Users => "search/users",
            SearchKind::Playlists => "search/playlists",
            SearchKind::Albums => "search/albums",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ClientConfig {
    /// Use this `client_id` instead of scraping one. Falls back to scraping if invalid.
    pub client_id: Option<String>,
    /// OAuth token, sent as `Authorization: OAuth <token>`.
    pub auth_token: Option<String>,
    pub timeout: Option<Duration>,
}

struct Inner {
    http: reqwest::Client,
    client_id: RwLock<Option<String>>,
    auth_token: Option<String>,
}

/// A cheap-to-clone handle to the SoundCloud API.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

/// Install rustls' crypto provider exactly once per process.
///
/// We select the `ring` provider rather than the default `aws-lc-rs` because
/// `aws-lc-sys` requires cmake to build, which is an unwelcome prerequisite for
/// anyone building this from source. Choosing a provider explicitly means
/// rustls will not pick one for us, so this must run before the first client.
fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // An error here means a provider was already installed by the host
        // application, which is fine — theirs wins.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

impl Client {
    pub fn new(config: ClientConfig) -> Result<Self> {
        install_crypto_provider();

        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(config.timeout.unwrap_or(Duration::from_secs(60)))
            .connect_timeout(Duration::from_secs(20))
            .build()?;

        Ok(Self {
            inner: Arc::new(Inner {
                http,
                client_id: RwLock::new(config.client_id.filter(|s| !s.trim().is_empty())),
                auth_token: config.auth_token.filter(|s| !s.trim().is_empty()),
            }),
        })
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    pub fn auth_token(&self) -> Option<&str> {
        self.inner.auth_token.as_deref()
    }

    pub async fn client_id(&self) -> Result<String> {
        if let Some(id) = self.inner.client_id.read().await.clone() {
            return Ok(id);
        }
        self.refresh_client_id().await
    }

    /// Scrape a fresh `client_id` from the web player's JS bundles.
    ///
    /// The bundles are tried newest-last-listed first, matching the web player's
    /// own load order — the id lives in one of the later chunks in practice.
    pub async fn refresh_client_id(&self) -> Result<String> {
        // Hold the write lock for the whole scrape so concurrent callers wait on
        // one refresh rather than each starting their own.
        let mut guard = self.inner.client_id.write().await;

        let page = self
            .inner
            .http
            .get(WEB_BASE)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;

        let script_re = Regex::new(r#"<script[^>]+src="([^"]+)""#).expect("static regex");
        let id_re = Regex::new(r#"client_id\s*:\s*"([0-9a-zA-Z]{32})""#).expect("static regex");

        let srcs: Vec<&str> = script_re
            .captures_iter(&page)
            .filter_map(|c| c.get(1).map(|m| m.as_str()))
            .collect();

        for src in srcs.into_iter().rev() {
            let Ok(script) = self.inner.http.get(src).send().await else {
                continue;
            };
            let Ok(script) = script.text().await else {
                continue;
            };
            if let Some(id) = id_re.captures(&script).and_then(|c| c.get(1)) {
                let id = id.as_str().to_string();
                *guard = Some(id.clone());
                return Ok(id);
            }
        }

        Err(Error::NoClientId)
    }

    /// Verify an OAuth token the same way the web player does.
    pub async fn verify_auth_token(&self, token: &str) -> Result<bool> {
        let client_id = self.client_id().await?;
        let resp = self
            .inner
            .http
            .post(AUTH_VERIFY)
            .query(&[("client_id", client_id.as_str())])
            .json(&serde_json::json!({ "session": { "access_token": token } }))
            .send()
            .await?;
        Ok(resp.status().is_success())
    }

    pub async fn is_client_id_valid(&self) -> bool {
        // `/resolve` on a known-good URL is the cheapest authenticated-ish probe.
        self.call_api_raw(
            &format!("{API_V2}resolve"),
            &[("url", "https://soundcloud.com/discover")],
        )
        .await
        .is_ok()
    }

    /// GET an API endpoint, injecting `client_id` and the auth header, and
    /// refreshing a stale `client_id` once before giving up.
    pub async fn call_api<T: DeserializeOwned>(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> Result<T> {
        let body = self.call_api_raw(url, query).await?;
        serde_json::from_str(&body).map_err(|source| Error::Json {
            context: url.to_string(),
            source,
        })
    }

    async fn call_api_raw(&self, url: &str, query: &[(&str, &str)]) -> Result<String> {
        let mut last_err = None;

        for attempt in 0..2 {
            let client_id = self.client_id().await?;
            let mut req = self.inner.http.get(url).query(query);

            // Do not send a second client_id if the caller's URL already carries
            // one (next_href values come back with it embedded).
            if !url.contains("client_id=") {
                req = req.query(&[("client_id", client_id.as_str())]);
            }
            if let Some(token) = &self.inner.auth_token {
                req = req.header("Authorization", format!("OAuth {token}"));
            }

            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(Error::Network(e));
                    continue;
                }
            };

            let status = resp.status();
            if status.is_success() {
                return Ok(resp.text().await?);
            }

            let err = Error::Http {
                status: status.as_u16(),
                url: url.to_string(),
            };

            if attempt == 0 && err.is_stale_credentials() {
                // Drop the cached id so refresh_client_id actually re-scrapes.
                *self.inner.client_id.write().await = None;
                self.refresh_client_id().await?;
                last_err = Some(err);
                continue;
            }

            return Err(err);
        }

        Err(last_err.unwrap_or(Error::NoClientId))
    }

    /// Turn a soundcloud.com URL into the thing it points at.
    pub async fn resolve(&self, url: &str) -> Result<Entity> {
        let value: serde_json::Value = self
            .call_api(&format!("{API_V2}resolve"), &[("url", url)])
            .await?;
        entity_from_value(value, url)
    }

    /// Hydrate `{"id": N}` stubs from a playlist into full track objects.
    ///
    /// Returned in the order requested: SoundCloud does not preserve the order of
    /// the `ids` parameter, and a playlist's running order is the whole point.
    pub async fn tracks_by_ids(&self, ids: &[i64]) -> Result<Vec<Track>> {
        let mut by_id: std::collections::HashMap<i64, Track> = std::collections::HashMap::new();

        for chunk in ids.chunks(ID_BATCH_SIZE) {
            let joined = chunk
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let batch: Vec<Track> = self
                .call_api(&format!("{API_V2}tracks"), &[("ids", joined.as_str())])
                .await?;
            for t in batch {
                by_id.insert(t.id, t);
            }
        }

        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// Walk a `linked_partitioning` collection, invoking `on_page` for each page.
    ///
    /// Stops early when `on_page` returns `false`, so callers honouring a `--max`
    /// or an offset do not keep paging for results they will discard.
    pub async fn paginate<T, F>(&self, start_url: &str, limit: usize, mut on_page: F) -> Result<()>
    where
        T: DeserializeOwned,
        F: FnMut(Vec<T>) -> bool,
    {
        let limit = limit.min(MAX_PAGE_LIMIT).to_string();
        let mut next: Option<String> = Some(start_url.to_string());
        let mut first = true;

        while let Some(url) = next.take() {
            // `offset` is only meaningful on the first request; afterwards the
            // cursor lives inside next_href and re-sending offset breaks paging.
            let query: Vec<(&str, &str)> = if first {
                vec![
                    ("limit", limit.as_str()),
                    ("linked_partitioning", "1"),
                    ("offset", "0"),
                ]
            } else {
                vec![]
            };
            first = false;

            let page: Page<T> = self.call_api(&url, &query).await?;
            let has_next = page.next_href.clone();
            if !on_page(page.collection) {
                return Ok(());
            }
            next = has_next;
        }

        Ok(())
    }

    /// All tracks in one of a user's collections, in API order.
    pub async fn user_collection_tracks(
        &self,
        user_id: i64,
        collection: UserCollection,
        max: Option<usize>,
    ) -> Result<Vec<Track>> {
        let url = format!("{}{}", API_V2, collection.path(user_id));
        let mut out: Vec<Track> = Vec::new();

        self.paginate::<serde_json::Value, _>(&url, MAX_PAGE_LIMIT, |items| {
            for item in items {
                if let Some(t) = track_from_collection_item(&item) {
                    out.push(t);
                }
                if max.is_some_and(|m| out.len() >= m) {
                    return false;
                }
            }
            true
        })
        .await?;

        if let Some(m) = max {
            out.truncate(m);
        }
        Ok(out)
    }

    /// All playlists in one of a user's collections.
    pub async fn user_collection_playlists(
        &self,
        user_id: i64,
        collection: UserCollection,
    ) -> Result<Vec<Playlist>> {
        let url = format!("{}{}", API_V2, collection.path(user_id));
        let mut out: Vec<Playlist> = Vec::new();

        self.paginate::<serde_json::Value, _>(&url, MAX_PAGE_LIMIT, |items| {
            for item in items {
                if let Some(p) = playlist_from_collection_item(&item) {
                    out.push(p);
                }
            }
            true
        })
        .await?;

        Ok(out)
    }

    /// Fetch a playlist by id, with its track stubs hydrated.
    pub async fn playlist(&self, id: i64, secret_token: Option<&str>) -> Result<Playlist> {
        let mut query: Vec<(&str, &str)> = Vec::new();
        if let Some(t) = secret_token {
            query.push(("secret_token", t));
        }
        let mut playlist: Playlist = self
            .call_api(&format!("{API_V2}playlists/{id}"), &query)
            .await?;
        self.hydrate_playlist(&mut playlist).await?;
        Ok(playlist)
    }

    /// Replace any id-only stubs in `playlist.tracks` with full track objects.
    pub async fn hydrate_playlist(&self, playlist: &mut Playlist) -> Result<()> {
        let needs_hydration: Vec<i64> = playlist
            .tracks
            .iter()
            .filter(|t| t.permalink_url.is_none() || t.media.is_none())
            .map(|t| t.id)
            .collect();

        if needs_hydration.is_empty() {
            return Ok(());
        }

        let fetched = self.tracks_by_ids(&needs_hydration).await?;
        let mut by_id: std::collections::HashMap<i64, Track> =
            fetched.into_iter().map(|t| (t.id, t)).collect();

        for slot in &mut playlist.tracks {
            if let Some(full) = by_id.remove(&slot.id) {
                *slot = full;
            }
        }

        Ok(())
    }

    pub async fn track(&self, id: i64, secret_token: Option<&str>) -> Result<Track> {
        let mut query: Vec<(&str, &str)> = Vec::new();
        if let Some(t) = secret_token {
            query.push(("secret_token", t));
        }
        self.call_api(&format!("{API_V2}tracks/{id}"), &query).await
    }

    /// The user the auth token belongs to.
    pub async fn me(&self) -> Result<User> {
        if self.inner.auth_token.is_none() {
            return Err(Error::InvalidAuthToken);
        }
        self.call_api(&format!("{API_V2}me"), &[]).await
    }

    pub async fn search(&self, kind: SearchKind, query: &str, limit: usize) -> Result<Vec<Entity>> {
        let url = format!("{}{}", API_V2, kind.endpoint());
        let limit_s = limit.min(MAX_PAGE_LIMIT).to_string();

        let page: Page<serde_json::Value> = self
            .call_api(
                &url,
                &[
                    ("q", query),
                    ("limit", limit_s.as_str()),
                    ("linked_partitioning", "1"),
                    ("offset", "0"),
                ],
            )
            .await?;

        let mut out = Vec::new();
        for item in page.collection {
            if let Ok(e) = entity_from_value(item, query) {
                out.push(e);
            }
            if out.len() >= limit {
                break;
            }
        }

        if out.is_empty() {
            return Err(Error::NoSearchResults(query.to_string()));
        }
        Ok(out)
    }
}

/// Classify a `/resolve` (or search) response into a [`Entity`].
///
/// `kind` is authoritative when present; otherwise we fall back to structural
/// hints, since some endpoints omit it.
fn entity_from_value(value: serde_json::Value, context: &str) -> Result<Entity> {
    let kind = value.get("kind").and_then(|k| k.as_str()).unwrap_or("");

    let parse = |label: &str| -> Result<serde_json::Value> {
        let _ = label;
        Ok(value.clone())
    };

    match kind {
        "track" => Ok(Entity::Track(Box::new(
            serde_json::from_value(parse("track")?).map_err(|source| Error::Json {
                context: context.to_string(),
                source,
            })?,
        ))),
        "playlist" | "system-playlist" => Ok(Entity::Playlist(Box::new(
            serde_json::from_value(parse("playlist")?).map_err(|source| Error::Json {
                context: context.to_string(),
                source,
            })?,
        ))),
        "user" => Ok(Entity::User(Box::new(
            serde_json::from_value(parse("user")?).map_err(|source| Error::Json {
                context: context.to_string(),
                source,
            })?,
        ))),
        _ => {
            // No usable `kind`: infer from shape.
            if value.get("tracks").is_some() || value.get("track_count").is_some() {
                serde_json::from_value(value)
                    .map(|p| Entity::Playlist(Box::new(p)))
                    .map_err(|source| Error::Json {
                        context: context.to_string(),
                        source,
                    })
            } else if value.get("media").is_some() || value.get("duration").is_some() {
                serde_json::from_value(value)
                    .map(|t| Entity::Track(Box::new(t)))
                    .map_err(|source| Error::Json {
                        context: context.to_string(),
                        source,
                    })
            } else if value.get("username").is_some() || value.get("permalink").is_some() {
                serde_json::from_value(value)
                    .map(|u| Entity::User(Box::new(u)))
                    .map_err(|source| Error::Json {
                        context: context.to_string(),
                        source,
                    })
            } else {
                Err(Error::UnsupportedUrl(context.to_string()))
            }
        }
    }
}

/// Stream endpoints wrap the interesting object: `{"type": "track-repost",
/// "track": {...}}`. Dig the track out wherever it is.
fn track_from_collection_item(item: &serde_json::Value) -> Option<Track> {
    for key in ["track", "origin"] {
        if let Some(inner) = item.get(key) {
            if inner.get("id").is_some() && inner.get("title").is_some() {
                if let Ok(t) = serde_json::from_value::<Track>(inner.clone()) {
                    return Some(t);
                }
            }
        }
    }
    if item.get("id").is_some() && item.get("title").is_some() && item.get("track_count").is_none()
    {
        return serde_json::from_value::<Track>(item.clone()).ok();
    }
    None
}

fn playlist_from_collection_item(item: &serde_json::Value) -> Option<Playlist> {
    for key in ["playlist", "origin"] {
        if let Some(inner) = item.get(key) {
            if inner.get("track_count").is_some() || inner.get("tracks").is_some() {
                if let Ok(p) = serde_json::from_value::<Playlist>(inner.clone()) {
                    return Some(p);
                }
            }
        }
    }
    if item.get("track_count").is_some() || item.get("tracks").is_some() {
        return serde_json::from_value::<Playlist>(item.clone()).ok();
    }
    None
}

/// Strip query/fragment noise and normalise to an absolute soundcloud.com URL.
pub fn normalize_soundcloud_url(input: &str) -> Result<String> {
    let trimmed = input.trim();
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else if trimmed.starts_with("soundcloud.com")
        || trimmed.starts_with("www.soundcloud.com")
        || trimmed.starts_with("m.soundcloud.com")
        || trimmed.starts_with("on.soundcloud.com")
    {
        format!("https://{trimmed}")
    } else if !trimmed.contains("://") && !trimmed.contains(' ') && trimmed.contains('/') {
        // Bare "user/track" permalink.
        format!("{WEB_BASE}{}", trimmed.trim_start_matches('/'))
    } else {
        return Err(Error::UnsupportedUrl(input.to_string()));
    };

    let mut url = Url::parse(&with_scheme).map_err(|_| Error::UnsupportedUrl(input.to_string()))?;

    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if !(host == "soundcloud.com"
        || host.ends_with(".soundcloud.com")
        || host == "snd.sc"
        || host.ends_with(".sndcdn.com"))
    {
        return Err(Error::UnsupportedUrl(input.to_string()));
    }

    // A secret_token in the query is meaningful; everything else is tracking junk.
    let secret: Option<String> = url
        .query_pairs()
        .find(|(k, _)| k == "secret_token")
        .map(|(_, v)| v.into_owned());
    url.set_query(None);
    url.set_fragment(None);
    if let Some(s) = secret {
        url.query_pairs_mut().append_pair("secret_token", &s);
    }

    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_bare_and_full_urls() {
        assert_eq!(
            normalize_soundcloud_url("soundcloud.com/a/b").unwrap(),
            "https://soundcloud.com/a/b"
        );
        assert_eq!(
            normalize_soundcloud_url("https://soundcloud.com/a/b?utm_source=x").unwrap(),
            "https://soundcloud.com/a/b"
        );
        assert_eq!(
            normalize_soundcloud_url("  https://m.soundcloud.com/a/b#frag ").unwrap(),
            "https://m.soundcloud.com/a/b"
        );
    }

    #[test]
    fn preserves_secret_token() {
        let out =
            normalize_soundcloud_url("https://soundcloud.com/a/b?secret_token=s-abc").unwrap();
        assert!(out.contains("secret_token=s-abc"), "got {out}");
    }

    #[test]
    fn rejects_non_soundcloud_hosts() {
        // Important: without this, --yt-dlp-args-style escapes aside, a malicious
        // playlist entry could point the downloader at an arbitrary host.
        assert!(normalize_soundcloud_url("https://evil.example.com/a/b").is_err());
        assert!(normalize_soundcloud_url("https://soundcloud.com.evil.tld/x").is_err());
    }

    #[test]
    fn user_collection_paths_match_web_player() {
        assert_eq!(UserCollection::All.path(7), "stream/users/7");
        assert_eq!(UserCollection::Tracks.path(7), "users/7/tracks");
        assert_eq!(UserCollection::Likes.path(7), "users/7/likes");
        assert_eq!(UserCollection::Sets.path(7), "users/7/playlists");
        assert_eq!(UserCollection::Reposts.path(7), "stream/users/7/reposts");
        assert_eq!(UserCollection::Comments.path(7), "users/7/comments");
    }

    #[test]
    fn entity_classification_uses_kind() {
        let t = entity_from_value(
            serde_json::json!({"kind": "track", "id": 1, "title": "t"}),
            "x",
        )
        .unwrap();
        assert!(matches!(t, Entity::Track(_)));

        let p = entity_from_value(
            serde_json::json!({"kind": "playlist", "id": 2, "title": "p"}),
            "x",
        )
        .unwrap();
        assert!(matches!(p, Entity::Playlist(_)));
    }

    #[test]
    fn entity_classification_falls_back_to_shape() {
        let p = entity_from_value(serde_json::json!({"id": 2, "track_count": 5}), "x").unwrap();
        assert!(matches!(p, Entity::Playlist(_)));
    }

    #[test]
    fn repost_items_unwrap_to_the_inner_track() {
        let item = serde_json::json!({
            "type": "track-repost",
            "track": {"id": 42, "title": "reposted"}
        });
        let t = track_from_collection_item(&item).expect("should find inner track");
        assert_eq!(t.id, 42);
    }
}
