//! High-level command helpers built on top of [`Bridge`].
//!
//! [`Bridge`] speaks raw `{command, args} ↔ {success, data}` frames.
//! These helpers wrap the four wire commands helmor-taper actually
//! uses (`execute_js`, `invoke_tauri`, `capture_native_screenshot`,
//! and the IPC-monitor pair) plus a small fire-and-poll dance that
//! lets a sync `execute_js` drive an async Tauri command and wait for
//! its settlement.
//!
//! Kept here (not in [`Tape`]) because the probe scripts and a future
//! `cargo run --bin taper-probe` will need the same primitives.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::{sleep, Instant};

use crate::bridge::Bridge;

/// Default Tauri window label used by helmor-taper. The Helmor app's
/// main window opens with this label; scenarios never address other
/// windows.
pub const DEFAULT_WINDOW: &str = "main";

/// Outcome of a [`poll_result`] call. Mirrors the `window.__taper[slot]`
/// shape the JS shim writes.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PollResult {
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub error: Option<String>,
}

/// Evaluate `script` synchronously inside the webview and return the
/// `return`ed value. The script MUST be sync — the underlying bridge
/// substring-scans for `Promise.` / `new Promise(` and falls back to a
/// slower path that times out at the scenario level.
pub async fn execute_js(bridge: &Bridge, script: &str) -> Result<Value> {
    bridge
        .request(
            "execute_js",
            json!({"windowLabel": DEFAULT_WINDOW, "script": script}),
        )
        .await
        .context("execute_js failed")
}

/// Fire a backend Tauri command via the webview's `window.__TAURI_INTERNALS__.invoke`
/// and stash the eventual promise resolution on `window.__taper[slot]`.
/// Doesn't await the command itself — the surrounding script stays sync
/// so the bridge's async-detection doesn't downgrade it. Pair with
/// [`poll_result`] or use [`invoke_and_wait`] as a convenience.
pub async fn invoke_command(
    bridge: &Bridge,
    cmd: &str,
    args: Value,
    slot: &str,
) -> Result<()> {
    let script = format!(
        r#"
        window.__taper = window.__taper || {{}};
        var s = (window.__taper[{slot_json}] = {{ done:false, ok:false, value:null, error:null }});
        var invoke = (window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke)
            || (window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke);
        if (!invoke) {{ s.done = true; s.error = "no Tauri invoke on window"; return "no-invoke"; }}
        var p = invoke({cmd_json}, {args_json});
        p["then"](function(v){{ s.ok = true; s.value = v; s.done = true; }},
                  function(e){{ s.error = String((e && e.message) ? e.message : e); s.done = true; }});
        return "started";"#,
        slot_json = serde_json::to_string(slot)?,
        cmd_json = serde_json::to_string(cmd)?,
        args_json = serde_json::to_string(&args)?,
    );
    execute_js(bridge, &script).await?;
    Ok(())
}

/// Read the stashed [`PollResult`] for a slot. Returns the "not started"
/// shape if `window.__taper[slot]` is absent.
pub async fn poll_result(bridge: &Bridge, slot: &str) -> Result<PollResult> {
    let script = format!(
        r#"
        var s = (window.__taper && window.__taper[{slot_json}]) || {{ done:false, ok:false, value:null, error:null }};
        return {{ done: !!s.done, ok: !!s.ok, value: s.value, error: s.error }};"#,
        slot_json = serde_json::to_string(slot)?,
    );
    let raw = execute_js(bridge, &script).await?;
    let parsed: PollResult = serde_json::from_value(raw).context("poll_result: malformed JSON")?;
    Ok(parsed)
}

/// Fire `cmd` and poll until it settles (or timeout). Returns the
/// resolved value, or an error containing the command's rejection
/// message.
pub async fn invoke_and_wait(
    bridge: &Bridge,
    cmd: &str,
    args: Value,
    timeout: Duration,
    slot: &str,
) -> Result<Value> {
    invoke_command(bridge, cmd, args, slot).await?;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let r = poll_result(bridge, slot).await?;
        if r.done {
            if r.ok {
                return Ok(r.value);
            }
            return Err(anyhow!(
                "{cmd} rejected: {}",
                r.error.unwrap_or_else(|| "<no error message>".into())
            ));
        }
        sleep(Duration::from_millis(400)).await;
    }
    Err(anyhow!(
        "{cmd} did not settle within {}ms",
        timeout.as_millis()
    ))
}

