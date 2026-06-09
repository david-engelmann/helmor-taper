//! helmor-taper — recorded-evidence harness for Helmor.
//!
//! Drives the live Helmor desktop UI through the `tauri-plugin-mcp-bridge`
//! WebSocket protocol while ScreenCaptureKit records the window in
//! parallel. Far more reliable than blind `CGEventPost` + OCR, because
//! we target real DOM elements and can invoke backend Tauri commands
//! directly.
//!
//! Crate layout:
//!
//! - [`bridge`] — WebSocket client for the MCP bridge. Wire types,
//!   port-scanning connect logic, request/response correlation by UUID.
//! - `scenarios` (Phase R4, in progress) — recorded flows that drive
//!   the UI and assert outcomes.
//! - `recording` (Phase R3, in progress) — Swift shim integration for
//!   ScreenCaptureKit.
//!
//! ## TypeScript parity goal
//!
//! This crate is the Rust replacement for the TypeScript implementation
//! still present alongside it in this repo (`scripts/mcp-bridge.ts`,
//! `scenarios/*.ts`). The TypeScript scaffolding stays in place until
//! the Rust port reaches feature parity, then it gets removed in one
//! sweep so a reviewer never sees a half-migrated codebase.

pub mod bridge;
pub mod commands;
pub mod scenarios;
pub mod tape;

pub use bridge::{
    Bridge, BridgeConfig, BridgeError, BridgeRequest, BridgeResponse, ConnectError,
    DEFAULT_BASE_PORT, DEFAULT_HOST, DEFAULT_PORT_SCAN,
};
pub use commands::{
    capture_screenshot, execute_js, invoke_and_wait, invoke_command, poll_result, PollResult,
    DEFAULT_WINDOW,
};
pub use tape::{
    convert_mov_to_mp4, convert_mp4_to_gif, Assertion, ContinuousBeat, NullRecorder, PostError,
    PostProcessing, Recorder, RecorderError, ResultSummary, SceneSpec, ScreenCaptureKitRecorder,
    Tape, TapeBuilder, DEFAULT_OWNER,
};
