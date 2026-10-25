//! Docker Compose invocation helpers.
//!
//! Deliberately thin wrappers around shelling out to `docker compose`,
//! matching the same "shell out to a stable external CLI and parse
//! structured output" pattern `envoy-core::executor` uses for wrapped
//! application commands.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

const REQUIRED_SERVICES: [&str; 4] = ["collector", "tempo", "grafana", "sweep"];

/// Return whether Docker already has persistent volumes for this Compose
/// project.
///
/// A missing controller state file combined with retained Grafana data is not
/// a fresh installation: Grafana keeps the old admin password in its database
/// and ignores a newly generated `GF_SECURITY_ADMIN_PASSWORD`. Detect that
/// situation before startup so the operator can restore state instead of
/// receiving credentials that cannot unlock the retained deployment.
pub fn persistent_volumes_exist(compose_file: &Path) -> Result<bool, String> {
    let project_name = compose_project_name(compose_file)?;
    let label_filter = format!("label=com.docker.compose.project={project_name}");
    let output = Command::new("docker")
        .args(["volume", "ls", "--quiet", "--filter", &label_filter])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

fn compose_project_name(compose_file: &Path) -> Result<String, String> {
    if let Ok(project_name) = std::env::var("COMPOSE_PROJECT_NAME") {
        if !project_name.is_empty() {
            return Ok(project_name);
        }
    }
    default_compose_project_name(compose_file)
}

fn default_compose_project_name(compose_file: &Path) -> Result<String, String> {
    compose_file
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(str::to_lowercase)
        .ok_or_else(|| {
            format!(
                "could not derive a Docker Compose project name from {}",
                compose_file.display()
            )
        })
}

/// Locate the bundle's `docker-compose.yml`, searching upward from the
/// running controller binary's own location (the bundle ships the compose
/// file alongside the controller binaries).
pub fn default_compose_file() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    find_compose_file_from(&exe)
}

fn find_compose_file_from(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors().take(6) {
        let nested = ancestor.join("compose").join("docker-compose.yml");
        if nested.is_file() {
            return Some(nested);
        }
        let sibling = ancestor.join("docker-compose.yml");
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    None
}

fn compose_command(compose_file: &Path, env: &[(&str, String)]) -> Command {
    let mut command = Command::new("docker");
    command.arg("compose").arg("-f").arg(compose_file);
    // Every `docker compose` subcommand (not just `up`) interpolates the
    // compose file's `environment:` blocks up front, including the
    // required (`:?`) bearer token and Grafana password references -- so
    // `down`, `ps`, and `logs` all fail with "variable ... is missing a
    // value" unless the same env vars are supplied, even though those
    // subcommands don't logically need the values themselves. Apply `env`
    // unconditionally here so every caller gets this for free.
    for (key, value) in env {
        command.env(key, value);
    }
    command
}

/// `docker compose up -d --build`, applying `env` on top of the current
/// process environment for values the compose file interpolates (bearer
/// token, generated Grafana passwords, retention).
///
/// `--build` is load-bearing, not cosmetic: `docker compose up` alone only
/// builds a `build:`-context service's image the *first* time it's ever
/// missing -- it silently reuses a stale cached image on every subsequent
/// `up`, even after the build context's contents (e.g. a newly-published
/// `telemetry-controller` binary the sweep image `COPY`s in) have changed.
/// Confirmed directly: without `--build`, `start` kept running the sweep
/// container against an old binary after a rebuild, with no error --
/// exactly the kind of silent staleness this bundle's version-skew policy
/// (see envoy-core::telemetry::schema) is meant to avoid. Docker's layer
/// cache keeps the common no-change case fast.
pub fn up(compose_file: &Path, env: &[(&str, String)]) -> Result<(), String> {
    let mut command = compose_command(compose_file, env);
    command.args(["up", "-d", "--build"]);
    run_checked(command)
}

/// `docker compose down`, deliberately without `-v`: stopping the server
/// must always preserve telemetry data. `env` must be supplied (see
/// [`compose_command`]) even though `down` doesn't use the values itself.
pub fn down(compose_file: &Path, env: &[(&str, String)]) -> Result<(), String> {
    let mut command = compose_command(compose_file, env);
    command.arg("down");
    run_checked(command)
}

/// `docker compose ps --format json`, returning the raw (newline-delimited
/// JSON) stdout for the caller to parse.
pub fn ps_json(compose_file: &Path, env: &[(&str, String)]) -> Result<String, String> {
    let mut command = compose_command(compose_file, env);
    command
        .args(["ps", "--format", "json"])
        .stdout(Stdio::piped());
    let output = command.output().map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Return whether every required Compose service is running and, when a
/// health status is present, healthy.
pub fn required_services_running(
    compose_file: &Path,
    env: &[(&str, String)],
) -> Result<bool, String> {
    let output = ps_json(compose_file, env)?;
    required_services_running_from_json(&output)
}

fn required_services_running_from_json(output: &str) -> Result<bool, String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Ok(false);
    }

    let records = if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<Value>>(trimmed)
            .map_err(|error| format!("failed to parse Docker Compose status: {error}"))?
    } else {
        trimmed
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str::<Value>(line)
                    .map_err(|error| format!("failed to parse Docker Compose status: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    Ok(REQUIRED_SERVICES.iter().all(|required_service| {
        records.iter().any(|record| {
            let service = json_string(record, "Service", "service");
            let state = json_string(record, "State", "state");
            let health = json_string(record, "Health", "health");
            service == Some(*required_service)
                && state.is_some_and(|value| value.eq_ignore_ascii_case("running"))
                && health
                    .is_none_or(|value| value.is_empty() || value.eq_ignore_ascii_case("healthy"))
        })
    }))
}

