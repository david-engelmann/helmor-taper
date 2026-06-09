//! Headless probe for remote port forwarding. Starts a tiny HTTP
//! server in the container, asks Helmor to forward a local port → the
//! container's port, fetches the local URL, asserts the marker bytes
//! came back. Confirms `start_remote_port_forward` actually wires the
//! SSH control-master forward and `list_remote_port_forwards` reflects
//! the active entry.

use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;
use serde_json::json;
use tokio::time::sleep;

use crate::bridge::Bridge;
use crate::commands::invoke_and_wait;
use crate::probes::run_cmd;

#[derive(Debug, Clone)]
pub struct Config {
    pub runtime_name: String,
    pub container: String,
    pub remote_port: u16,
    pub local_port: u16,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            runtime_name: std::env::var("RUNTIME_NAME")
                .unwrap_or_else(|_| "docker-linux-arm64".into()),
            container: std::env::var("CONTAINER")
                .unwrap_or_else(|_| "helmor-test-linux-arm64".into()),
            remote_port: std::env::var("REMOTE_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(47_931),
            local_port: std::env::var("LOCAL_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(47_932),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PortForwardEntry {
    runtime_name: String,
    local_port: u16,
}

pub async fn run(bridge: &Bridge, config: &Config) -> Result<bool> {
    let marker = format!("PORTFWD_PROOF_{}", std::process::id());
    let inline_server = format!(
        "python3 -c 'import http.server, socket; s=http.server.HTTPServer((\"0.0.0.0\", {port}), type(\"H\", (http.server.BaseHTTPRequestHandler,), {{\"do_GET\": lambda self: (self.send_response(200), self.send_header(\"content-type\", \"text/plain\"), self.end_headers(), self.wfile.write(b\"{marker}\"))[0], \"log_message\": lambda *a, **k: None}})); s.serve_forever()'",
        port = config.remote_port,
    );
    let bg_cmd = format!("setsid {inline_server} > /tmp/pyhttp.log 2>&1 &");
    run_cmd(
        "docker",
        &["exec", "-u", "e2e", "-d", &config.container, "sh", "-c", &bg_cmd],
    )?;
    sleep(Duration::from_millis(800)).await;
    eprintln!("✓ test server listening on container :{}", config.remote_port);

    // Sanity probe inside the container.
    let inner_check = format!(
        "python3 -c \"import socket; s=socket.socket(); s.connect(('127.0.0.1', {port})); s.sendall(b'GET / HTTP/1.0\\r\\n\\r\\n'); import sys; sys.stdout.buffer.write(s.recv(4096))\"",
        port = config.remote_port,
    );
    let inner_body = run_cmd(
        "docker",
        &["exec", &config.container, "sh", "-c", &inner_check],
    )
    .unwrap_or_default();
    if !inner_body.contains(&marker) {
        eprintln!(
            "✗ test server not responding inside container; got: {}",
            inner_body.chars().take(200).collect::<String>()
        );
        return Ok(false);
    }
    eprintln!("✓ test server responds with {marker} inside the container");

    let timeout = Duration::from_secs(60);
    let started = invoke_and_wait(
        bridge,
        "start_remote_port_forward",
        json!({
            "runtimeName": config.runtime_name,
            "localPort": config.local_port,
            "remotePort": config.remote_port,
            "label": "probe",
        }),
        timeout,
        "pf-start",
    )
    .await;
    let started = match started {
        Ok(v) => v,
        Err(e) => {
            eprintln!("✗ start_remote_port_forward failed: {e}");
            return Ok(false);
        }
    };
    eprintln!(
        "✓ start_remote_port_forward → {} → {}:{}",
        started["localPort"], started["runtimeName"], started["remotePort"]
    );

    // Fetch via the LOCAL port.
    let url = format!("http://127.0.0.1:{}/", config.local_port);
    let (body, ok) = match std::process::Command::new("curl")
        .args(["-s", "--max-time", "5", &url])
        .output()
    {
        Ok(out) => (
            String::from_utf8_lossy(&out.stdout).to_string(),
            out.status.success(),
        ),
        Err(e) => {
            eprintln!("✗ curl spawn failed: {e}");
            (String::new(), false)
        }
    };
    let outer_hit = body.contains(&marker);
    eprintln!(
        "✓ localhost:{} responded body=\"{}\" (ok={ok})",
        config.local_port,
        body.chars().take(60).collect::<String>()
    );

    let listing: Vec<PortForwardEntry> = serde_json::from_value(
        invoke_and_wait(
            bridge,
            "list_remote_port_forwards",
            json!({}),
            timeout,
            "pf-list",
        )
        .await?,
    )?;
    let listed = listing
        .iter()
        .any(|e| e.runtime_name == config.runtime_name && e.local_port == config.local_port);
    eprintln!("✓ list_remote_port_forwards includes the new entry: {listed}");

    // Cleanup.
    let _ = invoke_and_wait(
        bridge,
        "stop_remote_port_forward",
        json!({"runtimeName": config.runtime_name, "localPort": config.local_port}),
        timeout,
        "pf-stop",
    )
    .await;
    let kill_cmd = format!(
        "pkill -f 'http.server.HTTPServer.*{}' 2>/dev/null || pkill -f 'HTTPServer' 2>/dev/null || true",
        config.remote_port,
    );
    let _ = run_cmd("docker", &["exec", &config.container, "sh", "-c", &kill_cmd]);

    Ok(outer_hit && listed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_defaults() {
        unsafe {
            std::env::remove_var("RUNTIME_NAME");
            std::env::remove_var("CONTAINER");
            std::env::remove_var("REMOTE_PORT");
            std::env::remove_var("LOCAL_PORT");
        }
        let c = Config::from_env();
        assert_eq!(c.runtime_name, "docker-linux-arm64");
        assert_eq!(c.remote_port, 47_931);
        assert_eq!(c.local_port, 47_932);
    }

    #[test]
    fn port_forward_entry_deserializes_camel_case() {
        let v = json!([{"runtimeName": "docker-linux-arm64", "localPort": 9999}]);
        let parsed: Vec<PortForwardEntry> = serde_json::from_value(v).unwrap();
        assert_eq!(parsed[0].runtime_name, "docker-linux-arm64");
        assert_eq!(parsed[0].local_port, 9999);
    }

}
