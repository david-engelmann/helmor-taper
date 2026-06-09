//! End-to-end exercise of the [`Tape`] API against an in-process mock
//! bridge. Covers the surface a real scenario will use: build, drive
//! UI via js/click/wait_for, record assertions, run scene markers, and
//! emit a parseable `result.json`.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use helmor_taper::{
    BridgeConfig, NullRecorder, PostProcessing, ResultSummary, SceneSpec, ScreenCaptureKitRecorder,
    TapeBuilder,
};
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

/// Build a directory of swell shims that mimic the four swift tools
/// well enough to wire Tape's end-to-end recording + post-processing
/// without invoking ScreenCaptureKit. Returns (record_shim, post_shim).
///
/// The recorder shim writes `FAKE_MOV` to its out-path argument; the
/// post-processing shim copies its input to its output so the
/// .mov → .mp4 → .gif chain produces files at the expected paths.
fn make_recording_shims(dir: &std::path::Path) -> (PathBuf, PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let record = dir.join("record-shim.sh");
    std::fs::write(
        &record,
        r#"#!/usr/bin/env bash
# args: $1=<script-arg> $2=<owner> $3=<duration> $4=<out.mov>
printf 'FAKE_MOV' > "$4"
exit 0
"#,
    )
    .unwrap();
    let mut p = std::fs::metadata(&record).unwrap().permissions();
    p.set_mode(0o755);
    std::fs::set_permissions(&record, p).unwrap();

    let mov_to_mp4 = dir.join("mov2mp4.sh");
    std::fs::write(
        &mov_to_mp4,
        r#"#!/usr/bin/env bash
# args: $1=<script-arg> $2=<input.mov> $3=<output.mp4>
cp "$2" "$3"
exit 0
"#,
    )
    .unwrap();
    let mut p = std::fs::metadata(&mov_to_mp4).unwrap().permissions();
    p.set_mode(0o755);
    std::fs::set_permissions(&mov_to_mp4, p).unwrap();

    let mp4_to_gif = dir.join("mp42gif.sh");
    std::fs::write(
        &mp4_to_gif,
        r#"#!/usr/bin/env bash
# args: $1=<script-arg> $2=<input.mp4> $3=<output.gif> $4=<fps> $5=<maxWidth>
# Sanity: confirm fps + maxWidth threaded through.
[ -z "$4" ] && { echo "missing fps" >&2; exit 11; }
[ -z "$5" ] && { echo "missing maxWidth" >&2; exit 12; }
printf 'FAKE_GIF_%s_%s' "$4" "$5" > "$3"
exit 0
"#,
    )
    .unwrap();
    let mut p = std::fs::metadata(&mp4_to_gif).unwrap().permissions();
    p.set_mode(0o755);
    std::fs::set_permissions(&mp4_to_gif, p).unwrap();

    (record, mov_to_mp4, mp4_to_gif)
}

