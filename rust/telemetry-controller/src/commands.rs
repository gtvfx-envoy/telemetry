//! Command handlers for `telemetry-controller`'s subcommands.

use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::compose;
use crate::grafana;
use crate::native::NativeLayout;
use crate::ports;
use crate::process::is_process_running;
use crate::runtime::{resolve_runtime, RuntimeChoice};
use crate::state::{load_state, save_state, RuntimeKind, ServerState};
use crate::sweep;
use crate::tempo_config;

const GRAFANA_URL: &str = "http://localhost:3000";

/// `telemetry-controller start [--runtime auto|compose|native]`.
///
/// Idempotent: adding/starting the bundle never launches background
/// services on its own, and a second `start` while already running is a
/// no-op that reports success rather than relaunching or erroring.
pub fn start(runtime_flag: &str) -> i32 {
    let choice: RuntimeChoice = match runtime_flag.parse() {
        Ok(choice) => choice,
        Err(message) => {
            eprintln!("Error: {message}");
            return 1;
        }
    };

    let runtime = match resolve_runtime(choice) {
        Ok(runtime) => runtime,
        Err(message) => {
            eprintln!("Error: {message}");
            return 1;
        }
    };

    let mut state = load_state();

    if is_already_running(&state) {
        println!(
            "Already running ({}). Use `telemetry-controller status` for details.",
            runtime.as_str()
        );
        return 0;
    }

    let occupied = ports::find_occupied_ports(ports::DEFAULT_PORTS);
    if !occupied.is_empty() {
        eprintln!("Error: the following ports are already in use:");
        for (name, port) in &occupied {
            eprintln!("  {name}: {port}");
        }
        eprintln!(
            "Stop whatever is using them, or clear a stale instance with `telemetry-controller stop`."
        );
        return 1;
    }

    ensure_generated_credentials(&mut state);

    let start_result = match runtime {
        RuntimeKind::Compose => start_compose(&state),
        RuntimeKind::Native => start_native(&mut state),
    };
    if let Err(message) = start_result {
        eprintln!("Error: {message}");
        return 1;
    }

    state.runtime = Some(runtime);
    state.started_at = Some(now_timestamp());
    if let Err(error) = save_state(&state) {
        eprintln!("Warning: failed to persist controller state: {error}");
    }

    println!("Started ({}).", runtime.as_str());
    println!("Grafana: {GRAFANA_URL}");
    if let Some(password) = &state.grafana_viewer_password {
        println!("  studio-viewer password (printed once, recoverable via `status`): {password}");
    }
    0
}

fn ensure_generated_credentials(state: &mut ServerState) {
    if state.grafana_admin_password.is_none() {
        state.grafana_admin_password = Some(grafana::generate_password());
    }
    if state.grafana_viewer_password.is_none() {
        state.grafana_viewer_password = Some(grafana::generate_password());
    }
    if state.collector_bearer_token.is_none() {
        state.collector_bearer_token = Some(grafana::generate_password());
    }
    if state.retention_days.is_none() {
        state.retention_days = Some(30);
    }
}

fn start_compose(state: &ServerState) -> Result<(), String> {
    let compose_file = compose::default_compose_file()
        .ok_or_else(|| "could not locate docker-compose.yml alongside this binary".to_string())?;
    let env = [
        (
            "GRAFANA_ADMIN_PASSWORD",
            state.grafana_admin_password.clone().unwrap_or_default(),
        ),
        (
            "GRAFANA_VIEWER_PASSWORD",
            state.grafana_viewer_password.clone().unwrap_or_default(),
        ),
        (
            "COLLECTOR_BEARER_TOKEN",
            state.collector_bearer_token.clone().unwrap_or_default(),
        ),
        (
            "TEMPO_RETENTION_HOURS",
            (state.retention_days.unwrap_or(30) * 24).to_string(),
        ),
    ];
    compose::up(&compose_file, &env)?;

    // Best-effort: provision the shared "studio-viewer" account once
    // Grafana is reachable. A failure here is reported but does not fail
    // `start` overall -- the rest of the stack (Collector, Tempo) is
    // already up and usable, and an operator can re-run `start` to retry
    // provisioning once Grafana catches up.
    if let (Some(admin_password), Some(viewer_password)) = (
        &state.grafana_admin_password,
        &state.grafana_viewer_password,
    ) {
        if let Err(message) =
            grafana::ensure_viewer_account(GRAFANA_URL, admin_password, viewer_password)
        {
            eprintln!(
                "Warning: could not provision the shared studio-viewer Grafana account yet: \
{message}. Re-run `telemetry-controller start` in a minute to retry."
            );
        }
    }

    Ok(())
}

