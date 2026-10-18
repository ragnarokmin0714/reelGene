//! One error type for the whole core.
//!
//! It is typed, not stringly: the engine has to tell three kinds of failure apart
//! without parsing messages — a transient one (retry it), an expired signed URL
//! (re-resolve and carry on), and a permanent one (fail the episode and say why).

use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Connection refused or reset, DNS failure, timeout, truncated body.
    #[error("network error: {0}")]
    Network(String),

    /// A non-success HTTP status. `url` is already redacted (no query string).
    #[error("HTTP {status} from {url}")]
    Status {
        status: u16,
        url: String,
        retry_after: Option<Duration>,
    },

    #[error("bad playlist: {0}")]
    Playlist(String),

    /// The content is DRM-protected. reelgene does not download it, by design.
    #[error("protected by DRM ({0}); reelgene does not download DRM-protected content")]
    Drm(String),

    #[error("not supported: {0}")]
    Unsupported(String),

    /// A site resolver failed — usually because the site changed.
    #[error("resolver {resolver}: {message}")]
    Resolver { resolver: String, message: String },

    #[error("decryption failed: {0}")]
    Decrypt(String),

    #[error("storage: {0}")]
    Store(String),

    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A response bigger than anything legitimate of its kind.
    #[error("too large: {0}")]
    TooLarge(String),

    #[error("cancelled")]
    Cancelled,

    #[error("{0}")]
    Invalid(String),
}

impl Error {
    /// Worth trying again unchanged: the network, rate limits, server errors.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Network(_) => true,
            Error::Status { status, .. } => matches!(status, 408 | 425 | 429 | 500..=599),
            _ => false,
        }
    }

    /// What a signed URL past its expiry answers. Re-resolving gets a fresh one.
    pub fn is_expired_url(&self) -> bool {
        matches!(
            self,
            Error::Status {
                status: 401 | 403 | 410,
                ..
            }
        )
    }

    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }

    pub fn resolver(resolver: &str, message: impl Into<String>) -> Self {
        Error::Resolver {
            resolver: resolver.to_owned(),
            message: message.into(),
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Store(e.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    fn status(code: u16) -> Error {
        Error::Status {
            status: code,
            url: "https://cdn.example/x".into(),
            retry_after: None,
        }
    }

    #[test]
    fn transient_means_rate_limits_server_errors_and_the_network() {
        assert!(status(429).is_transient());
        assert!(status(503).is_transient());
        assert!(status(408).is_transient());
        assert!(Error::Network("reset".into()).is_transient());
        assert!(!status(404).is_transient(), "a 404 will not fix itself");
        assert!(!status(403).is_transient());
    }

    #[test]
    fn expired_url_is_the_signature_family_not_not_found() {
        assert!(status(403).is_expired_url());
        assert!(status(410).is_expired_url());
        assert!(status(401).is_expired_url());
        assert!(!status(404).is_expired_url());
        assert!(!status(500).is_expired_url());
    }
}
