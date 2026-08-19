//! Docker Compose invocation helpers.
//!
//! Deliberately thin wrappers around shelling out to `docker compose`,
//! matching the same "shell out to a stable external CLI and parse
//! structured output" pattern `envoy-core::executor` uses for wrapped
//! application commands.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    command
        .status()
        .map(|_| ())
        .map_err(|error| error.to_string())
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
}