fn start_native(state: &mut ServerState) -> Result<(), String> {
    let layout = NativeLayout::discover().ok_or_else(|| {
        "could not locate the native/ artifact directory alongside this binary".to_string()
    })?;
    let missing = layout.missing_artifacts();
    if !missing.is_empty() {
        return Err(format!(
            "missing native artifacts:\n  {}",
            missing.join("\n  ")
        ));
    }

    if let Some(days) = state.retention_days {
        tempo_config::set_retention_days(&layout.tempo_config(), days)?;
    }

    let collector_pid = spawn_native(
        &layout.collector_binary(),
        &[
            "--config".to_string(),
            layout.collector_config().display().to_string(),
        ],
    )?;
    let tempo_pid = spawn_native(
        &layout.tempo_binary(),
        &[
            "-config.file".to_string(),
            layout.tempo_config().display().to_string(),
        ],
    )?;
    let grafana_pid = spawn_native(&layout.grafana_binary(), &[])?;

    state
        .native_pids
        .insert("collector".to_string(), collector_pid);
    state.native_pids.insert("tempo".to_string(), tempo_pid);
    state.native_pids.insert("grafana".to_string(), grafana_pid);
    Ok(())
}

fn spawn_native(binary: &std::path::Path, args: &[String]) -> Result<u32, String> {
    Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|child| child.id())
        .map_err(|error| format!("failed to launch {}: {error}", binary.display()))
}

/// Idempotency check: is the runtime this state describes actually still
/// running, or is this a stale record (e.g. a prior hard crash)?
fn is_already_running(state: &ServerState) -> bool {
    match state.runtime {
        Some(RuntimeKind::Compose) => compose::default_compose_file()
            .map(|compose_file| {
                compose::ps_json(&compose_file)
                    .map(|json| !json.trim().is_empty())
                    .unwrap_or(false)
            })
            .unwrap_or(false),
        Some(RuntimeKind::Native) => {
            !state.native_pids.is_empty()
                && state
                    .native_pids
                    .values()
                    .all(|pid| is_process_running(*pid))
        }
        None => false,
    }
}

/// `telemetry-controller stop`. Preserves telemetry data.
pub fn stop() -> i32 {
    let mut state = load_state();

    match state.runtime {
        Some(RuntimeKind::Compose) => {
            let Some(compose_file) = compose::default_compose_file() else {
                eprintln!("Error: could not locate docker-compose.yml alongside this binary.");
                return 1;
            };
            if let Err(message) = compose::down(&compose_file) {
                eprintln!("Error stopping Compose stack: {message}");
                return 1;
            }
        }
        Some(RuntimeKind::Native) => {
            stop_native_processes(&state);
        }
        None => {
            println!("Not running.");
            return 0;
        }
    }

    state.runtime = None;
    state.native_pids.clear();
    state.started_at = None;
    if let Err(error) = save_state(&state) {
        eprintln!("Warning: failed to persist controller state: {error}");
    }
    println!("Stopped. Telemetry data was preserved.");
    0
}

