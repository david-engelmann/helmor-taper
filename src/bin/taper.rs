//! `taper` — top-level CLI for the helmor-taper Rust port.
//!
//! Subcommands during the migration (Phase R1):
//! - `ping` — connect to the bridge, run `list_windows`, print the
//!   port that landed and the window count. Smoke test for "is the
//!   bridge reachable?" without writing any code that calls into it.
//! - `eval <script>` — evaluate JS in the `main` window via
//!   `execute_js`, print the JSON return value.
//! - `windows` — dump the `list_windows` payload.
//!
//! More subcommands land as later phases port the scenarios.

use std::env;
use std::process::ExitCode;

use helmor_taper::{Bridge, BridgeConfig};
use serde_json::json;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("HELMOR_TAPER_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage(&args[0]);
        return ExitCode::from(2);
    }

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("failed to start tokio runtime: {err}");
            return ExitCode::from(1);
        }
    };

    let res = runtime.block_on(dispatch(&args[1], &args[2..]));
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(1)
        }
    }
}

fn print_usage(prog: &str) {
    eprintln!("Usage: {prog} <subcommand> [args...]");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  ping                Connect to the MCP bridge + list_windows");
    eprintln!("  eval '<js>'         Run JS in the main window, print JSON result");
    eprintln!("  windows             Dump list_windows JSON");
}

async fn dispatch(subcommand: &str, rest: &[String]) -> anyhow::Result<()> {
    let cfg = BridgeConfig::default();
    let bridge = Bridge::connect(cfg).await?;
    eprintln!("connected on port {}", bridge.port());

    match subcommand {
        "ping" => {
            let windows = bridge.request("list_windows", json!({})).await?;
            let count = windows.as_array().map(|a| a.len()).unwrap_or(0);
            println!("bridge reachable on port {}, {count} window(s)", bridge.port());
            Ok(())
        }
        "windows" => {
            let windows = bridge.request("list_windows", json!({})).await?;
            println!("{}", serde_json::to_string_pretty(&windows)?);
            Ok(())
        }
        "eval" => {
            let script = rest.first().ok_or_else(|| {
                anyhow::anyhow!("eval requires a script argument: taper eval '<js>'")
            })?;
            let result = bridge
                .request(
                    "execute_js",
                    json!({"windowLabel": "main", "script": script}),
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        other => {
            anyhow::bail!("unknown subcommand: {other}");
        }
    }
}
