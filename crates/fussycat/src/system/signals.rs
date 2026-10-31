//! The signals that stop a run: SIGINT (Ctrl-C) and SIGTERM (`kill`,
//! `timeout`). Every command runs in a process group of its own, out of
//! reach of a signal to fussycat's, so fussycat passes these on before it goes.
//!
//! Not SIGHUP. Handling a signal replaces what fussycat inherited, and
//! `nohup fussycat ... &` asks for a hangup to be ignored. fussycat can't tell
//! whether it was without `unsafe`, so it leaves SIGHUP alone: under nohup
//! it's ignored, as asked; otherwise a closed terminal ends fussycat as it
//! ends any program.

use std::convert::Infallible;
use std::io;
use std::thread;

use tokio::runtime::Builder;
use tokio::signal::unix::{SignalKind, signal};

use super::process::stop_all;

/// A signal that stops a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Interrupt,
    Terminate,
}

impl Signal {
    fn kind(self) -> SignalKind {
        match self {
            Self::Interrupt => SignalKind::interrupt(),
            Self::Terminate => SignalKind::terminate(),
        }
    }

    /// 2 and 15.
    #[must_use]
    pub fn number(self) -> i32 {
        self.kind().as_raw_value()
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Interrupt => "SIGINT",
            Self::Terminate => "SIGTERM",
        }
    }
}

/// From now on, at the first stop signal: kill every running command's
/// group, start no more, and hand the signal to `stop`, which ends fussycat
/// (it can't return: there's no [`Infallible`] to return). The signals are
/// watched on a thread of their own, with its own runtime, so they're
/// heard whatever the run's runtime is doing.
pub fn on_stop_signal(stop: impl FnOnce(Signal) -> Infallible + Send + 'static) -> io::Result<()> {
    let watcher = Builder::new_current_thread().enable_io().build()?;
    // Registered before this returns, so no command starts unwatched.
    let (mut interrupt, mut terminate) = {
        let _inside = watcher.enter();
        (
            signal(Signal::Interrupt.kind())?,
            signal(Signal::Terminate.kind())?,
        )
    };
    thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            let signal = watcher.block_on(async {
                tokio::select! {
                    _ = interrupt.recv() => Signal::Interrupt,
                    _ = terminate.recv() => Signal::Terminate,
                }
            });
            let _stopped = stop_all();
            match stop(signal) {}
        })?;
    Ok(())
}