#[cfg(unix)]
fn stop_native_processes(state: &ServerState) {
    for pid in state.native_pids.values() {
        // SAFETY: `kill` is a plain libc syscall; passing an out-of-range
        // or already-exited pid is not memory-unsafe, it simply fails.
        unsafe {
            libc_kill(*pid as i32, 15);
        }
    }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

#[cfg(not(unix))]
fn stop_native_processes(state: &ServerState) {
    for pid in state.native_pids.values() {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// `telemetry-controller status`.
pub fn status() -> i32 {
    let state = load_state();
    let running = is_already_running(&state);

    println!("Telemetry server status");
    println!("{}", "=".repeat(48));
    match state.runtime {
        Some(runtime) => println!(
            "Runtime:   {} ({})",
            runtime.as_str(),
            if running {
                "running"
            } else {
                "STALE -- recorded but not actually running"
            }
        ),
        None => println!("Runtime:   not started"),
    }
    if let Some(started_at) = &state.started_at {
        println!("Started:   {started_at}");
    }
    println!("Retention: {} days", live_retention_days(&state));
    println!("Grafana:   {GRAFANA_URL}");
    println!(
        "Collector: OTLP/HTTP token configured: {}",
        state.collector_bearer_token.is_some()
    );
    if let Some(password) = &state.grafana_viewer_password {
        println!("Studio-viewer password: {password}");
    }

    if running {
        0
    } else if state.runtime.is_some() {
        // Stale state is reported, not silently hidden, but is not itself
        // a hard failure -- `start` will clear it on the next successful
        // launch.
        1
    } else {
        0
    }
}

/// Prefer the live Tempo config file's actual retention value (source of
/// truth) when it can be read; fall back to the last value `configure`
/// recorded in state, and finally the 30-day default.
fn live_retention_days(state: &ServerState) -> u32 {
    let live_value = match state.runtime {
        Some(RuntimeKind::Native) => NativeLayout::discover()
            .and_then(|layout| tempo_config::get_retention_days(&layout.tempo_config())),
        _ => compose::default_compose_file()
            .and_then(|compose_file| {
                compose_file
                    .parent()
                    .map(|dir| dir.join("tempo").join("tempo.yaml"))
            })
            .and_then(|path| tempo_config::get_retention_days(&path)),
    };
    live_value.or(state.retention_days).unwrap_or(30)
}

/// `telemetry-controller logs [SERVICE]`.
pub fn logs(service: Option<&str>) -> i32 {
    let state = load_state();
    match state.runtime {
        Some(RuntimeKind::Compose) => {
            let Some(compose_file) = compose::default_compose_file() else {
                eprintln!("Error: could not locate docker-compose.yml alongside this binary.");
                return 1;
            };
            match compose::logs(&compose_file, service) {
                Ok(()) => 0,
                Err(message) => {
                    eprintln!("Error: {message}");
                    1
                }
            }
        }
        Some(RuntimeKind::Native) => {
            eprintln!(
                "Native-mode logs are written to stdout/stderr redirection files under the \
config root in a future release; for now, inspect the process directly."
            );
            1
        }
        None => {
            println!("Not running.");
            0
        }
    }
}

/// `telemetry-controller open`.
pub fn open() -> i32 {
    let result = if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/c", "start", "", GRAFANA_URL])
            .spawn()
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(GRAFANA_URL).spawn()
    } else {
        Command::new("xdg-open").arg(GRAFANA_URL).spawn()
    };

    match result {
        Ok(_) => 0,
        Err(error) => {
            eprintln!("Error: failed to open {GRAFANA_URL}: {error}");
            1
        }
    }
}

/// `telemetry-controller configure --retention-days=N`.
pub fn configure(retention_days: u32) -> i32 {
    let mut state = load_state();
    state.retention_days = Some(retention_days);

    let config_path = match state.runtime {
        Some(RuntimeKind::Native) => NativeLayout::discover().map(|layout| layout.tempo_config()),
        _ => compose::default_compose_file().and_then(|compose_file| {
            compose_file
                .parent()
                .map(|dir| dir.join("tempo").join("tempo.yaml"))
        }),
    };

    if let Some(path) = config_path.filter(|path| path.is_file()) {
        if let Err(message) = tempo_config::set_retention_days(&path, retention_days) {
            eprintln!("Error updating Tempo config: {message}");
            return 1;
        }
        println!("Updated {} to {retention_days} days.", path.display());
        if is_already_running(&state) {
            println!("Restart the server (`telemetry-controller stop` then `start`) for Tempo to pick up the new retention.");
        }
    } else {
        println!(
            "Recorded retention_days={retention_days} in controller state; Tempo config file \
not found yet (will apply on the next `start`)."
        );
    }

    if let Err(error) = save_state(&state) {
        eprintln!("Warning: failed to persist controller state: {error}");
    }
    0
}

/// `telemetry-controller sweep --drop-dir DIR --collector-endpoint URL [--daemon]`.
pub fn sweep_command(
    drop_dir: &std::path::Path,
    collector_endpoint: &str,
    daemon: bool,
    poll_interval: Duration,
) -> i32 {
    let state = load_state();
    let bearer_token = state.collector_bearer_token.clone();
    let mut retry_counts = std::collections::HashMap::new();

    loop {
        let outcome = sweep::sweep_once(
            drop_dir,
            collector_endpoint,
            bearer_token.as_deref(),
            sweep::DEFAULT_MAX_RETRIES,
            &mut retry_counts,
        );
        if outcome.forwarded > 0 || outcome.retried > 0 || outcome.dead_lettered > 0 {
            println!(
                "sweep: forwarded={} retried={} dead_lettered={} not_ready={}",
                outcome.forwarded, outcome.retried, outcome.dead_lettered, outcome.not_ready
            );
        }

        if !daemon {
            return 0;
        }
        std::thread::sleep(poll_interval);
    }
}

fn now_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // A minimal, dependency-free timestamp (seconds resolution is plenty
    // for a human-readable "started at" field; not full RFC 3339 to avoid
    // pulling in a date/time crate for this alone).
    format!("unix:{}", now.as_secs())
}
