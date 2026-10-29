use std::io::Write;

use fussycat_core::clock::Timestamp;
use fussycat_core::report::{Reporter, RunSummary, TurnReport};
use fussycat_core::run::Outcome;
use serde_json::{Value, json};

use super::outcome_name;

/// `--metrics FILE`: one JSON object per line, appended, for `jq`. Every
/// line carries the run's start time and model, so lines from many runs
/// (a daily cron) group cleanly.
pub struct JsonlReporter<W> {
    out: W,
    run: String,
    model: String,
}

impl<W: Write> JsonlReporter<W> {
    pub fn new(out: W, run_started: Timestamp, model: &str) -> Self {
        Self {
            out,
            run: run_started.to_string(),
            model: model.to_owned(),
        }
    }

    fn write(&mut self, kind: &str, body: Value) {
        let mut line = json!({"type": kind, "run": self.run, "model": self.model});
        if let (Some(line), Value::Object(body)) = (line.as_object_mut(), body) {
            line.extend(body);
        }
        if let Err(error) = writeln!(self.out, "{line}").and_then(|()| self.out.flush()) {
            super::warn(format_args!("--metrics: {error}"));
        }
    }
}

impl<W: Write> Reporter for JsonlReporter<W> {
    fn turn(&mut self, report: &TurnReport) {
        self.write("turn", serde_json::to_value(report).unwrap_or_default());
    }

    fn finish(&mut self, summary: &RunSummary, outcome: &Outcome) {
        let mut body = serde_json::to_value(summary).unwrap_or_default();
        if let Some(body) = body.as_object_mut() {
            body.insert("outcome".into(), outcome_name(outcome).into());
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
                "type": "turn", "run": "1970-01-01T00:00:00Z", "model": "gpt-oss:20b",
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
        let exhausted = lines(|r| r.finish(&summary, &Outcome::Exhausted(Limit::MaxTurns(2))));
        assert_eq!(exhausted[0]["detail"], "max turns (2) reached");
    }
}
