//! Scenario orchestrator. `Tape` is the type a scenario builds + runs;
//! it owns the bridge connection, tracks assertions, drives the UI via
//! [`crate::commands`], and writes `result.json` on `finish`.
//!
//! Phase R2 lands the scenario API, the recording-mode state machine,
//! and a `NullRecorder` for tests. The Swift ScreenCaptureKit
//! integration lives in [`recorder`] and is currently a stub — Phase
//! R3 fills in the real `ScreenCaptureKitRecorder` that shells out to
//! the `scripts/record-window.swift` shim.

mod assertion;
mod post;
mod recorder;
mod screencapturekit;

pub use assertion::{Assertion, ResultSummary};
pub use post::{convert_mov_to_mp4, convert_mp4_to_gif, PostError};
pub use recorder::{NullRecorder, Recorder, RecorderError};
pub use screencapturekit::{ScreenCaptureKitRecorder, DEFAULT_OWNER};

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::time::{sleep, Instant};

use crate::bridge::{Bridge, BridgeConfig};
use crate::commands;

/// One captured moment in a scenario timeline.
#[derive(Debug, Clone)]
pub struct SceneSpec {
    /// Caption burned across the top of this beat (scene mode) /
    /// marker text logged for the timeline (continuous mode).
    pub caption: String,
    /// Live ScreenCaptureKit capture seconds (catches motion).
    /// Defaults to 2.
    pub record_sec: u64,
    /// Total clip seconds; last frame freezes to fill. Defaults to 4.
    pub hold_sec: u64,
}

impl SceneSpec {
    pub fn new(caption: impl Into<String>) -> Self {
        Self {
            caption: caption.into(),
            record_sec: 2,
            hold_sec: 4,
        }
    }

    pub fn record_sec(mut self, secs: u64) -> Self {
        self.record_sec = secs;
        self
    }

    pub fn hold_sec(mut self, secs: u64) -> Self {
        self.hold_sec = secs;
        self
    }
}

/// One marker in continuous-mode recording — the timestamp from
/// recording start + the human-readable caption. Persisted into
/// `result.json#beats` so a viewer can cross-reference the gif's
/// elapsed time with what was happening on screen.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ContinuousBeat {
    /// Elapsed seconds since `start_recording` was called.
    pub t: f64,
    pub caption: String,
}

#[derive(Debug)]
struct ContinuousState {
    started_at: Instant,
    mov_path: PathBuf,
    // Consumed by the mov→mp4→gif post-processing step in Phase R3.
    // Stored here in R2 so the scenario API doesn't change shape when
    // R3 lands; the dead-code suppression goes away as soon as the
    // post-processing step is hooked in.
    #[allow(dead_code)]
    gif_fps: u32,
    #[allow(dead_code)]
    gif_max_width: u32,
}

/// Continuous-mode post-processing config: where the swift binary
/// lives and which scripts to run. Used by [`Tape::finish`] when a
/// recording was started.
#[derive(Debug, Clone)]
pub struct PostProcessing {
    pub swift_bin: PathBuf,
    pub mov_to_mp4_script: PathBuf,
    pub mp4_to_gif_script: PathBuf,
}

impl PostProcessing {
    /// Default layout: `swift` on `PATH`, scripts in
    /// `<repo>/scripts/`. Use this when running scenarios from the
    /// repo checkout.
    pub fn from_scripts_dir(scripts_dir: impl Into<PathBuf>) -> Self {
        let dir = scripts_dir.into();
        Self {
            swift_bin: PathBuf::from("swift"),
            mov_to_mp4_script: dir.join("mov-to-mp4.swift"),
            mp4_to_gif_script: dir.join("mp4-to-gif.swift"),
        }
    }

    /// Override the swift binary path. Used in tests to substitute a
    /// shell shim.
    pub fn with_swift_bin(mut self, path: impl Into<PathBuf>) -> Self {
        self.swift_bin = path.into();
        self
    }
}

