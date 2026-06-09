//! End-to-end exercise of the [`Tape`] API against an in-process mock
//! bridge. Covers the surface a real scenario will use: build, drive
//! UI via js/click/wait_for, record assertions, run scene markers, and
//! emit a parseable `result.json`.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use helmor_taper::{BridgeConfig, NullRecorder, ResultSummary, SceneSpec, TapeBuilder};
use serde_json::{json, Value};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// Bridge response shape — mirrors `helmor_taper::BridgeResponse` but
/// lives here so we can hand-construct frames without a public-API
/// dependency on the internal type.
fn ok_frame(id: &str, data: Value) -> String {
    serde_json::to_string(&json!({"id": id, "success": true, "data": data})).unwrap()
}

/// Spawn a tiny mock bridge that knows how to respond to the JS-eval
/// patterns the `Tape` API actually uses. Returns the bound port.
async fn spawn_mock_bridge() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let (mut write, mut read) = ws.split();
                while let Some(Ok(msg)) = read.next().await {
                    let Message::Text(text) = msg else { continue };
                    let req: Value = serde_json::from_str(&text).unwrap();
                    let id = req["id"].as_str().unwrap_or_default().to_string();
                    let command = req["command"].as_str().unwrap_or_default();

                    // Inspect the script for common Tape primitives.
                    let response = match command {
                        "execute_js" => {
                            let script = req["args"]["script"].as_str().unwrap_or_default();
                            if script.contains("document.querySelector(") && script.contains(".click(); return true;") {
                                // `click` helper: report a successful hit.
                                ok_frame(&id, json!(true))
                            } else if script.contains("return !!document.querySelector(") {
                                // `wait_for` predicate: present after the second poll.
                                ok_frame(&id, json!(true))
                            } else if script.contains("helmor:open-settings") {
                                ok_frame(&id, json!("ok"))
                            } else if script.contains("KeyboardEvent") {
                                ok_frame(&id, json!("esc"))
                            } else if script.contains("return 1+1") || script.contains("1+1;") {
                                ok_frame(&id, json!(2))
                            } else {
                                ok_frame(&id, json!(null))
                            }
                        }
                        _ => ok_frame(&id, json!(null)),
                    };
                    let _ = write.send(Message::Text(response)).await;
                }
            });
        }
    });
    port
}

async fn make_tape(
    out_dir: PathBuf,
) -> helmor_taper::Tape {
    let port = spawn_mock_bridge().await;
    let url = format!("ws://127.0.0.1:{port}");
    // Build a Bridge via the public connect_direct entry, then attach
    // it to a TapeBuilder via build_disconnected. The "disconnected"
    // naming is misleading here — the Bridge IS connected, it just
    // doesn't go through the port-scanner.
    let bridge = helmor_taper::Bridge::connect_direct(&url, BridgeConfig::default())
        .await
        .unwrap();
    TapeBuilder::new("integration-test", out_dir)
        .recorder(Box::new(NullRecorder::default()))
        .build_disconnected(bridge)
}

#[tokio::test]
async fn tape_records_assertions_and_writes_result_json() {
    let dir = tempdir().unwrap();
    let mut tape = make_tape(dir.path().to_path_buf()).await;

    tape.assert("ssh_reachable", true, "connected in 200ms");
    tape.assert("daemon_binary", false, "missing");
    tape.assert("daemon_log_clean", true, "");

    let passed = tape.finish(json!({})).await.unwrap();
    assert!(!passed, "one failing assertion should fail the whole tape");

    let body = std::fs::read_to_string(dir.path().join("result.json")).unwrap();
    let parsed: ResultSummary = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed.scenario, "integration-test");
    assert_eq!(parsed.assertions.len(), 3);
    assert!(parsed.assertions[1].name == "daemon_binary" && !parsed.assertions[1].ok);
    assert!(parsed.beats.is_empty(), "no scene calls → no beats");
}

#[tokio::test]
async fn tape_continuous_mode_emits_beats_and_extras() {
    let dir = tempdir().unwrap();
    let mut tape = make_tape(dir.path().to_path_buf()).await;

    // start_recording with a tiny duration so the NullRecorder's
    // "started" check passes without waiting.
    tape.start_recording(10, 5, 720).await.unwrap();

    // Two beats — both should be persisted with their elapsed t.
    tape.scene(SceneSpec::new("first beat").hold_sec(0)).await.unwrap();
    tape.scene(SceneSpec::new("second beat").hold_sec(0)).await.unwrap();

    let passed = tape
        .finish(json!({"containerHostname": "081e3cab7eb5"}))
        .await
        .unwrap();
    assert!(passed, "no assertions → trivially pass");

    let body = std::fs::read_to_string(dir.path().join("result.json")).unwrap();
    let parsed: ResultSummary = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed.beats.len(), 2);
    assert_eq!(parsed.beats[0].caption, "first beat");
    assert_eq!(parsed.beats[1].caption, "second beat");
    assert!(parsed.beats[0].t < parsed.beats[1].t, "beat timestamps monotonic");
    // Extras flattened.
    let raw: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(raw["containerHostname"], "081e3cab7eb5");
    assert!(raw.get("extras").is_none(), "extras must flatten, not nest");
}

#[tokio::test]
async fn tape_click_helper_drives_js_via_bridge() {
    let dir = tempdir().unwrap();
    let tape = make_tape(dir.path().to_path_buf()).await;

    let hit = tape
        .click("[data-testid=\"remote-server-reconnect-docker-arm64\"]")
        .await
        .unwrap();
    assert!(hit, "mock bridge should report click hit");
}

#[tokio::test]
async fn tape_wait_for_returns_true_when_selector_present() {
    let dir = tempdir().unwrap();
    let tape = make_tape(dir.path().to_path_buf()).await;

    let appeared = tape
        .wait_for("[data-testid=\"workspace-runtime-chip\"]", Duration::from_secs(1))
        .await
        .unwrap();
    assert!(appeared);
}

#[tokio::test]
async fn tape_open_settings_dispatches_custom_event() {
    let dir = tempdir().unwrap();
    let tape = make_tape(dir.path().to_path_buf()).await;
    // The mock bridge returns "ok" for any script containing
    // "helmor:open-settings". The point of this test is that the
    // Tape helper doesn't fail mid-dispatch.
    tape.open_settings("appearance").await.unwrap();
}

#[tokio::test]
async fn tape_scene_without_start_recording_errors() {
    let dir = tempdir().unwrap();
    let mut tape = make_tape(dir.path().to_path_buf()).await;
    let err = tape
        .scene(SceneSpec::new("orphan beat").hold_sec(0))
        .await
        .expect_err("scene without start_recording must error");
    assert!(
        err.to_string().contains("not yet implemented"),
        "got: {err}"
    );
}

#[tokio::test]
async fn tape_double_start_recording_errors() {
    let dir = tempdir().unwrap();
    let mut tape = make_tape(dir.path().to_path_buf()).await;
    tape.start_recording(10, 5, 720).await.unwrap();
    let err = tape
        .start_recording(10, 5, 720)
        .await
        .expect_err("second start_recording must error");
    assert!(err.to_string().contains("already started"), "got: {err}");
}

#[tokio::test]
async fn tape_finish_writes_out_dir_when_missing() {
    let dir = tempdir().unwrap();
    let nested = dir.path().join("nested-tape");
    // intentionally don't create `nested`; finish must mkdir -p.
    let mut tape = make_tape(nested.clone()).await;
    tape.assert("ok", true, "");
    tape.finish(json!({})).await.unwrap();
    assert!(nested.join("result.json").exists());
}
