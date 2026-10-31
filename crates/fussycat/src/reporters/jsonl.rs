use std::io::Write;

use fussycat_core::clock::Timestamp;
use fussycat_core::report::{Reporter, RunSummary, TurnReport};
use fussycat_core::run::Outcome;
use serde_json::{Value, json};

/// The `--metrics` format's version, on every line. It goes up when a
/// field changes meaning or goes away; a new field doesn't change it, so a
/// script should ignore fields it doesn't know.
pub const METRICS_VERSION: u32 = 1;

/// `--metrics FILE`: one JSON object per line, appended, for `jq`. Every
/// line carries the format's version, the run's id, its start time and its
/// model, so lines from many runs (a daily cron, or several at once)
/// group cleanly.
pub struct JsonlReporter<W> {
    out: W,
    run: String,
    started: String,
    model: String,
}

impl<W: Write> JsonlReporter<W> {
    /// `run` names this run alone ([`run_id`]); `started` is when it began.
    pub fn new(out: W, run: String, started: Timestamp, model: &str) -> Self {
        Self {
            out,
            run,
            started: started.to_string(),
            model: model.to_owned(),
        }
    }

    /// One line, in one write: runs appending to one file at once never
    /// interleave inside a line (the file is opened to append).
    fn write(&mut self, kind: &str, body: Value) {
        let mut line = json!({
            "version": METRICS_VERSION, "type": kind, "run": self.run,
            "started": self.started, "model": self.model,
        });
        if let (Some(line), Value::Object(body)) = (line.as_object_mut(), body) {
            line.extend(body);
        }
        let line = format!("{line}\n");
        if let Err(error) = self.out.write_all(line.as_bytes()) {
            super::warn(format_args!("--metrics: {error}"));
        }
    }
}

/// A run's id: 16 hex digits from the process's random hash keys, so runs
/// started in the same second, or on two machines, get different ones.
#[must_use]
pub fn run_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::hash::RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    format!("{:016x}", hasher.finish())
}

impl<W: Write> Reporter for JsonlReporter<W> {
    fn turn(&mut self, report: &TurnReport) {
        self.write("turn", serde_json::to_value(report).unwrap_or_default());
    }

    fn finish(&mut self, summary: &RunSummary, outcome: &Outcome) {
        let mut body = serde_json::to_value(summary).unwrap_or_default();
        if let Some(body) = body.as_object_mut() {
            body.insert("outcome".into(), outcome.name().into());
            if let Some(reason) = outcome.reason() {
                body.insert("reason".into(), json!(reason));
            }
            if let Some(detail) = detail(outcome) {
                body.insert("detail".into(), detail.into());
            }
        }
        self.write("summary", body);
    }
}

/// Why a run didn't answer. The answer itself belongs on stdout only.
fn detail(outcome: &Outcome) -> Option<String> {
    match outcome {
        Outcome::Answered(_) => None,
        Outcome::Exhausted(limit) => Some(limit.to_string()),
        Outcome::Failed(failure) => Some(failure.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fussycat_core::provider::Stop;
    use fussycat_core::report::{Fingerprint, Usage};
    use fussycat_core::run::Limit;

    fn lines(act: impl FnOnce(&mut JsonlReporter<&mut Vec<u8>>)) -> Vec<Value> {
        let mut out = Vec::new();
        act(&mut JsonlReporter::new(
            &mut out,
            "0123456789abcdef".into(),
            Timestamp::from_unix_millis(0),
            "gpt-oss:20b",
        ));
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn a_turn_is_one_line_with_its_run_and_model() {
        let report = TurnReport {
            turn: 1,
            elapsed_ms: 1_200,
            usage: Usage {
                input: Some(167),
                output: Some(69),
                cache_read: Some(0),
                cache_write: None,
                cost_usd: None,
            },
            tool_calls: 1,
            skipped_tool_calls: 0,
            retries: 1,
            stop: Some(Stop::OutputLimit),
            prefix: vec![Fingerprint {
                block: "(tools)".into(),
                hash: "07cc7607b4949e25".into(),
            }],
            live: vec![],
        };
        let lines = lines(|r| r.turn(&report));
        assert_eq!(
            lines,
            [json!({
                "version": 1, "type": "turn", "run": "0123456789abcdef",
                "started": "1970-01-01T00:00:00Z", "model": "gpt-oss:20b",
                "turn": 1, "elapsed_ms": 1200, "tool_calls": 1, "skipped_tool_calls": 0, "retries": 1,
                "stop": "output_limit",
                "usage": {"input": 167, "output": 69, "cache_read": 0, "cache_write": null, "cost_usd": null},
                "prefix": [{"block": "(tools)", "hash": "07cc7607b4949e25"}],
                "live": []
            })]
        );
    }

    #[test]
    fn the_summary_says_how_it_ended_but_never_the_answer() {
        let summary = RunSummary {
            turns: 2,
            elapsed_ms: 3_000,
            usage: Usage::default(),
            retries: 0,
        };
        let answered = lines(|r| r.finish(&summary, &Outcome::Answered("secret".into())));
        assert_eq!(answered[0]["outcome"], "answered");
        assert!(!answered[0].to_string().contains("secret"));
        assert_eq!(answered[0].get("reason"), None);
        let exhausted = lines(|r| r.finish(&summary, &Outcome::Exhausted(Limit::MaxTurns(2))));
        assert_eq!(exhausted[0]["outcome"], "exhausted");
        assert_eq!(
            exhausted[0]["reason"],
            json!({"kind": "max_turns", "turns": 2})
        );
        assert_eq!(exhausted[0]["detail"], "max turns (2) reached");
    }

    /// Each line reaches the file in one write, so concurrent runs
    /// appending to one file can't interleave within a line.
    #[test]
    fn each_line_is_one_write() {
        struct Writes(Vec<usize>);
        impl Write for Writes {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.push(bytes.len());
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writes = Writes(Vec::new());
        let summary = RunSummary {
            turns: 1,
            elapsed_ms: 1,
            usage: Usage::default(),
            retries: 0,
        };
        JsonlReporter::new(&mut writes, run_id(), Timestamp::from_unix_millis(0), "m")
            .finish(&summary, &Outcome::Answered("hi".into()));
        assert_eq!(writes.0.len(), 1, "{:?}", writes.0);
    }

    #[test]
    fn run_ids_differ_between_runs() {
        let (a, b) = (run_id(), run_id());
        assert_ne!(a, b);
        assert_eq!(a.len(), 16);
    }
}
