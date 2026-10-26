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
