//! `taper` — top-level CLI for the helmor-taper Rust port.
//!
//! Subcommands:
//! - `ping` — connect to the bridge, run `list_windows`, print the
//!   port that landed and the window count. Smoke test for "is the
//!   bridge reachable?" without writing any code that calls into it.
//! - `eval <script>` — evaluate JS in the `main` window via
//!   `execute_js`, print the JSON return value.
//! - `windows` — dump the `list_windows` payload.
//! - `scenario <name>` — run a Rust-ported scenario by name. Reads
//!   per-scenario config from env vars (see each scenario's
//!   `Config::from_env`). Output dir defaults to
//!   `./tapes/<name>`; override with `TAPE_DIR`.
//!
//! Available scenarios:
//! - `connect-over-ssh` — port of `scenarios/connect-over-ssh.ts`
//! - `remote-workspace` — port of `scenarios/remote-workspace.ts`

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use helmor_taper::probes::{
    bundle_install, daemon_persistence, feature_probe, remote_agent, remote_port_forward,
    remote_terminal, remote_watch,
};
use helmor_taper::scenarios::{
    add_remote_wizard, agent_on_remote, chat_real_on_remote, connect_over_ssh, end_to_end_demo,
    first_connect_bundle, isolation_proof, observability, remote_file_ops, remote_runner,
    remote_workspace, resilience, row_actions,
};
use helmor_taper::{
    Bridge, BridgeConfig, PostProcessing, ScreenCaptureKitRecorder, TapeBuilder,
};
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
    eprintln!("  scenario <name>     Run a Rust-ported scenario by name");
    eprintln!();
    eprintln!("Scenarios:");
    eprintln!("  connect-over-ssh    SSH connect → daemon health → connected row");
    eprintln!("  remote-workspace    Select remote-bound workspace → header chip live");
    eprintln!("  row-actions         Auth / Diagnostics / Disconnect affordances per row");
    eprintln!("  observability       Runtime Debug → diagnostics + metrics + log tail");
    eprintln!("  add-remote-wizard   Add-remote wizard surfaces SSH state pre-connect");
    eprintln!("  resilience          docker stop → banner → Reconnect → green");
    eprintln!("  first-connect-bundle Auto-install agent runtime on first connect");
    eprintln!("  remote-file-ops     File tree + changes route to the container");
    eprintln!("  remote-runner       SSH connect + backend-truth: list_remote_runtimes");
    eprintln!("  isolation-proof     Chat exchanges prove agent runs on container");
    eprintln!("  agent-on-remote     send_agent_message_stream → session row populates");
    eprintln!("  chat-real-on-remote Composer-driven chat: ls + README + file creation");
    eprintln!("  end-to-end-demo     THE demo — full user journey, 75-90s gif");
    eprintln!();
    eprintln!("Probes (headless feature checks; `taper probe <name>`):");
    eprintln!("  bundle-install      Install pipeline → manifest → reconnect → agent.send");
    eprintln!("  daemon-persistence  Daemon PID survives disconnect/reconnect");
    eprintln!("  remote-agent        send_agent_message_stream streams events back");
    eprintln!("  remote-port-forward Local port → container service via SSH forward");
    eprintln!("  remote-terminal     PTY hosted on container (whoami/hostname/pwd)");
    eprintln!("  remote-watch        Filesystem watcher fires WorkspaceFilesChanged");
    eprintln!("  feature-probe       Sweep of 19+ feature surfaces, JSON report");
}

async fn dispatch(subcommand: &str, rest: &[String]) -> anyhow::Result<()> {
    match subcommand {
        "scenario" => run_scenario(rest).await,
        "probe" => run_probe(rest).await,
        "ping" | "windows" | "eval" => run_bridge_command(subcommand, rest).await,
        other => anyhow::bail!("unknown subcommand: {other}"),
    }
}

