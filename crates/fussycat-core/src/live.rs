//! Live blocks: re-fetched before every turn, rendered at the tail with
//! their fetch time. A failed fetch never aborts a turn (framework
//! `refresh`): the block renders stale, or failed if it never succeeded.

use serde::Serialize;

use crate::clock::{Timestamp, human_duration};
use crate::quilt::{Block, BlockDecl, BlockName, SourceError};

/// What a live block knows after this turn's fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    Fresh {
        content: String,
        fetched_at: Timestamp,
    },
    /// The fetch failed; this is the last good content.
    Stale {
        content: String,
        fetched_at: Timestamp,
        error: SourceError,
    },
    /// The fetch failed and no fetch has ever succeeded.
    Failed { error: SourceError },
}

/// One live block's fetch, as the turn report shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LiveReport {
    pub block: BlockName,
    /// `fresh`, `stale` or `failed`.
    pub status: &'static str,
    /// When the rendered content was fetched (absent when failed).
    pub fetched_at: Option<Timestamp>,
    pub error: Option<String>,
}

/// A live declaration plus its last good fetch, for the length of a run.
pub(crate) struct LiveBlock<'a> {
    decl: &'a BlockDecl,
    last_good: Option<(String, Timestamp)>,
}

impl<'a> LiveBlock<'a> {
    pub(crate) fn new(decl: &'a BlockDecl) -> Self {
        Self {
            decl,
            last_good: None,
        }
    }

    pub(crate) async fn refresh(&mut self, now: Timestamp) -> Freshness {
        match self.decl.source.fetch().await {
            Ok(content) => {
                self.last_good = Some((content.clone(), now));
                Freshness::Fresh {
                    content,
                    fetched_at: now,
                }
            }
            Err(error) => match &self.last_good {
                Some((content, fetched_at)) => Freshness::Stale {
                    content: content.clone(),
                    fetched_at: *fetched_at,
                    error,
                },
                None => Freshness::Failed { error },
            },
        }
    }

    pub(crate) fn name(&self) -> &BlockName {
        &self.decl.name
    }
}

/// The block as the model sees it at `now`.
pub(crate) fn render(name: &BlockName, freshness: &Freshness, now: Timestamp) -> Block {
    let name_attr = escape_attribute(name.as_str());
    let content = match freshness {
        Freshness::Fresh {
            content,
            fetched_at,
        } => format!(
            "<live name=\"{name_attr}\" fetched_at=\"{fetched_at}\">\n{}\n</live>",
            content.trim_end()
        ),
        Freshness::Stale {
            content,
            fetched_at,
            error,
        } => format!(
            "<live name=\"{name_attr}\" fetched_at=\"{fetched_at}\" status=\"stale\" age=\"{}\">\nrefresh failed: {error}\n{}\n</live>",
            human_duration(now.millis_since(*fetched_at)),
            content.trim_end()
        ),
        Freshness::Failed { error } => {
            format!("<live name=\"{name_attr}\" status=\"failed\">\nfetch failed: {error}\n</live>")
        }
    };
    Block {
        name: name.clone(),
        content,
    }
}

pub(crate) fn report(name: &BlockName, freshness: &Freshness) -> LiveReport {
    let (status, fetched_at, error) = match freshness {
        Freshness::Fresh { fetched_at, .. } => ("fresh", Some(*fetched_at), None),
        Freshness::Stale {
            fetched_at, error, ..
        } => ("stale", Some(*fetched_at), Some(error.to_string())),
        Freshness::Failed { error } => ("failed", None, Some(error.to_string())),
    };
    LiveReport {
        block: name.clone(),
        status,
        fetched_at,
        error,
    }
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quilt::{Fetch, Kind, Source};
    use std::sync::Mutex;

    /// Hands out scripted fetch results in order.
    struct Script(Mutex<Vec<Result<String, SourceError>>>);

    impl Source for Script {
        fn fetch(&self) -> Fetch<'_> {
            Box::pin(std::future::ready(self.0.lock().unwrap().remove(0)))
        }
    }

    fn decl(results: Vec<Result<&str, &str>>) -> BlockDecl {
        let results = results
            .into_iter()
            .map(|r| r.map(str::to_owned).map_err(|e| SourceError(e.into())))
            .collect();
        BlockDecl {
            name: BlockName::new("date -u").unwrap(),
            kind: Kind::Live,
            source: Box::new(Script(Mutex::new(results))),
        }
    }

    fn at(seconds: u64) -> Timestamp {
        Timestamp::from_unix_millis(seconds * 1000)
    }

    #[test]
    fn a_fetch_that_never_succeeded_renders_failed() {
        let decl = decl(vec![Err("boom")]);
        let mut live = LiveBlock::new(&decl);
        let freshness = crate::block_on(live.refresh(at(0)));
        assert_eq!(
            render(live.name(), &freshness, at(0)).content,
            "<live name=\"date -u\" status=\"failed\">\nfetch failed: boom\n</live>"
        );
        assert_eq!(report(live.name(), &freshness).status, "failed");
    }

    #[test]
    fn a_failed_refresh_keeps_the_last_good_content_and_says_how_old_it_is() {
        let decl = decl(vec![Ok("noon\n"), Err("boom")]);
        let mut live = LiveBlock::new(&decl);
        let fresh = crate::block_on(live.refresh(at(0)));
        assert_eq!(
            render(live.name(), &fresh, at(0)).content,
            "<live name=\"date -u\" fetched_at=\"1970-01-01T00:00:00Z\">\nnoon\n</live>"
        );
        let stale = crate::block_on(live.refresh(at(45)));
        assert_eq!(
            render(live.name(), &stale, at(45)).content,
            "<live name=\"date -u\" fetched_at=\"1970-01-01T00:00:00Z\" status=\"stale\" age=\"45s\">\nrefresh failed: boom\nnoon\n</live>"
        );
        let reported = report(live.name(), &stale);
        assert_eq!(
            (reported.status, reported.error),
            ("stale", Some("boom".into()))
        );
    }

    #[test]
    fn names_are_escaped_in_the_attribute() {
        let name = BlockName::new("echo \"a<b\" & c").unwrap();
        let failed = Freshness::Failed {
            error: SourceError("x".into()),
        };
        assert!(
            render(&name, &failed, at(0))
                .content
                .starts_with("<live name=\"echo &quot;a&lt;b&quot; &amp; c\"")
        );
    }
}
