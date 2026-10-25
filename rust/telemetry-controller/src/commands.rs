//! Command handlers for `telemetry-controller`'s subcommands.

use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::compose;
use crate::config_root::{tempo_config_dir, tempo_config_path};
use crate::grafana;
use crate::ports;
use crate::runtime::{resolve_runtime, RuntimeChoice};
use crate::state::{load_state, save_state, RuntimeKind, ServerState};
use crate::sweep;
use crate::tempo_config;

const GRAFANA_URL: &str = "http://localhost:3000";

/// `telemetry-controller start [--runtime auto|compose]`.
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

    let Some(mut state) = load_state_or_report() else {
        return 1;
    };

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

    if state.grafana_admin_password.is_none() {
        let Some(compose_file) = compose::default_compose_file() else {
            eprintln!("Error: could not locate docker-compose.yml alongside this binary.");
            return 1;
        };
        match compose::persistent_volumes_exist(&compose_file) {
            Ok(true) => {
                eprintln!(
                    "Error: persistent telemetry volumes already exist, but the controller has no stored Grafana admin credential."
                );
                eprintln!(
                    "Restore ~/.envoy/telemetry-server/state.json (or the equivalent ENVOY_CONFIG_ROOT path) before starting."
                );
                eprintln!(
                    "Do not delete the existing Docker volumes unless their Tempo and Grafana data is intentionally being discarded."
                );
                return 1;
            }
            Ok(false) => {}
            Err(message) => {
                eprintln!("Error: failed to inspect existing Compose volumes: {message}");
                return 1;
            }
        }
    }

    let credentials_generated = ensure_generated_credentials(&mut state);
    state.runtime = Some(runtime);
    state.started_at = Some(now_timestamp());
    if let Err(error) = save_state(&state) {
        eprintln!("Error: failed to persist controller state before startup: {error}");
        return 1;
    }

    if let Err(message) = start_compose(&state) {
        eprintln!("Error: {message}");
        eprintln!(
            "Controller state was retained so `telemetry-controller stop` can clean up any partial startup."
        );
        return 1;
    }

    println!("Started ({}).", runtime.as_str());
    println!("Grafana: {GRAFANA_URL}");
    if credentials_generated {
        println!(
            "Generated credentials (store securely; recover with `status --show-credentials`):"
        );
        print_credentials(&state);
    }
    0
}

fn load_state_or_report() -> Option<ServerState> {
    match load_state() {
        Ok(state) => Some(state),
        Err(error) => {
            eprintln!("Error: {error}");
            None
        }
    }
}

fn ensure_generated_credentials(state: &mut ServerState) -> bool {
    let mut generated = false;
    if state.grafana_admin_password.is_none() {
        state.grafana_admin_password = Some(grafana::generate_password());
        generated = true;
    }
    if state.grafana_viewer_password.is_none() {
        state.grafana_viewer_password = Some(grafana::generate_password());
        generated = true;
    }
    if state.collector_bearer_token.is_none() {
        state.collector_bearer_token = Some(grafana::generate_password());
        generated = true;
    }
    if state.retention_days.is_none() {
        state.retention_days = Some(30);
    }
    generated
}

fn print_credentials(state: &ServerState) {
    if let Some(password) = &state.grafana_admin_password {
        println!("  Grafana admin password: {password}");
    }
    if let Some(password) = &state.grafana_viewer_password {
        println!("  Grafana studio-viewer password: {password}");
    }
    if let Some(token) = &state.collector_bearer_token {
        println!("  Collector bearer token: {token}");
    }
}

/// Build the env-var set every `docker compose` invocation must supply so
/// the compose file's required (`:?`) interpolations succeed.
///
/// This matters for *every* subcommand, not just `up`: `docker compose`
/// interpolates the whole compose file (including `environment:` blocks)
/// before running any subcommand, so `down`, `ps`, and `logs` all fail with
/// "variable ... is missing a value" unless these are supplied too, even
/// though those subcommands don't use the values themselves.
fn compose_env_from_state(state: &ServerState) -> [(&'static str, String); 4] {
    let config_directory = tempo_config_dir();
    let absolute_config_directory = if config_directory.is_absolute() {
        config_directory
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(&config_directory))
            .unwrap_or(config_directory)
    };
    [
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
            "TEMPO_CONFIG_DIR",
            absolute_config_directory.to_string_lossy().into_owned(),
        ),
    ]
}