/// Capture a native screenshot of the main webview (NOT the full
/// screen — does not commandeer your monitor). Writes PNG bytes to
/// `out_path`. The bridge returns a base64-encoded `data:` URL; we
/// strip the prefix and decode the body.
pub async fn capture_screenshot(bridge: &Bridge, out_path: &Path) -> Result<()> {
    let response = bridge
        .request(
            "capture_native_screenshot",
            json!({
                "windowLabel": DEFAULT_WINDOW,
                "format": "png",
                "quality": 90,
            }),
        )
        .await
        .context("capture_native_screenshot failed")?;

    let data_url = response
        .get("dataUrl")
        .and_then(Value::as_str)
        .or_else(|| response.get("data").and_then(Value::as_str))
        .ok_or_else(|| {
            anyhow!(
                "screenshot response missing dataUrl: {}",
                serde_json::to_string(&response).unwrap_or_default()
            )
        })?;

    let b64 = data_url.split_once(',').map(|p| p.1).unwrap_or(data_url);
    let bytes = decode_base64(b64)?;

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create screenshot dir {}", parent.display())
        })?;
    }
    std::fs::write(out_path, &bytes)
        .with_context(|| format!("failed to write screenshot {}", out_path.display()))?;
    Ok(())
}

/// Tiny base64 decoder. Avoids pulling in a base64 crate for the one
/// call site — and it's about 30 lines that's been tested for decades.
fn decode_base64(input: &str) -> Result<Vec<u8>> {
    // Strip whitespace + standard `=` padding count.
    let cleaned: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    let trimmed = cleaned.trim_end_matches('=');
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);

    let lookup = |c: char| -> Result<u32> {
        Ok(match c {
            'A'..='Z' => (c as u32) - ('A' as u32),
            'a'..='z' => (c as u32) - ('a' as u32) + 26,
            '0'..='9' => (c as u32) - ('0' as u32) + 52,
            '+' => 62,
            '/' => 63,
            _ => return Err(anyhow!("invalid base64 char: {c:?}")),
        })
    };

    let mut buf = 0u32;
    let mut nbits = 0u32;
    for c in trimmed.chars() {
        buf = (buf << 6) | lookup(c)?;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push(((buf >> nbits) & 0xFF) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_result_deserializes_minimal_shape() {
        let v = json!({"done": true, "ok": true, "value": "hi", "error": null});
        let p: PollResult = serde_json::from_value(v).unwrap();
        assert!(p.done);
        assert!(p.ok);
        assert_eq!(p.value, json!("hi"));
        assert!(p.error.is_none());
    }

    #[test]
    fn poll_result_deserializes_pending_shape() {
        let v = json!({"done": false, "ok": false, "value": null, "error": null});
        let p: PollResult = serde_json::from_value(v).unwrap();
        assert!(!p.done);
    }

    #[test]
    fn poll_result_deserializes_error_shape() {
        let v = json!({"done": true, "ok": false, "value": null, "error": "boom"});
        let p: PollResult = serde_json::from_value(v).unwrap();
        assert!(p.done);
        assert!(!p.ok);
        assert_eq!(p.error.as_deref(), Some("boom"));
    }

    #[test]
    fn decode_base64_round_trips_ascii() {
        // `aGVsbG8=` is "hello"
        let bytes = decode_base64("aGVsbG8=").unwrap();
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn decode_base64_handles_no_padding() {
        // `aGVsbG8` (no padding) should still decode to "hello"
        let bytes = decode_base64("aGVsbG8").unwrap();
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn decode_base64_round_trips_png_signature_bytes() {
        // PNG magic header: 0x89 0x50 0x4E 0x47 → "iVBORw==" in base64.
        let bytes = decode_base64("iVBORw==").unwrap();
        assert_eq!(bytes, vec![0x89, 0x50, 0x4E, 0x47]);
    }

    #[test]
    fn decode_base64_rejects_invalid_char() {
        let err = decode_base64("aGV*sbG8").expect_err("`*` is not a valid b64 char");
        assert!(err.to_string().contains("invalid base64 char"));
    }
}
