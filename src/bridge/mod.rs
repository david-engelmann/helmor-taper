//! WebSocket client for the `tauri-plugin-mcp-bridge` protocol.
//!
//! ## Wire protocol
//!
//! Request frame:
//! ```json
//! {"id": "<uuid>", "command": "<name>", "args": {...}}
//! ```
//!
//! Response frame (success):
//! ```json
//! {"id": "<uuid>", "success": true, "data": ...}
//! ```
//!
//! Response frame (failure):
//! ```json
//! {"id": "<uuid>", "success": false, "error": "..."}
//! ```
//!
//! Helmor's debug build hosts the bridge on `127.0.0.1:9223`, scanning
//! up to +100 ports (the runtime picks a port via `bind_dynamic`).
//! Connect by port-scanning that range until a `connect` succeeds.
//!
//! Commands used by helmor-taper:
//! - `list_windows` → array of window descriptors
//! - `execute_js {windowLabel, script}` → `{success, result}` — drives
//!   the live UI by evaluating JS inside the Tauri webview.
//! - `capture_native_screenshot {windowLabel, format, quality, maxWidth}`
//!   → `{dataUrl}` — screenshot of just the webview (NOT the OS chrome).
//! - `invoke_tauri {command, args}` → direct Tauri backend command.

mod client;
mod protocol;

pub use client::{Bridge, BridgeConfig, ConnectError};
pub use protocol::{BridgeError, BridgeRequest, BridgeResponse};

/// Default host for the MCP bridge.
pub const DEFAULT_HOST: &str = "127.0.0.1";

/// Default base port the bridge listens on (debug builds only).
pub const DEFAULT_BASE_PORT: u16 = 9223;

/// How many ports above `DEFAULT_BASE_PORT` to scan when looking for
/// the bridge. The runtime picks a free port in that window.
pub const DEFAULT_PORT_SCAN: u16 = 100;