fn start_compose(state: &ServerState) -> Result<(), String> {
    let compose_file = compose::default_compose_file()
        .ok_or_else(|| "could not locate docker-compose.yml alongside this binary".to_string())?;
    let template_path = compose_file
        .parent()
        .map(|directory| directory.join("tempo").join("tempo.yaml"))
        .ok_or_else(|| "Compose file has no parent directory".to_string())?;
    tempo_config::prepare_runtime_config(
        &template_path,
        &tempo_config_path(),
        state.retention_days.unwrap_or(30),
    )?;
    let env = compose_env_from_state(state);
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

/// Idempotency check: is the runtime this state describes actually still
/// running, or is this a stale record (e.g. a prior hard crash)?
fn is_already_running(state: &ServerState) -> bool {
    match state.runtime {
        Some(RuntimeKind::Compose) => compose::default_compose_file()
            .map(|compose_file| {
                let env = compose_env_from_state(state);
                compose::required_services_running(&compose_file, &env).unwrap_or(false)
            })
            .unwrap_or(false),
        None => false,
    }
}

/// `telemetry-controller stop`. Preserves telemetry data.
pub fn stop() -> i32 {
    let Some(mut state) = load_state_or_report() else {
        return 1;
    };

    match state.runtime {
        Some(RuntimeKind::Compose) => {
            let Some(compose_file) = compose::default_compose_file() else {
                eprintln!("Error: could not locate docker-compose.yml alongside this binary.");
                return 1;
            };
            if let Err(message) = compose::down(&compose_file, &compose_env_from_state(&state)) {
                eprintln!("Error stopping Compose stack: {message}");
                return 1;
            }
        }
        None => {
            println!("Not running.");
            return 0;
        }
    }

    state.runtime = None;
    state.started_at = None;
    if let Err(error) = save_state(&state) {
        eprintln!("Error: failed to persist stopped controller state: {error}");
        return 1;
    }
    println!("Stopped. Telemetry data was preserved.");
    0
}

/// `telemetry-controller status [--show-credentials]`.
pub fn status(show_credentials: bool) -> i32 {
    let Some(state) = load_state_or_report() else {
        return 1;
    };
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
    if show_credentials {
        println!("Credentials:");
        print_credentials(&state);
    } else {
        println!("Credentials: hidden (use `status --show-credentials` to display)");
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

/// Prefer the mutable runtime Tempo config when it can be read; fall back to
/// controller state and finally the 30-day default.
fn live_retention_days(state: &ServerState) -> u32 {
    let live_value = tempo_config::get_retention_days(&tempo_config_path());
    live_value.or(state.retention_days).unwrap_or(30)
}

/// `telemetry-controller logs [SERVICE]`.
pub fn logs(service: Option<&str>) -> i32 {
    let Some(state) = load_state_or_report() else {
        return 1;
    };
    match state.runtime {
        Some(RuntimeKind::Compose) => {
            let Some(compose_file) = compose::default_compose_file() else {
                eprintln!("Error: could not locate docker-compose.yml alongside this binary.");
                return 1;
            };
            match compose::logs(&compose_file, service, &compose_env_from_state(&state)) {
                Ok(()) => 0,
                Err(message) => {
                    eprintln!("Error: {message}");
                    1
                }
            }
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
    if retention_days.checked_mul(24).is_none() {
        eprintln!(
            "Error: retention_days={retention_days} is too large (overflows hours); choose a smaller value."
        );
        return 1;
    }

    let Some(mut state) = load_state_or_report() else {
        return 1;
    };
    state.retention_days = Some(retention_days);

    if let Err(error) = save_state(&state) {
        eprintln!("Error: failed to persist retention setting: {error}");
        return 1;
    }

    let config_path = tempo_config_path();
    if config_path.is_file() {
        if let Err(message) = tempo_config::set_retention_days(&config_path, retention_days) {
            eprintln!("Error updating Tempo config: {message}");
            return 1;
        }
        println!(
            "Updated {} to {retention_days} days.",
            config_path.display()
        );
        if is_already_running(&state) {
            println!("Restart the server (`telemetry-controller stop` then `start`) for Tempo to pick up the new retention.");
        }
    } else {
        println!(
            "Recorded retention_days={retention_days} in controller state; Tempo config file \
not found yet (will apply on the next `start`)."
        );
    }

    0
}

/// `telemetry-controller sweep --drop-dir DIR --collector-endpoint URL
/// [--bearer-token TOKEN] [--daemon]`.
///
/// `bearer_token_override` (from the `--bearer-token` flag / its
/// `COLLECTOR_BEARER_TOKEN` env var) takes precedence over the token in
/// `telemetry-controller`'s own state file. The override is what the
/// sweep's own compose service actually uses in practice -- the sweep
/// normally runs inside its own container, which has no access to the
/// host's config root/state file at all, so falling back to `load_state()`
/// alone would silently resolve to "no token" and reject every forward
/// attempt as unauthenticated.
pub fn sweep_command(
    drop_dir: &std::path::Path,
    collector_endpoint: &str,
    bearer_token_override: Option<&str>,
    daemon: bool,
    poll_interval: Duration,
) -> i32 {
    let Some(state) = load_state_or_report() else {
        return 1;
    };
    let bearer_token = resolve_sweep_bearer_token(bearer_token_override, &state);
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

/// Resolve the bearer token the sweep should present to the Collector: the
/// explicit `--bearer-token`/`COLLECTOR_BEARER_TOKEN` override always wins
/// when present, falling back to the controller's own state file only when
/// the command is invoked directly on the controller host.
fn resolve_sweep_bearer_token(override_value: Option<&str>, state: &ServerState) -> Option<String> {
    override_value
        .map(str::to_string)
        .or_else(|| state.collector_bearer_token.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_override_wins_even_when_state_has_a_token() {
        let state = ServerState {
            collector_bearer_token: Some("state-token".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_sweep_bearer_token(Some("override-token"), &state),
            Some("override-token".to_string())
        );
    }

    #[test]
    fn falls_back_to_state_token_when_no_override_given() {
        let state = ServerState {
            collector_bearer_token: Some("state-token".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_sweep_bearer_token(None, &state),
            Some("state-token".to_string())
        );
    }

    #[test]
    fn returns_none_when_neither_override_nor_state_has_a_token() {
        let state = ServerState::default();
        assert_eq!(resolve_sweep_bearer_token(None, &state), None);
    }
}
