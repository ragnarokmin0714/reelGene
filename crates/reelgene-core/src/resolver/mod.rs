//! The only site-specific seam in the program.
//!
//! Everything below this layer is generic HLS/HTTP plumbing. When a site changes,
//! a resolver is what breaks, and a resolver is all that has to be rewritten. If
//! a fix ever needs the engine, the store or `hls.rs` changed too, the
//! abstraction has leaked and that is the bug to fix.
//!
//! The trait is split in two, which is what lets an overnight resume work
//! against CDNs whose signed URLs expire in minutes:
//!
//! * [`Resolver::discover`] runs once, when a link is added: it names the
//!   series and lists its episodes.
//! * [`Resolver::resolve`] runs before *every* download attempt, every resume
//!   and every re-resolve after an expired URL included, and returns a URL that
//!   is valid now.
//!
//! The queue stores the recipe (`spec` + `hint`), never a URL.

pub mod direct;
mod extra;

use crate::error::{Error, Result};
use crate::http::Http;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    /// An M3U8 playlist; resume granularity is one segment.
    Hls,
    /// A single file; resume granularity is one byte.
    File,
}

#[derive(Debug, Clone)]
pub struct Discovered {
    /// The series, or the video's own title when there is one episode.
    pub title: String,
    /// Persisted verbatim and handed back to `resolve`. Everything needed to
    /// derive a fresh URL later must be in here.
    pub spec: Value,
    pub episodes: Vec<EpisodeRef>,
}

#[derive(Debug, Clone)]
pub struct EpisodeRef {
    /// As the site numbers it; naming templates use it.
    pub index: i64,
    pub title: String,
    /// The per-episode half of the recipe (an id, a path fragment, …).
    pub hint: Value,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub url: Url,
    pub kind: MediaKind,
    /// Headers this URL needs (a Referer check is common), sent with the
    /// playlist, its segments and its keys.
    pub headers: Vec<(String, String)>,
}

/// What a resolver may use while it works.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub http: &'a Http,
    pub cancel: &'a CancellationToken,
}

impl Ctx<'_> {
    pub async fn get_text(&self, url: &Url, headers: &[(String, String)]) -> Result<String> {
        self.http.get_text(url, headers, self.cancel).await
    }

    pub async fn get_json(&self, url: &Url, headers: &[(String, String)]) -> Result<Value> {
        let text = self.get_text(url, headers).await?;
        serde_json::from_str(&text)
            .map_err(|e| Error::Invalid(format!("{} is not JSON: {e}", crate::net::redact(url))))
    }
}

#[async_trait]
pub trait Resolver: Send + Sync {
    /// Stored with every job. Renaming it orphans the jobs already queued.
    fn id(&self) -> &'static str;

    /// Whether a link is this resolver's. Offline and cheap: it runs on every
    /// link the clipboard watcher sees.
    fn matches(&self, url: &Url) -> bool;

    async fn discover(&self, url: &Url, cx: Ctx<'_>) -> Result<Discovered>;

    async fn resolve(&self, spec: &Value, hint: &Value, cx: Ctx<'_>) -> Result<Resolved>;
}

/// Every resolver compiled into this build.
pub struct Registry {
    resolvers: Vec<Box<dyn Resolver>>,
}

impl Registry {
    /// Site resolvers first, the direct-URL resolver last as the catch-all.
    pub fn new() -> Self {
        let mut registry = Self {
            resolvers: Vec::new(),
        };
        extra::register(&mut registry);
        registry.add(Box::new(direct::Direct));
        registry
    }

    /// # Panics
    /// On a duplicate id: two resolvers claiming one id would send stored jobs
    /// to the wrong one, and that must fail in the first test run.
    pub fn add(&mut self, resolver: Box<dyn Resolver>) {
        assert!(
            self.get(resolver.id()).is_none(),
            "two resolvers share the id {:?}",
            resolver.id()
        );
        self.resolvers.push(resolver);
    }

    pub fn get(&self, id: &str) -> Option<&dyn Resolver> {
        self.resolvers
            .iter()
            .find(|r| r.id() == id)
            .map(|r| r.as_ref())
    }

    /// The first resolver that claims `url`, if any.
    pub fn find(&self, url: &Url) -> Option<&dyn Resolver> {
        self.resolvers
            .iter()
            .find(|r| r.matches(url))
            .map(|r| r.as_ref())
    }

    pub fn ids(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.resolvers.iter().map(|r| r.id())
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn media_links_go_to_the_direct_resolver_and_pages_to_nobody() {
        let registry = Registry::new();
        assert_eq!(
            registry
                .find(&u("https://cdn.example.com/a/index.m3u8"))
                .map(|r| r.id()),
            Some("direct")
        );
        assert!(
            registry
                .find(&u("https://site.example.com/drama/123"))
                .is_none(),
            "a web page is not a video file"
        );
        assert!(registry.get("direct").is_some());
        assert!(registry.ids().any(|id| id == "direct"));
    }

    #[test]
    #[should_panic(expected = "share the id")]
    fn duplicate_ids_are_refused() {
        let mut registry = Registry::new();
        registry.add(Box::new(direct::Direct));
    }
}
