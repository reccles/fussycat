//! The fussycat domain: an [Agent Quilt][spec] agent loop with no I/O.
//!
//! Language and model: `context/domain-design.spec.md`. Everything that
//! touches the world (providers, shells, files, stdout) is an adapter in
//! the `fussycat` crate, behind the ports defined here: [`provider::Provider`],
//! [`quilt::Source`], [`toolbox::Tool`], [`clock::Clock`] and
//! [`report::Reporter`].
//!
//! [spec]: ../../../docs/agent-quilt-framework.md

pub mod agent;
pub mod clock;
pub mod live;
pub mod log;
pub mod provider;
pub mod quilt;
pub mod report;
pub mod run;
pub mod toolbox;

/// The test fakes are always ready, so polling once is enough: no runtime.
#[cfg(test)]
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(output) => output,
        std::task::Poll::Pending => panic!("fakes never wait"),
    }
}
