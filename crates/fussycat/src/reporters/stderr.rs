use std::io::Write;

use fussycat_core::provider::Stop;
use fussycat_core::report::{Reporter, RunSummary, TurnReport, Usage};
use fussycat_core::run::Outcome;

use super::outcome_name;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    /// Nothing (`-q`).
    Quiet,
    /// The one-line summary, like `dd`.
    Summary,
    /// A line per turn as well (`-v`).
    Verbose,
}

/// Human-readable reports on stderr. Stdout stays the answer alone.
pub struct StderrReporter<W> {
    verbosity: Verbosity,
    out: W,
}

impl<W: Write> StderrReporter<W> {
    pub fn new(verbosity: Verbosity, out: W) -> Self {
        Self { verbosity, out }
    }

    /// Diagnostics are best effort: a closed stderr must not fail the run.
    fn line(&mut self, text: &str) {
        let _ = writeln!(self.out, "{}: {text}", crate::NAME);
    }
}

impl<W: Write> Reporter for StderrReporter<W> {
    fn turn(&mut self, report: &TurnReport) {
        if self.verbosity == Verbosity::Verbose {
            self.line(&turn_line(report));
        }
    }

    fn finish(&mut self, summary: &RunSummary, outcome: &Outcome) {
        if self.verbosity != Verbosity::Quiet {
            self.line(&summary_line(summary, outcome));
        }
    }
}

fn turn_line(report: &TurnReport) -> String {
    let mut line = format!(
        "turn {}: {}, {} tool call{}{}{}; {}",
        report.turn,
        seconds(report.elapsed_ms),
        report.tool_calls,
        if report.tool_calls == 1 { "" } else { "s" },
        retries(report.retries, ", "),
        match report.stop {
            Stop::End => "",
            Stop::OutputLimit => ", cut off at the output limit",
        },
        tokens(&report.usage)
    );
    if !report.live.is_empty() {
        let statuses: Vec<String> = report
            .live
            .iter()
            .map(|live| format!("{} {}", live.block, live.status))
            .collect();
        line.push_str("; live: ");
        line.push_str(&statuses.join(", "));
    }
    line
}

/// No tokens part when no turn finished: there's nothing to count, and
/// `tokens in ?` would only bury the error that follows.
fn summary_line(summary: &RunSummary, outcome: &Outcome) -> String {
    let line = format!(
        "{} after {} turn{} in {}{}",
        outcome_name(outcome),
        summary.turns,
        if summary.turns == 1 { "" } else { "s" },
        seconds(summary.elapsed_ms),
        retries(summary.retries, " with "),
    );
    if summary.turns == 0 {
        line
    } else {
        format!("{line}; {}", tokens(&summary.usage))
    }
}

/// `, 2 retries`; nothing when there were none.
fn retries(count: u32, lead: &str) -> String {
    match count {
        0 => String::new(),
        1 => format!("{lead}1 retry"),
        n => format!("{lead}{n} retries"),
    }
}

/// `tokens in 629 (628 cached, 99%), out 2, $0.000042`. Unreported counts
/// show as `?`; an unreported cost is left out (normal for Ollama), and is
/// `null` in `--metrics`. Cache writes show only when a provider reports
/// some (Anthropic): `(840 cached, 48%; 893 written)`.
fn tokens(usage: &Usage) -> String {
    let count = |n: Option<u64>| n.map_or_else(|| "?".to_owned(), |n| n.to_string());
    let written = match usage.cache_write {
        Some(written) if written > 0 => format!("; {written} written"),
        _ => String::new(),
    };
    let cached = match (usage.cache_read, usage.input) {
        (Some(read), Some(input)) if input > 0 => {
            format!(" ({read} cached, {}%{written})", read * 100 / input)
        }
        (Some(read), _) => format!(" ({read} cached{written})"),
        (None, _) => " (cache not reported)".to_owned(),
    };
    let cost = usage
        .cost_usd
        .map_or_else(String::new, |usd| format!(", ${usd:.6}"));
    format!(
        "tokens in {}{cached}, out {}{cost}",
        count(usage.input),
        count(usage.output)
    )
}

