//! [`Recorder`] implementation that shells out to the existing
//! `scripts/record-window.swift` ScreenCaptureKit shim.
//!
//! The Swift binary takes three positional args:
//! `swift record-window.swift <owner-substring> <duration-seconds> <out.mov>`
//! and writes the .mov on exit. We just spawn it, retain the child
//! handle so we can wait + reap it cleanly, and surface a structured
//! error if the shim exits non-zero.
//!
//! Test surface: the swift binary path is injectable (defaults to
//! whatever's on `PATH`), so a unit test can substitute `/usr/bin/true`
//! or a tiny shell shim that writes a synthetic .mov without needing
//! real ScreenCaptureKit. Tests cover: start → wait happy path, child
//! reaped on Drop without a wait, non-zero exit surfaces stderr, and
//! the AlreadyStarted guard.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use super::recorder::{Recorder, RecorderError};

/// Default substring matched against `kCGWindowOwnerName` in
/// `record-window.swift`. Overridden via the `HELMOR_TAPER_PROC_NAME`
/// env var or [`ScreenCaptureKitRecorder::with_owner`].
pub const DEFAULT_OWNER: &str = "Helmor";

/// Records the Helmor window via ScreenCaptureKit by spawning
/// [`scripts/record-window.swift`]. Owns the [`Child`] handle for the
/// duration of the recording so [`wait_for_finish`] can reap it and so
/// `Drop` can kill an orphan when a scenario panics mid-record.
pub struct ScreenCaptureKitRecorder {
    swift_bin: PathBuf,
    script_path: PathBuf,
    owner: String,
    child: Option<Child>,
    /// Most recent stderr capture from the swift shim. Populated when
    /// `wait_for_finish` discovers a non-zero exit so the error
    /// message can include the actual ScreenCaptureKit failure
    /// (permissions, no matching window, etc.).
    pub last_stderr: String,
}

impl std::fmt::Debug for ScreenCaptureKitRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreenCaptureKitRecorder")
            .field("swift_bin", &self.swift_bin)
            .field("script_path", &self.script_path)
            .field("owner", &self.owner)
            .field("active", &self.child.is_some())
            .finish()
    }
}

impl ScreenCaptureKitRecorder {
    /// Build a recorder targeting `script_path` (e.g.
    /// `helmor-taper/scripts/record-window.swift`). The owner defaults
    /// to "Helmor"; override via [`Self::with_owner`].
    pub fn new(script_path: impl Into<PathBuf>) -> Self {
        Self {
            swift_bin: PathBuf::from("swift"),
            script_path: script_path.into(),
            owner: std::env::var("HELMOR_TAPER_PROC_NAME")
                .unwrap_or_else(|_| DEFAULT_OWNER.to_string()),
            child: None,
            last_stderr: String::new(),
        }
    }

    /// Override the swift binary path. Used in tests to substitute a
    /// shell shim that writes a synthetic .mov.
    pub fn with_swift_bin(mut self, path: impl Into<PathBuf>) -> Self {
        self.swift_bin = path.into();
        self
    }

    /// Override the window-owner substring. Matched case-insensitively
    /// by the swift shim against `kCGWindowOwnerName`.
    pub fn with_owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = owner.into();
        self
    }
}

impl Recorder for ScreenCaptureKitRecorder {
    fn start(&mut self, out_path: &Path, duration: Duration) -> Result<(), RecorderError> {
        if self.child.is_some() {
            return Err(RecorderError::AlreadyStarted {
                started: out_path.to_path_buf(),
            });
        }
        let child = Command::new(&self.swift_bin)
            .arg(&self.script_path)
            .arg(&self.owner)
            .arg(format!("{}", duration.as_secs_f64()))
            .arg(out_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                RecorderError::Shim(format!(
                    "failed to spawn `{} {}`: {err}",
                    self.swift_bin.display(),
                    self.script_path.display()
                ))
            })?;
        self.child = Some(child);
        Ok(())
    }

    fn wait_for_finish(&mut self) -> Result<(), RecorderError> {
        let Some(mut child) = self.child.take() else {
            return Err(RecorderError::NotStarted);
        };
        // Capture stderr concurrently so a chatty shim doesn't block
        // the kernel pipe buffer and starve the process.
        let stderr_handle = child.stderr.take();
        let stderr_text = if let Some(mut stderr) = stderr_handle {
            std::thread::spawn(move || {
                use std::io::Read;
                let mut buf = String::new();
                let _ = stderr.read_to_string(&mut buf);
                buf
            })
        } else {
            std::thread::spawn(String::new)
        };
        let status = child.wait()?;
        self.last_stderr = stderr_text.join().unwrap_or_default();
        if status.success() {
            Ok(())
        } else {
            Err(RecorderError::Shim(format!(
                "record-window.swift exited {status}: {}",
                self.last_stderr.trim()
            )))
        }
    }

    fn kind(&self) -> &'static str {
        "screen-capture-kit"
    }
}