/// Builder for [`Tape`]. Use this when you want to swap in a custom
/// recorder (e.g. `NullRecorder` for tests) or skip post-processing.
pub struct TapeBuilder {
    name: String,
    out_dir: PathBuf,
    bridge_config: BridgeConfig,
    recorder: Box<dyn Recorder>,
    post_processing: Option<PostProcessing>,
}

impl TapeBuilder {
    pub fn new(name: impl Into<String>, out_dir: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            out_dir: out_dir.into(),
            bridge_config: BridgeConfig::default(),
            recorder: Box::new(NullRecorder::default()),
            post_processing: None,
        }
    }

    pub fn bridge_config(mut self, config: BridgeConfig) -> Self {
        self.bridge_config = config;
        self
    }

    pub fn recorder(mut self, recorder: Box<dyn Recorder>) -> Self {
        self.recorder = recorder;
        self
    }

    /// Enable continuous-mode post-processing (.mov → .mp4 → .gif).
    /// Without this, [`Tape::finish`] leaves the .mov in place.
    pub fn post_processing(mut self, pp: PostProcessing) -> Self {
        self.post_processing = Some(pp);
        self
    }

    /// Connect to the bridge and return a ready-to-use `Tape`.
    /// Fails fast if the bridge isn't reachable so the caller can
    /// short-circuit with a clear error.
    pub async fn build(self) -> Result<Tape> {
        let bridge = Bridge::connect(self.bridge_config)
            .await
            .context("Tape: bridge connect failed")?;
        Ok(Tape {
            name: self.name,
            out_dir: self.out_dir,
            bridge,
            assertions: Vec::new(),
            started_at: Instant::now(),
            wall_started_at: chrono_like_now_iso(),
            continuous: None,
            beats: Vec::new(),
            scene_idx: 0,
            recorder: Arc::new(Mutex::new(self.recorder)),
            post_processing: self.post_processing,
        })
    }

    /// Build a Tape without connecting to the bridge — used by tests
    /// that drive the assertion / finish path without an MCP server.
    pub fn build_disconnected(self, mock_bridge: Bridge) -> Tape {
        Tape {
            name: self.name,
            out_dir: self.out_dir,
            bridge: mock_bridge,
            assertions: Vec::new(),
            started_at: Instant::now(),
            wall_started_at: chrono_like_now_iso(),
            continuous: None,
            beats: Vec::new(),
            scene_idx: 0,
            recorder: Arc::new(Mutex::new(self.recorder)),
            post_processing: self.post_processing,
        }
    }
}

/// Active scenario instance. Owns the bridge connection + the
/// in-progress assertion list + (in continuous mode) the recorder
/// handle. Drop semantics are intentional: dropping the Tape closes
/// the bridge connection via Bridge's own Drop.
pub struct Tape {
    pub name: String,
    pub out_dir: PathBuf,
    bridge: Bridge,
    assertions: Vec<Assertion>,
    started_at: Instant,
    wall_started_at: String,
    continuous: Option<ContinuousState>,
    beats: Vec<ContinuousBeat>,
    scene_idx: usize,
    recorder: Arc<Mutex<Box<dyn Recorder>>>,
    post_processing: Option<PostProcessing>,
}

impl Tape {
    pub fn bridge(&self) -> &Bridge {
        &self.bridge
    }

    pub fn assertions(&self) -> &[Assertion] {
        &self.assertions
    }

    pub fn beats(&self) -> &[ContinuousBeat] {
        &self.beats
    }

    pub fn log(&self, msg: &str) {
        let elapsed_s = self.started_at.elapsed().as_secs_f64();
        eprintln!("[{} +{:.1}s] {msg}", self.name, elapsed_s);
    }