#[tokio::test]
async fn tape_full_continuous_mode_pipeline_with_post_processing() {
    let dir = tempdir().unwrap();
    let shim_dir = dir.path().join("shims");
    std::fs::create_dir(&shim_dir).unwrap();
    let (record_shim, mov2mp4_shim, mp42gif_shim) = make_recording_shims(&shim_dir);
    let tape_dir = dir.path().join("tape-out");

    let port = spawn_mock_bridge().await;
    let url = format!("ws://127.0.0.1:{port}");
    let bridge = helmor_taper::Bridge::connect_direct(&url, BridgeConfig::default())
        .await
        .unwrap();

    let recorder = Box::new(
        ScreenCaptureKitRecorder::new(PathBuf::from("ignored-script-arg"))
            .with_swift_bin(record_shim),
    );
    // PostProcessing models a single swift_bin path used by both
    // tool invocations. For the happy-path test we build a dual-
    // purpose shim that sniffs argv: 4 args → mov→mp4 passthrough
    // copy; 5 args → mp4→gif stamped output that lets the test
    // assert fps + maxWidth were threaded through correctly.
    let _ = (mov2mp4_shim, mp42gif_shim);
    let dual_shim = shim_dir.join("dual-purpose.sh");
    std::fs::write(
        &dual_shim,
        r#"#!/usr/bin/env bash
# args: $1=<script-arg> $2=<input> $3=<output> [$4=<fps> $5=<maxWidth>]
# Dual-purpose: if a 4th arg is present, behave as mp4→gif. Otherwise mov→mp4.
if [ -n "$4" ]; then
  # mp4 -> gif: write a stamped gif so the test can assert fps + maxWidth.
  printf 'FAKE_GIF_%s_%s' "$4" "$5" > "$3"
else
  # mov -> mp4: passthrough copy.
  cp "$2" "$3"
fi
exit 0
"#,
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&dual_shim).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(&dual_shim, p).unwrap();
    }
    let post = PostProcessing {
        swift_bin: dual_shim,
        mov_to_mp4_script: PathBuf::from("mov2mp4-script-arg"),
        mp4_to_gif_script: PathBuf::from("mp42gif-script-arg"),
    };

    let mut tape = TapeBuilder::new("e2e-pipeline", &tape_dir)
        .recorder(recorder)
        .post_processing(post)
        .build_disconnected(bridge);

    tape.start_recording(2, 5, 720).await.unwrap();
    tape.scene(SceneSpec::new("first beat").hold_sec(0)).await.unwrap();
    tape.scene(SceneSpec::new("second beat").hold_sec(0)).await.unwrap();
    let passed = tape.finish(json!({"shim_pipeline": true})).await.unwrap();
    assert!(passed);

    // Recorded .mov is what the recorder shim wrote.
    let mov = tape_dir.join("master.mov");
    assert_eq!(std::fs::read(&mov).unwrap(), b"FAKE_MOV");
    // Post-processing produced both downstream artifacts.
    let mp4 = tape_dir.join("master.mp4");
    let gif = tape_dir.join("master.gif");
    assert_eq!(std::fs::read(&mp4).unwrap(), b"FAKE_MOV", "mov2mp4 copies bytes verbatim");
    let gif_bytes = std::fs::read(&gif).unwrap();
    let gif_str = String::from_utf8(gif_bytes).unwrap();
    assert!(gif_str.contains("FAKE_GIF_5_720"), "gif shim received fps/maxWidth: {gif_str}");

    // result.json carries the beats + the flattened scenario extra.
    let summary: ResultSummary =
        serde_json::from_str(&std::fs::read_to_string(tape_dir.join("result.json")).unwrap())
            .unwrap();
    assert_eq!(summary.beats.len(), 2);
    let raw: Value =
        serde_json::from_str(&std::fs::read_to_string(tape_dir.join("result.json")).unwrap())
            .unwrap();
    assert_eq!(raw["shim_pipeline"], true);
}

#[tokio::test]
async fn tape_post_processing_failure_propagates_through_finish() {
    let dir = tempdir().unwrap();
    let shim_dir = dir.path().join("shims");
    std::fs::create_dir(&shim_dir).unwrap();
    let (record_shim, _ok_mov, _ok_gif) = make_recording_shims(&shim_dir);
    // Replace the mov-to-mp4 shim with a failing one.
    use std::os::unix::fs::PermissionsExt;
    let failing = shim_dir.join("mov2mp4-fail.sh");
    std::fs::write(
        &failing,
        r#"#!/usr/bin/env bash
echo "passthrough preset not available on this host" >&2
exit 1
"#,
    )
    .unwrap();
    let mut p = std::fs::metadata(&failing).unwrap().permissions();
    p.set_mode(0o755);
    std::fs::set_permissions(&failing, p).unwrap();

    let port = spawn_mock_bridge().await;
    let url = format!("ws://127.0.0.1:{port}");
    let bridge = helmor_taper::Bridge::connect_direct(&url, BridgeConfig::default())
        .await
        .unwrap();

    let recorder = Box::new(
        ScreenCaptureKitRecorder::new("ignored").with_swift_bin(record_shim),
    );
    let post = PostProcessing {
        swift_bin: failing,
        mov_to_mp4_script: PathBuf::from("ignored-script-arg"),
        mp4_to_gif_script: PathBuf::from("unused"),
    };
    let mut tape = TapeBuilder::new("e2e-pipeline-fail", dir.path().join("out"))
        .recorder(recorder)
        .post_processing(post)
        .build_disconnected(bridge);

    tape.start_recording(2, 5, 720).await.unwrap();
    let err = tape.finish(json!({})).await.expect_err("post-processing must propagate");
    assert!(
        err.to_string().contains("mov-to-mp4")
            && err.to_string().contains("passthrough preset not available"),
        "error chain should surface the failing tool name + stderr: {err:#}"
    );
}
