//! Recorder trait + the two implementations a scenario will use:
//! [`NullRecorder`] for tests and the (Phase R3) ScreenCaptureKit
//! recorder for production.
//!
//! Kept behind a trait so the [`super::Tape`] state machine + every
//! integration test below can be exercised without spawning the
//! Swift recording binary — the trait is `Send + Sync` so it can
//! live behind the `Arc<Mutex<_>>` in `Tape`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RecorderError {
    #[error("recorder already running (started at {started:?})")]
    AlreadyStarted { started: PathBuf },
    #[error("recorder not started — call start() first")]
    NotStarted,
    #[error("ScreenCaptureKit shim failed: {0}")]
    Shim(String),
    #[error("recorder i/o: {0}")]
    Io(#[from] std::io::Error),
}

/// Anything that can capture the Helmor window for a fixed duration.
/// `start()` is non-blocking — it spawns the recording in the
/// background; `wait_for_finish()` blocks until the recording's exit
/// (either the duration ran out or the recorder died).
///
/// Trait-objected via `Box<dyn Recorder>` in [`super::Tape`] so the
/// real implementation can swap in later without touching scenarios.
pub trait Recorder: Send + Sync + std::fmt::Debug {
    fn start(&mut self, out_path: &Path, duration: Duration) -> Result<(), RecorderError>;
    fn wait_for_finish(&mut self) -> Result<(), RecorderError>;
    /// Identifier used in `tracing` output / `result.json` diagnostic
    /// extras, e.g. "screen-capture-kit", "null", "stub".
    fn kind(&self) -> &'static str;
}

/// Records nothing — useful for tests that exercise [`super::Tape`]
/// without actually invoking ScreenCaptureKit. Tracks call counts so
/// tests can assert the contract.
#[derive(Debug, Default)]
pub struct NullRecorder {
    pub starts: Vec<(PathBuf, Duration)>,
    pub finishes: usize,
}

impl Recorder for NullRecorder {
    fn start(&mut self, out_path: &Path, duration: Duration) -> Result<(), RecorderError> {
        if let Some((path, _)) = self.starts.last() {
            return Err(RecorderError::AlreadyStarted {
                started: path.clone(),
            });
        }
        self.starts.push((out_path.to_path_buf(), duration));
        Ok(())
    }

    fn wait_for_finish(&mut self) -> Result<(), RecorderError> {
        if self.starts.is_empty() {
            return Err(RecorderError::NotStarted);
        }
        self.finishes += 1;
        Ok(())
    }

    fn kind(&self) -> &'static str {
        "null"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_recorder_records_starts_and_finishes() {
        let mut r = NullRecorder::default();
        r.start(Path::new("/tmp/foo.mov"), Duration::from_secs(3))
            .unwrap();
        r.wait_for_finish().unwrap();
        assert_eq!(r.starts.len(), 1);
        assert_eq!(r.starts[0].0, PathBuf::from("/tmp/foo.mov"));
        assert_eq!(r.starts[0].1, Duration::from_secs(3));
        assert_eq!(r.finishes, 1);
    }

    #[test]
    fn null_recorder_rejects_double_start() {
        let mut r = NullRecorder::default();
        r.start(Path::new("/tmp/a.mov"), Duration::from_secs(1))
            .unwrap();
        let err = r
            .start(Path::new("/tmp/b.mov"), Duration::from_secs(1))
            .expect_err("must reject second start");
        assert!(matches!(err, RecorderError::AlreadyStarted { .. }));
    }

    #[test]
    fn null_recorder_rejects_finish_before_start() {
        let mut r = NullRecorder::default();
        let err = r.wait_for_finish().expect_err("must require start first");
        assert!(matches!(err, RecorderError::NotStarted));
    }

    #[test]
    fn null_recorder_advertises_kind() {
        let r = NullRecorder::default();
        assert_eq!(r.kind(), "null");
    }
}
