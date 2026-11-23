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
pub mod page;
pub mod pasted;
mod title;
pub mod ytdlp;

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

impl Discovered {
    /// One video, not an episode of a series: a single episode that only
    /// repeats the title. One a title was split into (`劇名 第5集` → series
    /// and `第5集`) still belongs under its series.
    pub fn is_single(&self) -> bool {
        is_single(&self.title, &self.episodes)
    }

    /// Name it; a single video's episode takes the name too, or its file
    /// would keep the one guessed from the link.
    pub fn retitle(&mut self, title: String) {
        retitle(&mut self.title, &mut self.episodes, title);
    }
}

pub(crate) fn is_single(title: &str, episodes: &[EpisodeRef]) -> bool {
    matches!(episodes, [ep] if ep.title == title)
}

pub(crate) fn retitle(title: &mut String, episodes: &mut [EpisodeRef], new: String) {
    if let [ep] = episodes
        && ep.title == *title
    {
        ep.title.clone_from(&new);
    }
    *title = new;
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

    /// The playlist itself, when the recipe holds it (one the user pasted)
    /// rather than `Resolved::url` naming where to fetch it. The URL then
    /// only stands for where it came from.
    fn playlist_text<'a>(&self, _spec: &'a Value) -> Option<&'a str> {
        None
    }
}

/// Every resolver compiled into this build.
pub struct Registry {
    resolvers: Vec<Box<dyn Resolver>>,
    /// Asked in turn when no resolver claims a link the user added: the
    /// page's own `<video>`, then yt-dlp.
    fallbacks: Vec<&'static str>,
}

impl Registry {
    /// Site resolvers first, the direct-URL resolver last as the catch-all.
    pub fn new() -> Self {
        let mut registry = Self {
            resolvers: Vec::new(),
            fallbacks: Vec::new(),
        };
        extra::register(&mut registry);
        registry.add(Box::new(direct::Direct));
        registry.add(Box::new(page::Page));
        registry.add(Box::new(pasted::Pasted));
        registry.add_fallback(page::ID);
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

    /// Try the resolver `id` (already added), after those added before it,
    /// for links nobody claims, when the user adds them. Never for links
    /// found on the clipboard.
    pub fn add_fallback(&mut self, id: &'static str) {
        assert!(self.get(id).is_some(), "no resolver {id:?} to fall back on");
        self.fallbacks.push(id);
    }

    pub fn fallbacks(&self) -> impl Iterator<Item = &dyn Resolver> + '_ {
        self.fallbacks.iter().filter_map(|id| self.get(id))
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
    use crate::engine::Discovery;

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

    fn found(title: &str, episodes: &[&str]) -> Discovered {
        Discovered {
            title: title.into(),
            spec: Value::Null,
            episodes: episodes
                .iter()
                .enumerate()
                .map(|(i, t)| EpisodeRef {
                    index: i as i64 + 1,
                    title: (*t).into(),
                    hint: Value::Null,
                })
                .collect(),
        }
    }

    #[test]
    fn a_single_video_is_one_episode_named_only_by_its_title() {
        assert!(found("clip", &["clip"]).is_single());
        assert!(
            !found("大劇", &["第5集"]).is_single(),
            "an episode split from its series' title stays under the series"
        );
        assert!(!found("show", &["Episode 1", "Episode 2"]).is_single());
        assert!(!found("show", &[]).is_single());
    }

    #[test]
    fn retitling_a_single_video_renames_its_episode_too() {
        let mut single = found("abc2", &["abc2"]);
        single.retitle("A page's title".into());
        assert_eq!(single.title, "A page's title");
        assert_eq!(single.episodes[0].title, "A page's title");
        assert!(single.is_single(), "still one video");

        let mut episode = found("大劇", &["第5集"]);
        episode.retitle("大劇 第一季".into());
        assert_eq!(
            episode.episodes[0].title, "第5集",
            "the episode keeps its own"
        );
    }

    #[test]
    fn a_job_of_one_episode_picked_from_a_series_is_not_single() {
        let series = Discovery {
            resolver: "direct".into(),
            found: found("x", &["x", "x"]),
        };
        let mut job = series.into_job(u("https://site.example/show"), "/d".into());
        job.episodes.truncate(1);
        assert!(!job.is_single());
        assert_eq!(job.template, crate::paths::DEFAULT_TEMPLATE);

        let one = Discovery {
            resolver: "direct".into(),
            found: found("clip", &["clip"]),
        };
        let mut job = one.into_job(u("https://cdn.example/clip.mp4"), "/d".into());
        assert!(job.is_single());
        assert_eq!(job.template, crate::paths::SINGLE_TEMPLATE);
        job.retitle("Named".into());
        assert_eq!(job.episodes[0].title, "Named");
    }

    #[test]
    #[should_panic(expected = "share the id")]
    fn duplicate_ids_are_refused() {
        let mut registry = Registry::new();
        registry.add(Box::new(direct::Direct));
    }
}