    pub fn assert(&mut self, name: impl Into<String>, ok: bool, detail: impl Into<String>) {
        let name = name.into();
        let detail = detail.into();
        let line = if detail.is_empty() {
            format!("{} {name}", if ok { "PASS" } else { "FAIL" })
        } else {
            format!("{} {name} — {detail}", if ok { "PASS" } else { "FAIL" })
        };
        self.log(&line);
        self.assertions.push(Assertion { name, ok, detail });
    }

    /// Evaluate sync JS in the webview and parse the return value as `T`.
    /// Use this for predicates / readouts that don't await Promises.
    pub async fn js<T: serde::de::DeserializeOwned>(&self, script: &str) -> Result<T> {
        let raw = commands::execute_js(&self.bridge, script).await?;
        let parsed = serde_json::from_value(raw).context("Tape::js failed to parse result")?;
        Ok(parsed)
    }

    /// Invoke a backend Tauri command via the fire+poll pattern and
    /// parse the resolved value as `T`. Default timeout 90s.
    pub async fn invoke<T: serde::de::DeserializeOwned>(
        &mut self,
        cmd: &str,
        args: Value,
    ) -> Result<T> {
        let slot = format!("{}-{}", self.name, self.scene_idx);
        let raw =
            commands::invoke_and_wait(&self.bridge, cmd, args, Duration::from_secs(90), &slot)
                .await?;
        let parsed = serde_json::from_value(raw).context("Tape::invoke failed to parse result")?;
        Ok(parsed)
    }

    /// Fire a backend Tauri command WITHOUT waiting for it. The
    /// resolution is stashed on `window.__taper[slot]`; later call
    /// [`poll`] or [`poll_until_done`] to read it. Used by scenarios
    /// that need to capture a "currently in flight" beat (e.g. the
    /// "connecting…" frame while `connect_remote_runtime` is still
    /// running) in parallel with the command.
    pub async fn invoke_async(&self, cmd: &str, args: Value, slot: &str) -> Result<()> {
        commands::invoke_command(&self.bridge, cmd, args, slot).await
    }

    /// Read a previously-fired command's outcome. Returns the
    /// "not yet started" shape if `slot` was never written.
    pub async fn poll(&self, slot: &str) -> Result<commands::PollResult> {
        commands::poll_result(&self.bridge, slot).await
    }

    /// Poll `slot` until [`PollResult::done`] is true or `timeout`
    /// elapses. Returns the final [`PollResult`] (which may carry a
    /// rejection). Tighter than `invoke_and_wait` because the scenario
    /// has already done other work between the `invoke_async` and the
    /// poll.
    pub async fn poll_until_done(
        &self,
        slot: &str,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<commands::PollResult> {
        let deadline = Instant::now() + timeout;
        loop {
            let r = self.poll(slot).await?;
            if r.done {
                return Ok(r);
            }
            if Instant::now() >= deadline {
                return Ok(r);
            }
            sleep(poll_interval).await;
        }
    }

    pub async fn open_settings(&self, section: &str) -> Result<()> {
        let section_json = serde_json::to_string(section)?;
        let script = format!(
            r#"window.dispatchEvent(new CustomEvent("helmor:open-settings",{{detail:{{section:{section_json}}}}})); return "ok";"#,
        );
        commands::execute_js(&self.bridge, &script).await?;
        Ok(())
    }

    pub async fn close_dialog(&self) -> Result<()> {
        commands::execute_js(
            &self.bridge,
            r#"document.dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true})); return "esc";"#,
        )
        .await?;
        Ok(())
    }

    /// Click the first element matching `selector`. Returns whether
    /// an element was found + clicked.
    pub async fn click(&self, selector: &str) -> Result<bool> {
        let script = format!(
            r#"var el=document.querySelector({sel_json}); if(el){{el.click(); return true;}} return false;"#,
            sel_json = serde_json::to_string(selector)?,
        );
        self.js::<bool>(&script).await
    }

    /// Poll until `selector` appears in the DOM (or timeout).
    pub async fn wait_for(&self, selector: &str, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        let script = format!(
            r#"return !!document.querySelector({sel_json});"#,
            sel_json = serde_json::to_string(selector)?,
        );
        while Instant::now() < deadline {
            let hit: bool = self.js(&script).await?;
            if hit {
                return Ok(true);
            }
            sleep(Duration::from_millis(300)).await;
        }
        Ok(false)
    }

    pub async fn sleep(&self, duration: Duration) {
        sleep(duration).await;
    }

    /// Write a value into a React-controlled input. Hits the prototype
    /// setter (so React's onChange listener fires) instead of just
    /// assigning `.value`. Returns the script result token: `"ok"` on
    /// success, `"no-input"` if the selector didn't match.
    pub async fn set_input_value(&self, selector: &str, value: &str) -> Result<String> {
        let script = format!(
            r#"
            var el=document.querySelector({sel});
            if(!el) return "no-input";
            var d=Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype,'value');
            (d && d.set)?d.set.call(el,{val}):(el.value={val});
            el.dispatchEvent(new Event('input',{{bubbles:true}}));
            return "ok";"#,
            sel = serde_json::to_string(selector)?,
            val = serde_json::to_string(value)?,
        );
        self.js(&script).await
    }

