//! Observation: what each turn cost and what the run did, reported as the
//! provider stated it and never estimated (INTENT principle 4).

use serde::Serialize;

use crate::live::LiveReport;
use crate::provider::{Request, Stop};
use crate::run::Outcome;

/// What one model call cost, as the provider reported it. `None` means
/// "not reported", never zero. `input` counts every prompt token; the
/// cache counts are parts of it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize)]
pub struct Usage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    /// In US dollars, when the provider reports it. Never computed from a
    /// price table here.
    pub cost_usd: Option<f64>,
}

impl Usage {
    /// The sum of two usages. A count is known only if both are: a total
    /// over turns where one went unreported would be an estimate.
    #[must_use]
    pub fn plus(self, other: Self) -> Self {
        fn add<T: std::ops::Add<Output = T>>(a: Option<T>, b: Option<T>) -> Option<T> {
            Some(a? + b?)
        }
        Self {
            input: add(self.input, other.input),
            output: add(self.output, other.output),
            cache_read: add(self.cache_read, other.cache_read),
            cache_write: add(self.cache_write, other.cache_write),
            cost_usd: add(self.cost_usd, other.cost_usd),
        }
    }
}

/// A block's identity on the wire, for comparing prefixes across turns and
/// runs (`jq` over `--metrics`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fingerprint {
    pub block: String,
    /// 64-bit FNV-1a of the block's neutral serialization, in hex. Stable
    /// across Rust versions and platforms, unlike `std`'s hasher.
    pub hash: String,
}

/// The name the tools block goes by in reports. Parenthesised so it can't
/// collide with a file or command name.
pub const TOOLS_BLOCK: &str = "(tools)";

/// Fingerprints of the cacheable prefix above the log: tools, then each
/// stable block. (A within-run divergence line arrives with pinned blocks:
/// until then nothing above the log can change during a run.)
#[must_use]
pub fn prefix_fingerprints(request: &Request) -> Vec<Fingerprint> {
    let tools = Fingerprint {
        block: TOOLS_BLOCK.to_owned(),
        hash: fingerprint(&request.tools),
    };
    let stable = request.stable.iter().map(|block| Fingerprint {
        block: block.name.to_string(),
        hash: fingerprint(&block.content),
    });
    std::iter::once(tools).chain(stable).collect()
}

/// One hash for the whole cacheable prefix: the same for every turn of a
/// run, and for every run of the same agent. Adapters whose provider routes
/// its cache by an explicit key send this.
#[must_use]
pub fn prefix_key(request: &Request) -> String {
    fingerprint(&prefix_fingerprints(request))
}

fn fingerprint(value: &impl Serialize) -> String {
    let bytes = serde_json::to_vec(value).expect("domain values always serialize");
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

/// One model call and the tool calls it asked for.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TurnReport {
    /// 1-based.
    pub turn: u32,
    pub elapsed_ms: u64,
    pub usage: Usage,
    pub tool_calls: usize,
    /// Failed provider attempts retried before this turn's response.
    pub retries: u32,
    /// Why the reply ended: `end`, or `output_limit` when it was cut off.
    pub stop: Stop,
    pub prefix: Vec<Fingerprint>,
    pub live: Vec<LiveReport>,
}

/// The whole run: the `dd`-style line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunSummary {
    pub turns: u32,
    pub elapsed_ms: u64,
    pub usage: Usage,
    /// Every turn's retries, summed.
    pub retries: u32,
}

/// Where reports go: stderr, a JSONL file, nowhere. Implemented by the
/// adapters; the core calls it once per turn and once at the end.
pub trait Reporter {
    fn turn(&mut self, report: &TurnReport);
    fn finish(&mut self, summary: &RunSummary, outcome: &Outcome);
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::{Outcome, Reporter, RunSummary, TurnReport};

    /// Keeps everything it's told.
    #[derive(Default)]
    pub struct Recorder {
        pub turns: Vec<TurnReport>,
        pub finished: Option<(RunSummary, Outcome)>,
    }

    impl Reporter for Recorder {
        fn turn(&mut self, report: &TurnReport) {
            self.turns.push(report.clone());
        }

        fn finish(&mut self, summary: &RunSummary, outcome: &Outcome) {
            self.finished = Some((summary.clone(), outcome.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quilt::{Block, BlockName};

    fn usage(input: Option<u64>, cache_read: Option<u64>) -> Usage {
        Usage {
            input,
            output: Some(1),
            cache_read,
            cache_write: None,
            cost_usd: Some(0.25),
        }
    }

    #[test]
    fn totals_are_known_only_when_every_part_was_reported() {
        let total = usage(Some(100), Some(0)).plus(usage(Some(120), Some(90)));
        assert_eq!(total.input, Some(220));
        assert_eq!(total.cache_read, Some(90));
        assert_eq!(total.cache_write, None);
        assert_eq!(total.cost_usd, Some(0.5));
        assert_eq!(
            usage(Some(1), None)
                .plus(usage(Some(1), Some(5)))
                .cache_read,
            None
        );
    }

    #[test]
    fn fingerprints_are_stable_fnv1a() {
        // FNV-1a 64 of the two bytes `""` (JSON for an empty string),
        // computed independently; a change here breaks cross-run comparison.
        assert_eq!(fingerprint(&""), "07cc7607b4949e25");
    }

    #[test]
    fn prefix_fingerprints_cover_tools_then_stable_blocks() {
        let request = Request {
            tools: vec![],
            stable: vec![Block {
                name: BlockName::new("core.md").unwrap(),
                content: "be terse".into(),
            }],
            log: vec![],
            live: vec![],
            max_output_tokens: None,
        };
        let names: Vec<_> = prefix_fingerprints(&request)
            .into_iter()
            .map(|f| f.block)
            .collect();
        assert_eq!(names, [TOOLS_BLOCK, "core.md"]);
    }

    #[test]
    fn the_prefix_key_ignores_the_log_and_live_tail_but_not_the_prefix() {
        let block = |name: &str, content: &str| Block {
            name: BlockName::new(name).unwrap(),
            content: content.into(),
        };
        let turn_one = Request {
            tools: vec![],
            stable: vec![block("core.md", "be terse")],
            log: vec![crate::log::Entry::Task("hi".into())],
            live: vec![block("date", "monday")],
            max_output_tokens: None,
        };
        let mut turn_two = turn_one.clone();
        turn_two.log.push(crate::log::Entry::Task("more".into()));
        turn_two.live = vec![block("date", "tuesday")];
        assert_eq!(prefix_key(&turn_one), prefix_key(&turn_two));

        let mut other_agent = turn_one.clone();
        other_agent.stable = vec![block("core.md", "be verbose")];
        assert_ne!(prefix_key(&turn_one), prefix_key(&other_agent));
    }
}
