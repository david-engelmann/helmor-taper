//! Headless probes that exercise individual remote-runner features
//! end-to-end against a live Helmor desktop. Unlike scenarios, probes
//! don't record video — they just hit the wire, assert the right
//! events came back, and exit 0/1.
//!
//! Each probe lives in its own module, exports a `Config` struct with
//! `from_env`, and a `run(bridge, config) -> Result<bool>` function.
//! `taper probe <name>` dispatches by name.
//!
//! Probes are the smaller, faster, less narrative cousins of scenarios.
//! Where a scenario answers "does this feature look right on camera?",
//! a probe answers "does this feature actually work right now?" —
//! useful for CI smoke checks, regression hunts, and "is my dev
//! environment still healthy?" quick reads.

pub mod bundle_install;
pub mod daemon_persistence;
pub mod feature_probe;
pub mod remote_agent;
pub mod remote_port_forward;
pub mod remote_terminal;
pub mod remote_watch;

use std::process::Command;

use anyhow::{anyhow, Result};

/// Shared helper: run a subprocess, surface non-zero exit with stderr.
/// Probes shell out to `docker exec`, `sqlite3`, `hostname`, `python3`
/// etc. for ground-truth verification.
pub(crate) fn run_cmd(prog: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(prog).args(args).output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "{prog} {} → exit {}: {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Variant that returns stdout even on non-zero exit. Used by probes
/// whose subprocess is expected to occasionally fail (e.g. fetch
/// returning 4xx) and the diagnostic is the body.
#[allow(dead_code)]
pub(crate) fn run_cmd_lenient(prog: &str, args: &[&str]) -> (String, bool) {
    match Command::new(prog).args(args).output() {
        Ok(out) => (
            String::from_utf8_lossy(&out.stdout).to_string(),
            out.status.success(),
        ),
        Err(_) => (String::new(), false),
    }
}
