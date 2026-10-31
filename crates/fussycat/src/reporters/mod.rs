//! Reporter adapters: where turn reports and the run summary go.

mod jsonl;
mod stderr;

pub use jsonl::{JsonlReporter, run_id};
pub use stderr::{StderrReporter, Verbosity};

use std::fmt::Display;
use std::io::Write;

use fussycat_core::report::{Reporter, RunSummary, TurnReport};
use fussycat_core::run::Outcome;

/// One line on stderr, prefixed with the program's name. Best effort: a
/// closed stderr must neither fail the run nor panic (`eprintln!` would).
pub fn warn(message: impl Display) {
    let _ = writeln!(std::io::stderr(), "{}: {message}", crate::NAME);
}

/// Sends every report to each reporter, in order.
#[derive(Default)]
pub struct Reporters(Vec<Box<dyn Reporter>>);

impl Reporters {
    pub fn push(&mut self, reporter: impl Reporter + 'static) {
        self.0.push(Box::new(reporter));
    }
}

impl Reporter for Reporters {
    fn turn(&mut self, report: &TurnReport) {
        for reporter in &mut self.0 {
            reporter.turn(report);
        }
    }

    fn finish(&mut self, summary: &RunSummary, outcome: &Outcome) {
        for reporter in &mut self.0 {
            reporter.finish(summary, outcome);
        }
    }
}
