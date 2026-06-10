//! Post-processing pipeline: `.mov → .mp4 → .gif`. Wraps the two
//! Swift conversion shims (`scripts/mov-to-mp4.swift`,
//! `scripts/mp4-to-gif.swift`) with the same shape the recorder uses:
//! a configurable swift binary path so tests can substitute a shell
//! shim, structured errors with stderr captured.
//!
//! Why not just remux/encode in Rust? AVFoundation is the macOS-native
//! source-of-truth — `AVAssetExportSession` does a passthrough mov→mp4
//! with no re-encode, and `AVAssetImageGenerator` produces sharp gifs
//! without ffmpeg's palette/dither artifacts. Replicating either in
//! Rust would be ~10× the code for a worse output. Subprocess calls
//! are the right altitude.

use std::path::Path;
use std::process::{Command, Stdio};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PostError {
    #[error("failed to spawn `{tool}`: {source}")]
    Spawn {
        tool: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{tool} exited {status}: {stderr}")]
    NonZero {
        tool: String,
        status: String,
        stderr: String,
    },
    #[error("post-processing i/o: {0}")]
    Io(#[from] std::io::Error),
}

/// Re-mux a .mov into a .mp4 (no re-encode, just a container swap).
/// Browsers don't natively play `.mov`; every browser plays `.mp4`
/// with H.264. Used to produce the README-embeddable asset.
pub fn convert_mov_to_mp4(
    swift_bin: &Path,
    script_path: &Path,
    input_mov: &Path,
    output_mp4: &Path,
) -> Result<(), PostError> {
    spawn_swift_tool(
        "mov-to-mp4",
        swift_bin,
        script_path,
        &[input_mov.as_os_str(), output_mp4.as_os_str()],
    )
}

/// Convert a .mp4 to a .gif at `fps` frames per second, downscaled so
/// `width ≤ max_width` (aspect ratio preserved).
pub fn convert_mp4_to_gif(
    swift_bin: &Path,
    script_path: &Path,
    input_mp4: &Path,
    output_gif: &Path,
    fps: u32,
    max_width: u32,
) -> Result<(), PostError> {
    let fps_str = fps.to_string();
    let max_width_str = max_width.to_string();
    spawn_swift_tool(
        "mp4-to-gif",
        swift_bin,
        script_path,
        &[
            input_mp4.as_os_str(),
            output_gif.as_os_str(),
            std::ffi::OsStr::new(&fps_str),
            std::ffi::OsStr::new(&max_width_str),
        ],
    )
}

fn spawn_swift_tool(
    tool: &str,
    swift_bin: &Path,
    script_path: &Path,
    args: &[&std::ffi::OsStr],
) -> Result<(), PostError> {
    let mut cmd = Command::new(swift_bin);
    cmd.arg(script_path).args(args).stdin(Stdio::null());
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|source| PostError::Spawn {
            tool: tool.to_string(),
            source,
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(PostError::NonZero {
            tool: tool.to_string(),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn make_shim(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        let mut perms = f.metadata().unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[test]
    fn mov_to_mp4_happy_path_copies_bytes() {
        let dir = tempdir().unwrap();
        let shim = make_shim(
            dir.path(),
            "mov2mp4.sh",
            r#"#!/usr/bin/env bash
# $1=<script-arg> $2=<input.mov> $3=<output.mp4>
cp "$2" "$3"
echo "shim: remuxed $2 -> $3" >&2
exit 0
"#,
        );

        let mov = dir.path().join("master.mov");
        let mp4 = dir.path().join("master.mp4");
        std::fs::write(&mov, b"FAKE_MOV").unwrap();

        convert_mov_to_mp4(&shim, Path::new("ignored-script-arg"), &mov, &mp4).expect("happy path");

        assert_eq!(std::fs::read(&mp4).unwrap(), b"FAKE_MOV");
    }

    #[test]
    fn mp4_to_gif_passes_fps_and_max_width() {
        let dir = tempdir().unwrap();
        // Shim asserts argv shape: $1=script $2=mp4 $3=gif $4=fps $5=maxWidth.
        let shim = make_shim(
            dir.path(),
            "mp42gif.sh",
            r#"#!/usr/bin/env bash
[ "$4" = "5" ] || { echo "BAD FPS $4" >&2; exit 11; }
[ "$5" = "720" ] || { echo "BAD MAX_WIDTH $5" >&2; exit 12; }
echo "FAKE_GIF" > "$3"
exit 0
"#,
        );

        let mp4 = dir.path().join("master.mp4");
        let gif = dir.path().join("master.gif");
        std::fs::write(&mp4, b"FAKE_MP4").unwrap();

        convert_mp4_to_gif(&shim, Path::new("ignored"), &mp4, &gif, 5, 720).expect("happy path");

        assert_eq!(std::fs::read(&gif).unwrap(), b"FAKE_GIF\n");
    }

    #[test]
    fn non_zero_exit_surfaces_tool_name_and_stderr() {
        let dir = tempdir().unwrap();
        let shim = make_shim(
            dir.path(),
            "fails.sh",
            r#"#!/usr/bin/env bash
echo "passthrough not available on this platform" >&2
exit 1
"#,
        );
        let mov = dir.path().join("a.mov");
        let mp4 = dir.path().join("a.mp4");
        std::fs::write(&mov, b"FAKE").unwrap();

        let err =
            convert_mov_to_mp4(&shim, Path::new("ignored"), &mov, &mp4).expect_err("non-zero exit");
        match err {
            PostError::NonZero { tool, stderr, .. } => {
                assert_eq!(tool, "mov-to-mp4");
                assert!(
                    stderr.contains("passthrough not available"),
                    "stderr passthrough: {stderr}"
                );
            }
            other => panic!("expected NonZero, got {other:?}"),
        }
    }

    #[test]
    fn missing_binary_surfaces_spawn_error_with_tool_name() {
        let dir = tempdir().unwrap();
        let mov = dir.path().join("a.mov");
        let mp4 = dir.path().join("a.mp4");
        std::fs::write(&mov, b"x").unwrap();
        let err = convert_mov_to_mp4(
            Path::new("/nonexistent/swift"),
            Path::new("ignored"),
            &mov,
            &mp4,
        )
        .expect_err("missing binary");
        match err {
            PostError::Spawn { tool, .. } => assert_eq!(tool, "mov-to-mp4"),
            other => panic!("expected Spawn, got {other:?}"),
        }
    }
}