fn json_string<'a>(record: &'a Value, primary_key: &str, fallback_key: &str) -> Option<&'a str> {
    record
        .get(primary_key)
        .or_else(|| record.get(fallback_key))
        .and_then(Value::as_str)
}

/// `docker compose logs [SERVICE]`, streamed directly to this process's own
/// stdout/stderr.
pub fn logs(
    compose_file: &Path,
    service: Option<&str>,
    env: &[(&str, String)],
) -> Result<(), String> {
    let mut command = compose_command(compose_file, env);
    command.arg("logs").arg("--no-color").arg("--tail=200");
    if let Some(service) = service {
        command.arg(service);
    }
    let status = command.status().map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("command exited with status {status}"))
    }
}

fn run_checked(mut command: Command) -> Result<(), String> {
    let status = command.status().map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("command exited with status {status}"))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn finds_compose_file_directly_alongside_the_binary() {
        let temp_dir = tempdir().expect("tempdir should be created");
        let fake_exe = temp_dir.path().join("bin").join("telemetry-controller");
        fs::create_dir_all(fake_exe.parent().unwrap()).expect("dir should be created");
        fs::write(
            fake_exe.parent().unwrap().join("docker-compose.yml"),
            "services: {}",
        )
        .expect("compose file should be written");

        let found = find_compose_file_from(&fake_exe).expect("should find compose file");
        assert_eq!(found, fake_exe.parent().unwrap().join("docker-compose.yml"));
    }

    #[test]
    fn finds_compose_file_under_a_nested_compose_directory() {
        let temp_dir = tempdir().expect("tempdir should be created");
        let fake_exe = temp_dir.path().join("bin").join("telemetry-controller");
        fs::create_dir_all(fake_exe.parent().unwrap().join("compose"))
            .expect("dir should be created");
        fs::write(
            fake_exe
                .parent()
                .unwrap()
                .join("compose")
                .join("docker-compose.yml"),
            "services: {}",
        )
        .expect("compose file should be written");

        let found = find_compose_file_from(&fake_exe).expect("should find compose file");
        assert_eq!(
            found,
            fake_exe
                .parent()
                .unwrap()
                .join("compose")
                .join("docker-compose.yml")
        );
    }

    #[test]
    fn returns_none_when_no_compose_file_exists_nearby() {
        let temp_dir = tempdir().expect("tempdir should be created");
        let fake_exe = temp_dir.path().join("bin").join("telemetry-controller");
        fs::create_dir_all(fake_exe.parent().unwrap()).expect("dir should be created");

        assert!(find_compose_file_from(&fake_exe).is_none());
    }

    #[test]
    fn derives_default_project_name_from_compose_directory() {
        let compose_file = Path::new("/srv/Telemetry/compose/docker-compose.yml");
        assert_eq!(
            default_compose_project_name(compose_file),
            Ok("compose".to_string())
        );
    }

    #[test]
    fn status_requires_every_service_to_be_running() {
        let output = r#"[
            {"Service":"collector","State":"running"},
            {"Service":"tempo","State":"running"},
            {"Service":"grafana","State":"running"},
            {"Service":"sweep","State":"running"}
        ]"#;

        assert_eq!(required_services_running_from_json(output), Ok(true));
    }

    #[test]
    fn status_rejects_missing_or_stopped_services() {
        let missing = r#"[
            {"Service":"collector","State":"running"},
            {"Service":"tempo","State":"running"},
            {"Service":"grafana","State":"running"}
        ]"#;
        let stopped = r#"[
            {"Service":"collector","State":"running"},
            {"Service":"tempo","State":"exited"},
            {"Service":"grafana","State":"running"},
            {"Service":"sweep","State":"running"}
        ]"#;

        assert_eq!(required_services_running_from_json(missing), Ok(false));
        assert_eq!(required_services_running_from_json(stopped), Ok(false));
    }

    #[test]
    fn status_supports_newline_delimited_json_and_health() {
        let healthy = concat!(
            "{\"service\":\"collector\",\"state\":\"running\"}\n",
            "{\"service\":\"tempo\",\"state\":\"running\"}\n",
            "{\"service\":\"grafana\",\"state\":\"running\",\"health\":\"healthy\"}\n",
            "{\"service\":\"sweep\",\"state\":\"running\"}\n"
        );
        let unhealthy = healthy.replace("\"healthy\"", "\"unhealthy\"");

        assert_eq!(required_services_running_from_json(healthy), Ok(true));
        assert_eq!(required_services_running_from_json(&unhealthy), Ok(false));
    }
}