/// `4.2s`: tenths, from integer milliseconds.
fn seconds(millis: u64) -> String {
    format!("{}.{}s", millis / 1000, millis % 1000 / 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fussycat_core::live::LiveReport;
    use fussycat_core::quilt::BlockName;
    use fussycat_core::run::Limit;

    fn usage() -> Usage {
        Usage {
            input: Some(629),
            output: Some(2),
            cache_read: Some(628),
            cache_write: None,
            cost_usd: None,
        }
    }

    fn summary() -> RunSummary {
        RunSummary {
            turns: 3,
            elapsed_ms: 4_250,
            usage: usage(),
            retries: 0,
        }
    }

    fn written(
        verbosity: Verbosity,
        act: impl FnOnce(&mut StderrReporter<&mut Vec<u8>>),
    ) -> String {
        let mut out = Vec::new();
        act(&mut StderrReporter::new(verbosity, &mut out));
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn the_summary_reads_like_dd() {
        assert_eq!(
            summary_line(&summary(), &Outcome::Answered("hi".into())),
            "answered after 3 turns in 4.2s; tokens in 629 (628 cached, 99%), out 2"
        );
    }

    #[test]
    fn cache_writes_show_only_when_reported() {
        let written = Usage {
            input: Some(1739),
            cache_read: Some(840),
            cache_write: Some(893),
            ..usage()
        };
        assert_eq!(
            tokens(&written),
            "tokens in 1739 (840 cached, 48%; 893 written), out 2"
        );
        let none = Usage {
            cache_write: Some(0),
            ..usage()
        };
        assert_eq!(tokens(&none), "tokens in 629 (628 cached, 99%), out 2");
    }

    #[test]
    fn a_reported_cost_joins_the_line() {
        let priced = RunSummary {
            usage: Usage {
                cost_usd: Some(0.000_042),
                ..usage()
            },
            ..summary()
        };
        assert_eq!(
            summary_line(&priced, &Outcome::Answered("hi".into())),
            "answered after 3 turns in 4.2s; tokens in 629 (628 cached, 99%), out 2, $0.000042"
        );
    }

    #[test]
    fn retries_show_only_when_there_were_some() {
        let flaky = RunSummary {
            retries: 2,
            ..summary()
        };
        assert_eq!(
            summary_line(&flaky, &Outcome::Answered("hi".into())),
            "answered after 3 turns in 4.2s with 2 retries; tokens in 629 (628 cached, 99%), out 2"
        );
    }

    #[test]
    fn a_run_that_never_finished_a_turn_has_no_tokens_to_count() {
        let nothing = RunSummary {
            turns: 0,
            elapsed_ms: 40,
            usage: Usage::default(),
            retries: 0,
        };
        let failed = Outcome::Failed(fussycat_core::run::Failure::Provider(
            fussycat_core::provider::ProviderError::new("HTTP 404: no such model"),
        ));
        assert_eq!(
            summary_line(&nothing, &failed),
            "failed after 0 turns in 0.0s"
        );
    }

    #[test]
    fn unreported_counts_are_unknown_not_zero() {
        let unknown = RunSummary {
            turns: 1,
            elapsed_ms: 900,
            usage: Usage::default(),
            retries: 0,
        };
        assert_eq!(
            summary_line(&unknown, &Outcome::Exhausted(Limit::MaxTurns(1))),
            "exhausted after 1 turn in 0.9s; tokens in ? (cache not reported), out ?"
        );
    }

    #[test]
    fn verbose_adds_a_line_per_turn_with_live_status() {
        let report = TurnReport {
            turn: 2,
            elapsed_ms: 1_300,
            usage: usage(),
            tool_calls: 1,
            retries: 0,
            stop: Stop::End,
            prefix: vec![],
            live: vec![LiveReport {
                block: BlockName::new("date -u").unwrap(),
                status: "stale",
                fetched_at: None,
                error: Some("boom".into()),
            }],
        };
        let text = written(Verbosity::Verbose, |r| r.turn(&report));
        assert_eq!(
            text,
            "fussycat: turn 2: 1.3s, 1 tool call; tokens in 629 (628 cached, 99%), out 2; live: date -u stale\n"
        );
        assert_eq!(written(Verbosity::Summary, |r| r.turn(&report)), "");
    }

    #[test]
    fn a_turn_cut_off_at_the_output_limit_says_so() {
        let report = TurnReport {
            turn: 1,
            elapsed_ms: 900,
            usage: usage(),
            tool_calls: 0,
            retries: 0,
            stop: Stop::OutputLimit,
            prefix: vec![],
            live: vec![],
        };
        assert_eq!(
            turn_line(&report),
            "turn 1: 0.9s, 0 tool calls, cut off at the output limit; tokens in 629 (628 cached, 99%), out 2"
        );
    }

    #[test]
    fn quiet_says_nothing() {
        let text = written(Verbosity::Quiet, |r| {
            r.finish(&summary(), &Outcome::Answered("hi".into()));
        });
        assert_eq!(text, "");
    }
}
