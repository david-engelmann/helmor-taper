//! Headline transport-foundation scenario: SSH into a Linux Docker
//! container, prove `helmor-server` installs + launches over the
//! wire, and the Remote Servers panel ends green.
//!
//! Rust port of `scenarios/connect-over-ssh.ts`. The shape matches the
//! TS implementation step-for-step so reviewing one against the other
//! is mechanical:
//!
//! 1. Disconnect any existing runtime, close any open dialog (clean slate).
//! 2. Open Settings → Remote Servers, assert it starts empty.
//! 3. Fire `connect_remote_runtime` WITHOUT awaiting + capture the
//!    "connecting…" beat in parallel.
//! 4. Poll the slot until settled. Capture daemon health.
//! 5. Reopen the panel + confirm the connected row says "Connected".
//!
//! Headline assertions (each appears in `result.json`):
//! - `panel_opens`, `starts_empty` — clean-slate sanity.
//! - `ssh_connect_succeeds` — the connect promise resolves Ok.
//! - `daemon_reports_remote` — health.kind.host matches the target host.
//! - `daemon_reports_version` — health.version is semver-shaped.
//! - `ui_shows_connected_row`, `row_says_connected` — Remote Servers
//!   panel reflects the live state.

use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tape::{SceneSpec, Tape};

#[derive(Debug, Clone)]
pub struct Config {
    /// SSH host alias the Helmor desktop should connect to. Resolved
    /// via the desktop's `~/.ssh/config` lookup.
    pub host: String,
    /// Display + identifier for the remote runtime; matches what the
    /// Remote Servers panel keys off.
    pub runtime_name: String,
    /// Absolute path to the helmor-server binary on the remote.
    pub remote_binary: String,
}