async fn run_probe(rest: &[String]) -> anyhow::Result<()> {
    let name = rest
        .first()
        .ok_or_else(|| anyhow::anyhow!("probe requires a name: taper probe <name>"))?
        .as_str();
    let bridge = Bridge::connect(BridgeConfig::default()).await?;
    eprintln!("connected on port {}", bridge.port());

    let passed = match name {
        "bundle-install" => {
            bundle_install::run(&bridge, &bundle_install::Config::from_env()).await?
        }
        "daemon-persistence" => {
            daemon_persistence::run(&bridge, &daemon_persistence::Config::from_env()).await?
        }
        "remote-agent" => remote_agent::run(&bridge, &remote_agent::Config::from_env()).await?,
        "remote-port-forward" => {
            remote_port_forward::run(&bridge, &remote_port_forward::Config::from_env()).await?
        }
        "remote-terminal" => {
            remote_terminal::run(&bridge, &remote_terminal::Config::from_env()).await?
        }
        "remote-watch" => remote_watch::run(&bridge, &remote_watch::Config::from_env()).await?,
        "feature-probe" => feature_probe::run(&bridge, &feature_probe::Config::from_env()).await?,
        other => anyhow::bail!(
            "unknown probe: {other}. Available: bundle-install, daemon-persistence, remote-agent, remote-port-forward, remote-terminal, remote-watch, feature-probe"
        ),
    };
    if !passed {
        std::process::exit(1);
    }
    Ok(())
}

async fn run_bridge_command(subcommand: &str, rest: &[String]) -> anyhow::Result<()> {
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
        _ => unreachable!(),
    }
}

async fn run_scenario(rest: &[String]) -> anyhow::Result<()> {
    let name = rest
        .first()
        .ok_or_else(|| anyhow::anyhow!("scenario requires a name: taper scenario <name>"))?
        .as_str();

    let out_dir = std::env::var("TAPE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("./tapes/{name}")));

    let scripts_dir = std::env::var("HELMOR_TAPER_SCRIPTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            // Default: scripts live alongside the crate root.
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts")
        });

    let recorder = Box::new(ScreenCaptureKitRecorder::new(
        scripts_dir.join("record-window.swift"),
    ));
    let post = PostProcessing::from_scripts_dir(&scripts_dir);

    let mut tape = TapeBuilder::new(name, &out_dir)
        .recorder(recorder)
        .post_processing(post)
        .build()
        .await?;

    let passed = match name {
        "connect-over-ssh" => {
            connect_over_ssh::run(&mut tape, &connect_over_ssh::Config::from_env()).await?
        }
        "remote-workspace" => {
            remote_workspace::run(&mut tape, &remote_workspace::Config::from_env()).await?
        }
        "row-actions" => row_actions::run(&mut tape, &row_actions::Config::from_env()).await?,
        "observability" => {
            observability::run(&mut tape, &observability::Config::from_env()).await?
        }
        "add-remote-wizard" => {
            add_remote_wizard::run(&mut tape, &add_remote_wizard::Config::from_env()).await?
        }
        "resilience" => resilience::run(&mut tape, &resilience::Config::from_env()).await?,
        "first-connect-bundle" => {
            first_connect_bundle::run(&mut tape, &first_connect_bundle::Config::from_env()).await?
        }
        "remote-file-ops" => {
            remote_file_ops::run(&mut tape, &remote_file_ops::Config::from_env()).await?
        }
        "remote-runner" => {
            remote_runner::run(&mut tape, &remote_runner::Config::from_env()).await?
        }
        "isolation-proof" => {
            isolation_proof::run(&mut tape, &isolation_proof::Config::from_env()).await?
        }
        "agent-on-remote" => {
            agent_on_remote::run(&mut tape, &agent_on_remote::Config::from_env()).await?
        }
        "chat-real-on-remote" => {
            chat_real_on_remote::run(&mut tape, &chat_real_on_remote::Config::from_env()).await?
        }
        "end-to-end-demo" => {
            end_to_end_demo::run(&mut tape, &end_to_end_demo::Config::from_env()).await?
        }
        other => anyhow::bail!(
            "unknown scenario: {other}. Available: connect-over-ssh, remote-workspace, row-actions, observability, add-remote-wizard, resilience, first-connect-bundle, remote-file-ops, remote-runner, isolation-proof, agent-on-remote, chat-real-on-remote, end-to-end-demo"
        ),
    };

    if !passed {
        std::process::exit(1);
    }
    Ok(())
}