    /// Click the first `<button>` whose `innerText.trim()` equals
    /// `text`. Returns true on hit. Used by scenarios that need to
    /// activate a button without a stable `data-testid` (the
    /// `<Button>{text}</Button>` shape).
    pub async fn click_button_by_text(&self, text: &str) -> Result<bool> {
        let script = format!(
            r#"var bs=document.querySelectorAll('button');
               for(var i=0;i<bs.length;i++){{
                 if((bs[i].innerText||'').trim()=={t}){{ bs[i].click(); return true; }}
               }} return false;"#,
            t = serde_json::to_string(text)?,
        );
        self.js(&script).await
    }

    /// Poll until `needle` appears anywhere inside `scope_selector`'s
    /// `innerText`. Returns true on hit, false on timeout. Used by
    /// scenarios that wait for an async UI update to render its
    /// payload (e.g. "Run file tree" populating a list).
    pub async fn wait_for_text(
        &self,
        scope_selector: &str,
        needle: &str,
        timeout: Duration,
    ) -> Result<bool> {
        let script = format!(
            r#"var s=document.querySelector({sel});
               return !!s && (s.innerText||'').indexOf({needle})>=0;"#,
            sel = serde_json::to_string(scope_selector)?,
            needle = serde_json::to_string(needle)?,
        );
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let hit: bool = self.js(&script).await?;
            if hit {
                return Ok(true);
            }
            sleep(Duration::from_millis(300)).await;
        }
        Ok(false)
    }

    /// Scroll the section containing `selector` to the top of its
    /// scrollable parent. Used by tapes that capture different cards
    /// in a long Settings pane — each beat brings its card into view
    /// before the recorder snapshots it.
    pub async fn scroll_to_section(&self, selector: &str) -> Result<bool> {
        let script = format!(
            r#"var el=document.querySelector({sel}); if(!el) return false;
               (el.closest('section')||el).scrollIntoView({{block:'start',behavior:'auto'}});
               return true;"#,
            sel = serde_json::to_string(selector)?,
        );
        self.js(&script).await
    }

    /// Start a single ScreenCaptureKit recording for `duration_sec`.
    /// In continuous mode, subsequent [`Tape::scene`] calls log + sleep
    /// for the hold duration rather than capturing per-scene clips.
    pub async fn start_recording(
        &mut self,
        duration_sec: u64,
        gif_fps: u32,
        gif_max_width: u32,
    ) -> Result<()> {
        if self.continuous.is_some() {
            anyhow::bail!("recording already started");
        }
        std::fs::create_dir_all(&self.out_dir)
            .with_context(|| format!("failed to create out_dir {}", self.out_dir.display()))?;
        let mov_path = self.out_dir.join("master.mov");

        {
            let mut recorder = self.recorder.lock().await;
            recorder.start(&mov_path, Duration::from_secs(duration_sec))?;
        }

        let state = ContinuousState {
            started_at: Instant::now(),
            mov_path: mov_path.clone(),
            gif_fps,
            gif_max_width,
        };
        self.log(&format!(
            "recording {duration_sec}s → {} (fps={gif_fps} max_w={gif_max_width})",
            mov_path.display()
        ));
        self.continuous = Some(state);

        // Give the recorder ~1.5s to acquire the window buffer.
        // ScreenCaptureKit's first frame can take a moment.
        sleep(Duration::from_millis(1500)).await;
        Ok(())
    }

    /// Mark a beat in the scenario timeline.
    ///
    /// Three modes:
    /// - **Continuous** (`start_recording` was called): logs + sleeps
    ///   `hold_sec` so the running recorder captures the right moment,
    ///   and pushes the beat into [`Tape::beats`] for `result.json`.
    /// - **No recording** (scenarios that drive the UI but don't
    ///   record — e.g. headless tests, smoke checks): logs + sleeps
    ///   `hold_sec` but DOESN'T record a beat (no recording → no
    ///   timeline to annotate). Letting `scene` be a no-op here means
    ///   the same scenario code runs in both test and production
    ///   contexts without the test having to call `start_recording`
    ///   first.
    /// - **Scene mode** (per-clip capture with burned captions): not
    ///   yet implemented; lands in a follow-up phase. The headline
    ///   continuous-mode tapes don't need it.
    pub async fn scene(&mut self, spec: SceneSpec) -> Result<()> {
        if let Some(ref state) = self.continuous {
            let t = state.started_at.elapsed().as_secs_f64();
            self.beats.push(ContinuousBeat {
                t,
                caption: spec.caption.clone(),
            });
            self.log(&format!("@{:.1}s — {}", t, spec.caption));
            sleep(Duration::from_secs(spec.hold_sec)).await;
            return Ok(());
        }
        // No recording active. Log the marker so scenario output still
        // reads sensibly, sleep the hold (in case the scenario depends
        // on the wall-clock pacing), and move on.
        self.scene_idx += 1;
        self.log(&format!("no-rec scene — {}", spec.caption));
        sleep(Duration::from_secs(spec.hold_sec)).await;
        Ok(())
    }

    /// Finalise the tape. Writes `result.json` with the assertions +
    /// beats + any `extras`, blocks on the recorder finishing if one
    /// is running, and returns whether every assertion passed.
    pub async fn finish(&mut self, extras: Value) -> Result<bool> {
        let passed = self.assertions.iter().all(|a| a.ok);
        let summary = ResultSummary {
            scenario: self.name.clone(),
            started_at: self.wall_started_at.clone(),
            passed,
            assertions: self.assertions.clone(),
            beats: self.beats.clone(),
            extras,
        };
        std::fs::create_dir_all(&self.out_dir)
            .with_context(|| format!("failed to create out_dir {}", self.out_dir.display()))?;
        let result_path = self.out_dir.join("result.json");
        let json = serde_json::to_string_pretty(&summary)?;
        std::fs::write(&result_path, json)
            .with_context(|| format!("failed to write {}", result_path.display()))?;

        if let Some(state) = self.continuous.take() {
            // Wait for the recorder to finish writing the .mov.
            {
                let mut recorder = self.recorder.lock().await;
                recorder.wait_for_finish()?;
            }
            self.log(&format!(
                "recording finished → {}",
                state.mov_path.display()
            ));

            if let Some(ref pp) = self.post_processing {
                let mp4_path = state.mov_path.with_extension("mp4");
                let gif_path = state.mov_path.with_extension("gif");
                self.log(&format!(
                    "post: mov → mp4 ({} → {})",
                    state.mov_path.display(),
                    mp4_path.display()
                ));
                post::convert_mov_to_mp4(
                    &pp.swift_bin,
                    &pp.mov_to_mp4_script,
                    &state.mov_path,
                    &mp4_path,
                )?;
                self.log(&format!(
                    "post: mp4 → gif (fps={} max_w={})",
                    state.gif_fps, state.gif_max_width
                ));
                post::convert_mp4_to_gif(
                    &pp.swift_bin,
                    &pp.mp4_to_gif_script,
                    &mp4_path,
                    &gif_path,
                    state.gif_fps,
                    state.gif_max_width,
                )?;
            } else {
                self.log("post: skipped (no PostProcessing configured)");
            }
        }

        self.log(&format!("finished; passed={passed}"));
        Ok(passed)
    }
}