impl Config {
    /// Read the standard env vars used by the TS port. Defaults
    /// match the `helmor-taper-arm64` Docker fixture so a bare
    /// `taper scenario connect-over-ssh` works against a checked-out
    /// repo with no env mucking.
    pub fn from_env() -> Self {
        Self {
            host: std::env::var("HOST_ALIAS").unwrap_or_else(|_| "helmor-taper-arm64".into()),
            runtime_name: std::env::var("RUNTIME_NAME")
                .unwrap_or_else(|_| "docker-linux-arm64".into()),
            remote_binary: std::env::var("REMOTE_BINARY")
                .unwrap_or_else(|_| "/home/e2e/.helmor/server/helmor-server".into()),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DaemonHealth {
    pub hostname: Option<String>,
    pub version: Option<String>,
    pub kind: Option<DaemonKind>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DaemonKind {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub host: Option<String>,
}

/// Run the scenario. Returns `tape.finish()`'s pass/fail boolean.
pub async fn run(tape: &mut Tape, config: &Config) -> Result<bool> {
    let row_selector = format!(
        "[data-testid=remote-server-row-{}]",
        config.runtime_name
    );

    // Clean slate: disconnect + close any dialog.
    let _ = tape
        .invoke::<Value>(
            "disconnect_remote_runtime",
            json!({"name": config.runtime_name}),
        )
        .await;
    tape.close_dialog().await?;
    tape.sleep(Duration::from_millis(500)).await;

    // Scene 1 — the empty panel.
    tape.open_settings("remote-servers").await?;
    tape.sleep(Duration::from_millis(900)).await;
    let panel_open = tape
        .wait_for("[role=dialog]", Duration::from_secs(5))
        .await?;
    tape.assert("panel_opens", panel_open, "");
    let starts_empty = tape
        .wait_for("[data-testid=remote-servers-empty]", Duration::from_secs(3))
        .await?;
    tape.assert("starts_empty", starts_empty, "");
    tape.scene(
        SceneSpec::new("Settings → Remote Servers: no remote hosts yet").hold_sec(4),
    )
    .await?;

    // Scene 2 — fire the SSH connect and capture the connecting beat.
    let start = std::time::Instant::now();
    tape.invoke_async(
        "connect_remote_runtime",
        json!({
            "name": config.runtime_name,
            "host": config.host,
            "remoteBinary": config.remote_binary,
            "forwardAgent": false,
        }),
        "connect",
    )
    .await?;
    tape.scene(
        SceneSpec::new(format!(
            "Connecting to {} over SSH — Helmor installs + launches helmor-server",
            config.host
        ))
        .record_sec(3)
        .hold_sec(4),
    )
    .await?;

    // Wait for the connect to settle, capture health.
    let outcome = tape
        .poll_until_done(
            "connect",
            Duration::from_secs(60),
            Duration::from_millis(400),
        )
        .await?;
    let elapsed_ms = start.elapsed().as_millis();
    tape.assert(
        "ssh_connect_succeeds",
        outcome.done && outcome.ok,
        if outcome.ok {
            format!("{elapsed_ms}ms")
        } else {
            outcome.error.clone().unwrap_or_default()
        },
    );
    let health: DaemonHealth = serde_json::from_value(outcome.value.clone()).unwrap_or_default();

    let kind_host_matches = health
        .kind
        .as_ref()
        .and_then(|k| k.host.as_deref())
        .is_some_and(|h| h == config.host);
    let kind_is_remote = health
        .kind
        .as_ref()
        .and_then(|k| k.kind.as_deref())
        .is_some_and(|k| k == "remote");
    tape.assert(
        "daemon_reports_remote",
        kind_host_matches && kind_is_remote,
        serde_json::to_string(&health.kind).unwrap_or_default(),
    );
    let version = health.version.clone().unwrap_or_default();
    let semver_re = regex_like_semver_check(&version);
    tape.assert(
        "daemon_reports_version",
        semver_re,
        format!("v{version}"),
    );

    // Scene 3 — reopen the panel showing the connected row.
    tape.close_dialog().await?;
    tape.sleep(Duration::from_millis(300)).await;
    tape.open_settings("remote-servers").await?;
    let row_visible = tape
        .wait_for(&row_selector, Duration::from_secs(8))
        .await?;
    tape.assert("ui_shows_connected_row", row_visible, "");
    let row_script = format!(
        r#"var r=document.querySelector({sel}); return r?r.innerText.replace(/\n+/g," · "):null;"#,
        sel = serde_json::to_string(&row_selector)?,
    );
    let row_text: Option<String> = tape.js(&row_script).await?;
    let says_connected = row_text
        .as_deref()
        .is_some_and(|t| t.to_lowercase().contains("connected"));
    tape.assert(
        "row_says_connected",
        says_connected,
        row_text.clone().unwrap_or_default(),
    );
    tape.scene(
        SceneSpec::new(format!(
            "Connected — helmor-server {} live on {}",
            health.version.clone().unwrap_or_default(),
            health.hostname.clone().unwrap_or_default()
        ))
        .hold_sec(5),
    )
    .await?;

    tape.finish(json!({
        "host": config.host,
        "runtimeName": config.runtime_name,
        "health": serde_json::to_value(&health).ok(),
    }))
    .await
}

/// Public wrapper around the (private) semver-shape check so sibling
/// scenarios (e.g. [`crate::scenarios::remote_runner`]) can reuse it
/// without duplicating the predicate.
pub fn regex_like_semver_check_pub(v: &str) -> bool {
    regex_like_semver_check(v)
}

/// Tiny version-shape check. Avoids pulling in `regex` for one site —
/// the TS port used `/^\d+\.\d+\.\d+/`.
fn regex_like_semver_check(v: &str) -> bool {
    let mut parts = v.splitn(3, '.');
    let major = parts.next().unwrap_or("");
    let minor = parts.next().unwrap_or("");
    let patch_rest = parts.next().unwrap_or("");
    if major.is_empty() || minor.is_empty() || patch_rest.is_empty() {
        return false;
    }
    if !major.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if !minor.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // Patch part may have a -suffix; require leading digit(s).
    let patch_leading = patch_rest
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();
    !patch_leading.is_empty()
}

impl serde::Serialize for DaemonHealth {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("DaemonHealth", 3)?;
        st.serialize_field("hostname", &self.hostname)?;
        st.serialize_field("version", &self.version)?;
        st.serialize_field("kind", &self.kind)?;
        st.end()
    }
}

impl serde::Serialize for DaemonKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("DaemonKind", 2)?;
        st.serialize_field("type", &self.kind)?;
        st.serialize_field("host", &self.host)?;
        st.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_check_accepts_clean_three_part() {
        assert!(regex_like_semver_check("0.26.0"));
        assert!(regex_like_semver_check("1.2.3"));
        assert!(regex_like_semver_check("12.345.6789"));
    }

    #[test]
    fn semver_check_accepts_trailing_pre_release() {
        assert!(regex_like_semver_check("1.2.3-rc1"));
        assert!(regex_like_semver_check("0.26.0-alpha.1"));
    }

    #[test]
    fn semver_check_rejects_two_part() {
        assert!(!regex_like_semver_check("1.2"));
        assert!(!regex_like_semver_check("1."));
        assert!(!regex_like_semver_check("1"));
        assert!(!regex_like_semver_check(""));
    }

    #[test]
    fn semver_check_rejects_non_numeric_components() {
        assert!(!regex_like_semver_check("a.b.c"));
        assert!(!regex_like_semver_check("v1.2.3"));
        assert!(!regex_like_semver_check("1.x.3"));
    }

    #[test]
    fn config_from_env_uses_defaults_when_unset() {
        // Make sure the test environment doesn't inherit a stray
        // HOST_ALIAS etc. from the shell. unsafe here is fine; we
        // only mutate env in a single-threaded test.
        unsafe {
            std::env::remove_var("HOST_ALIAS");
            std::env::remove_var("RUNTIME_NAME");
            std::env::remove_var("REMOTE_BINARY");
        }
        let c = Config::from_env();
        assert_eq!(c.host, "helmor-taper-arm64");
        assert_eq!(c.runtime_name, "docker-linux-arm64");
        assert_eq!(c.remote_binary, "/home/e2e/.helmor/server/helmor-server");
    }

    #[test]
    fn daemon_health_deserializes_partial_payload() {
        // Real daemons sometimes omit fields; the scenario should
        // accept that and report the missing piece in its assertion.
        let v = json!({"version": "0.26.0"});
        let h: DaemonHealth = serde_json::from_value(v).unwrap();
        assert_eq!(h.version.as_deref(), Some("0.26.0"));
        assert!(h.hostname.is_none());
        assert!(h.kind.is_none());
    }

    #[test]
    fn daemon_health_round_trips_with_kind() {
        let v = json!({
            "hostname": "081e3cab7eb5",
            "version": "0.26.0",
            "kind": { "type": "remote", "host": "helmor-taper-arm64" },
        });
        let h: DaemonHealth = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(h.hostname.as_deref(), Some("081e3cab7eb5"));
        assert_eq!(
            h.kind.as_ref().and_then(|k| k.kind.as_deref()),
            Some("remote")
        );
        let serialized = serde_json::to_value(&h).unwrap();
        // Field name remap survives the round trip.
        assert_eq!(serialized["kind"]["type"], "remote");
    }
}
