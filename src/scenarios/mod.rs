//! Recorded scenarios — each driven by a [`crate::Tape`] against a
//! live Helmor desktop via the MCP bridge. Phase R4 ports two
//! reference scenarios; the rest land in follow-up commits as their
//! TS counterparts are migrated.
//!
//! Crate layout for a scenario:
//!
//! ```text
//! src/scenarios/<name>.rs
//!   pub struct Config { … }
//!   impl Config { pub fn from_env() -> Self { … } }
//!   pub async fn run(tape: &mut Tape, config: &Config) -> Result<bool>
//! ```
//!
//! Each scenario is independently runnable via
//! `taper scenario <name>` (CLI wiring lives in `src/bin/taper.rs`).
//! The `Config` struct knows how to read its inputs from env vars so
//! the CLI doesn't have to teach itself a different flag set per
//! scenario.

pub mod add_remote_wizard;
pub mod connect_over_ssh;
pub mod first_connect_bundle;
pub mod isolation_proof;
pub mod observability;
pub mod remote_file_ops;
pub mod remote_runner;
pub mod remote_workspace;
pub mod resilience;
pub mod row_actions;