/// Format `SystemTime::now()` as an ISO-8601 UTC string ("YYYY-MM-DD
/// T HH:MM:SS.mmmZ"), matching the TS port's `new Date(t0).toISOString()`.
/// Standalone so we don't pull in `chrono`/`time` for one call site.
fn chrono_like_now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format_iso_utc(elapsed.as_secs(), elapsed.subsec_millis())
}

fn format_iso_utc(secs_since_epoch: u64, millis: u32) -> String {
    // Days since 1970-01-01 + time-of-day component.
    let days = (secs_since_epoch / 86_400) as i64;
    let tod = secs_since_epoch % 86_400;
    let hour = (tod / 3600) as u32;
    let minute = ((tod % 3600) / 60) as u32;
    let second = (tod % 60) as u32;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Howard Hinnant's "days_from_civil" inverse — given days since
/// 1970-01-01, return (year, month, day) in the proleptic Gregorian
/// calendar. Handles leap years correctly through ~year 5000.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = y + if m <= 2 { 1 } else { 0 };
    (y as i32, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_spec_builder_defaults() {
        let spec = SceneSpec::new("hello");
        assert_eq!(spec.caption, "hello");
        assert_eq!(spec.record_sec, 2);
        assert_eq!(spec.hold_sec, 4);

        let custom = SceneSpec::new("custom").record_sec(5).hold_sec(10);
        assert_eq!(custom.record_sec, 5);
        assert_eq!(custom.hold_sec, 10);
    }

    #[test]
    fn continuous_beat_round_trips_via_serde() {
        let b = ContinuousBeat {
            t: 1.5,
            caption: "hostname check".into(),
        };
        let json = serde_json::to_string(&b).unwrap();
        let back: ContinuousBeat = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn format_iso_utc_matches_known_epoch_points() {
        // Unix epoch start.
        assert_eq!(format_iso_utc(0, 0), "1970-01-01T00:00:00.000Z");
        // 2026-06-07T12:34:56.789Z = 1780835696s + 789ms (verified via
        // `python -c "import datetime; print(int(datetime.datetime(2026,6,7,12,34,56,tzinfo=datetime.timezone.utc).timestamp()))"`).
        assert_eq!(format_iso_utc(1780835696, 789), "2026-06-07T12:34:56.789Z");
    }

    #[test]
    fn format_iso_utc_handles_leap_year_correctly() {
        // 2024-02-29T00:00:00Z = 1709164800
        assert_eq!(format_iso_utc(1709164800, 0), "2024-02-29T00:00:00.000Z");
        // 2025-02-28T00:00:00Z = 1740700800 (NOT a leap year)
        assert_eq!(format_iso_utc(1740700800, 0), "2025-02-28T00:00:00.000Z");
    }

    #[test]
    fn civil_from_days_handles_century_boundaries() {
        // 2000-01-01 is day 10957 since 1970-01-01.
        assert_eq!(civil_from_days(10957), (2000, 1, 1));
        // 2100-03-01 (skip the non-leap 2100-02-29).
        // 2100-01-01 = 47482, 2100-03-01 = 47541
        assert_eq!(civil_from_days(47541), (2100, 3, 1));
    }
}
