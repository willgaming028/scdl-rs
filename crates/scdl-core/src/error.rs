use std::path::PathBuf;

/// Errors produced by `scdl-core`.
///
/// Variants are deliberately fine-grained where a caller might want to react
/// differently — notably [`Error::Http`] carrying a status, which the API client
/// uses to decide whether a stale `client_id` should be refreshed and the request
/// retried.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("HTTP {status} from {url}")]
    Http { status: u16, url: String },

    #[error("could not parse response from {context}: {source}")]
    Json {
        context: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("could not extract a client_id from soundcloud.com")]
    NoClientId,

    #[error("invalid auth token")]
    InvalidAuthToken,

    #[error("{0} is not a SoundCloud URL we understand")]
    UnsupportedUrl(String),

    #[error("no results for search query {0:?}")]
    NoSearchResults(String),

    #[error("track {title:?} has no downloadable format matching your filters")]
    NoFormat { title: String },

    #[error("track {title:?} is DRM-protected and cannot be downloaded")]
    Drm { title: String },

    #[error("track {title:?} is only available as a 30-second preview (SoundCloud Go+)")]
    Snipped { title: String },

    #[error("ffmpeg is required to produce {0} but was not found on PATH")]
    FfmpegMissing(String),

    #[error("ffmpeg exited with status {status}: {stderr}")]
    Ffmpeg { status: String, stderr: String },

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    PlainIo(#[from] std::io::Error),

    // lofty 0.25 dropped its unified `LoftyError` enum in favour of per-operation
    // error types, so we box whatever the failing call produced.
    #[error("tagging {path} failed: {source}")]
    Tag {
        path: PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("bad filename template: {0}")]
    Template(String),

    #[error("refusing unsafe output path {0}: it escapes the download directory")]
    UnsafePath(PathBuf),

    #[error("archive file {path} is malformed at line {line}: {reason}")]
    MalformedArchive {
        path: PathBuf,
        line: usize,
        reason: String,
    },

    #[error("config error: {0}")]
    Config(String),

    #[error("download was cancelled")]
    Cancelled,
}

impl Error {
    /// True when the failure is plausibly caused by a `client_id` that SoundCloud
    /// has rotated out from under us, meaning a refresh-and-retry is worth trying.
    pub fn is_stale_credentials(&self) -> bool {
        matches!(
            self,
            Error::Http {
                status: 401 | 403,
                ..
            }
        )
    }

    /// True for transient failures worth retrying with backoff.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Http { status, .. } => matches!(status, 429 | 500 | 502 | 503 | 504),
            Error::Network(e) => e.is_timeout() || e.is_connect(),
            _ => false,
        }
    }

    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub(crate) fn tag(
        path: impl Into<PathBuf>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Error::Tag {
            path: path.into(),
            source: Box::new(source),
        }
    }
}