impl Drop for ScreenCaptureKitRecorder {
    /// If a scenario panics mid-record and we never call
    /// `wait_for_finish`, kill + reap the orphan so we don't leak a
    /// process whose `swift` parent inherits stdout/stderr from the
    /// caller's session.
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Create a tiny shell shim that mimics `record-window.swift`'s
    /// arg shape: it writes a synthetic .mov at the third positional
    /// arg, prints a recognisable line to stderr, and exits cleanly.
    fn make_happy_path_shim(dir: &Path) -> PathBuf {
        let shim = dir.join("happy-shim.sh");
        let body = r#"#!/usr/bin/env bash
# Mimics `swift record-window.swift owner duration out.mov`.
# Arg layout: $1=<script-or-noop> $2=<owner> $3=<duration> $4=<out>
echo "shim: owner=$2 duration=$3 out=$4" >&2
printf 'FAKE_MOV_BYTES' > "$4"
exit 0
"#;
        std::fs::write(&shim, body).unwrap();
        let mut perms = std::fs::metadata(&shim).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o755);
        }
        std::fs::set_permissions(&shim, perms).unwrap();
        shim
    }

    fn make_failing_shim(dir: &Path) -> PathBuf {
        let shim = dir.join("failing-shim.sh");
        let body = r#"#!/usr/bin/env bash
echo "shim: simulated screencapturekit failure (no matching window)" >&2
exit 1
"#;
        std::fs::write(&shim, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&shim).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&shim, p).unwrap();
        }
        shim
    }

    #[test]
    fn happy_path_writes_synthetic_mov_and_exits_clean() {
        let dir = tempdir().unwrap();
        let shim = make_happy_path_shim(dir.path());
        let mov_path = dir.path().join("out.mov");

        let mut recorder = ScreenCaptureKitRecorder::new("ignored-script-arg")
            .with_swift_bin(shim)
            .with_owner("Helmor");

        recorder
            .start(&mov_path, Duration::from_millis(500))
            .expect("start should succeed");
        recorder
            .wait_for_finish()
            .expect("happy-path shim should exit 0");

        let body = std::fs::read(&mov_path).unwrap();
        assert_eq!(body, b"FAKE_MOV_BYTES");
        // The shim logs the args it saw — confirm owner + duration
        // round-tripped through `start`'s argv builder.
        assert!(
            recorder.last_stderr.contains("owner=Helmor"),
            "stderr should include the owner arg: {}",
            recorder.last_stderr
        );
        assert!(
            recorder.last_stderr.contains("duration=0.5"),
            "stderr should include the duration as float: {}",
            recorder.last_stderr
        );
    }

    #[test]
    fn non_zero_exit_surfaces_stderr_in_shim_error() {
        let dir = tempdir().unwrap();
        let shim = make_failing_shim(dir.path());
        let mov_path = dir.path().join("out.mov");

        let mut recorder =
            ScreenCaptureKitRecorder::new("ignored").with_swift_bin(shim);
        recorder.start(&mov_path, Duration::from_secs(1)).unwrap();
        let err = recorder
            .wait_for_finish()
            .expect_err("failing shim must surface as error");

        match err {
            RecorderError::Shim(msg) => {
                assert!(
                    msg.contains("simulated screencapturekit failure"),
                    "shim error must carry stderr: {msg}"
                );
                assert!(msg.contains("exit"));
            }
            other => panic!("expected Shim, got {other:?}"),
        }
    }

    #[test]
    fn double_start_returns_already_started() {
        let dir = tempdir().unwrap();
        let shim = make_happy_path_shim(dir.path());
        let mov_a = dir.path().join("a.mov");
        let mov_b = dir.path().join("b.mov");

        let mut recorder =
            ScreenCaptureKitRecorder::new("ignored").with_swift_bin(shim);
        recorder.start(&mov_a, Duration::from_secs(1)).unwrap();
        let err = recorder
            .start(&mov_b, Duration::from_secs(1))
            .expect_err("second start must error");
        assert!(matches!(err, RecorderError::AlreadyStarted { .. }));
        // Reap the first child so the test doesn't leak a process.
        let _ = recorder.wait_for_finish();
    }

    #[test]
    fn wait_before_start_returns_not_started() {
        let mut recorder = ScreenCaptureKitRecorder::new("ignored");
        let err = recorder.wait_for_finish().expect_err("wait before start");
        assert!(matches!(err, RecorderError::NotStarted));
    }

    #[test]
    fn missing_swift_binary_surfaces_spawn_error() {
        let dir = tempdir().unwrap();
        let mov = dir.path().join("out.mov");
        let mut recorder = ScreenCaptureKitRecorder::new("ignored")
            .with_swift_bin("/nonexistent/path/to/swift");
        let err = recorder
            .start(&mov, Duration::from_secs(1))
            .expect_err("missing binary must error");
        match err {
            RecorderError::Shim(msg) => {
                assert!(msg.contains("failed to spawn"), "got: {msg}");
            }
            other => panic!("expected Shim, got {other:?}"),
        }
    }

    #[test]
    fn drop_kills_orphan_child() {
        // Spawn a shim that would sleep for 30s if we let it. The
        // Drop impl on the recorder must kill it well before that —
        // we assert by reading the wall clock around the drop.
        let dir = tempdir().unwrap();
        let shim = dir.path().join("sleepy.sh");
        let body = r#"#!/usr/bin/env bash
sleep 30
"#;
        std::fs::write(&shim, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&shim).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&shim, p).unwrap();
        }
        let mov = dir.path().join("out.mov");

        let started = std::time::Instant::now();
        {
            let mut recorder = ScreenCaptureKitRecorder::new("ignored")
                .with_swift_bin(shim);
            recorder.start(&mov, Duration::from_secs(30)).unwrap();
            // Drop without wait — Drop must reap the child.
        }
        let drop_elapsed = started.elapsed();
        assert!(
            drop_elapsed < Duration::from_secs(5),
            "Drop should kill the orphan promptly, took {drop_elapsed:?}"
        );
    }

    #[test]
    fn kind_advertises_screen_capture_kit() {
        let r = ScreenCaptureKitRecorder::new("ignored");
        assert_eq!(r.kind(), "screen-capture-kit");
    }

    #[test]
    fn shim_receives_correct_arg_layout() {
        // Belt-and-braces: confirm the four positional args are in
        // the right order (script, owner, duration, out_path).
        let dir = tempdir().unwrap();
        let shim_path = dir.path().join("arg-checker.sh");
        let body = r#"#!/usr/bin/env bash
# expected: $1=<script-arg> $2=<owner> $3=<duration> $4=<out>
[ "$2" = "DocTestOwner" ] || { echo "BAD OWNER $2" >&2; exit 11; }
[ "$3" = "2" ] || { echo "BAD DURATION $3" >&2; exit 12; }
[[ "$4" == *"out.mov" ]] || { echo "BAD OUT $4" >&2; exit 13; }
echo "OK $2 $3 $4" >&2
echo "FAKE" > "$4"
exit 0
"#;
        std::fs::write(&shim_path, body).unwrap();
        let _ = &dir; // anchor lifetime
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&shim_path).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&shim_path, p).unwrap();
        }

        let mov_path = dir.path().join("out.mov");
        let mut recorder = ScreenCaptureKitRecorder::new("ignored-script")
            .with_swift_bin(&shim_path)
            .with_owner("DocTestOwner");

        recorder.start(&mov_path, Duration::from_secs(2)).unwrap();
        recorder.wait_for_finish().expect("arg checker should exit 0");
        assert!(recorder.last_stderr.contains("OK DocTestOwner 2"));
    }
}
