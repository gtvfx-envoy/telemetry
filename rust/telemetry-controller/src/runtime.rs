//! Docker Compose runtime selection.
//!
//! v0.1 supports Compose on every platform. The previous native prototype
//! was removed from the release surface until it can provide equivalent
//! authentication, persistence, logging, and integration-test coverage.

use std::process::Command;
use std::str::FromStr;

use crate::state::RuntimeKind;

/// The `--runtime` flag's value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeChoice {
    Auto,
    Compose,
}

impl FromStr for RuntimeChoice {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Ok(RuntimeChoice::Auto),
            "compose" => Ok(RuntimeChoice::Compose),
            other => Err(format!(
                "unknown runtime '{other}' (expected auto or compose)"
            )),
        }
    }
}

/// Return `true` if `docker compose version` succeeds on this machine.
pub fn docker_compose_available() -> bool {
    Command::new("docker")
        .args(["compose", "version"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Resolve `choice` into the concrete runtime to use, given the real
/// environment.
pub fn resolve_runtime(choice: RuntimeChoice) -> Result<RuntimeKind, String> {
    resolve_runtime_with(choice, docker_compose_available())
}

/// The actual decision logic, parameterized over environment facts so it
/// can be exercised in tests without needing a real Docker installation.
fn resolve_runtime_with(
    choice: RuntimeChoice,
    docker_available: bool,
) -> Result<RuntimeKind, String> {
    match choice {
        RuntimeChoice::Auto | RuntimeChoice::Compose => {
            if docker_available {
                Ok(RuntimeKind::Compose)
            } else {
                Err(
                    "Docker Compose is required but `docker compose version` failed. Install \
Docker Desktop (Windows/macOS) or Docker Engine plus the Compose plugin (Linux), then retry."
                        .to_string(),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_runtime_choice_case_insensitively() {
        assert_eq!("Auto".parse::<RuntimeChoice>(), Ok(RuntimeChoice::Auto));
        assert_eq!(
            "COMPOSE".parse::<RuntimeChoice>(),
            Ok(RuntimeChoice::Compose)
        );
        assert!("native".parse::<RuntimeChoice>().is_err());
        assert!("bogus".parse::<RuntimeChoice>().is_err());
    }

    #[test]
    fn auto_prefers_compose_when_docker_is_available() {
        assert_eq!(
            resolve_runtime_with(RuntimeChoice::Auto, true),
            Ok(RuntimeKind::Compose)
        );
        assert_eq!(
            resolve_runtime_with(RuntimeChoice::Compose, true),
            Ok(RuntimeKind::Compose)
        );
    }

    #[test]
    fn auto_fails_without_docker() {
        assert!(resolve_runtime_with(RuntimeChoice::Auto, false).is_err());
    }

    #[test]
    fn explicit_compose_fails_without_docker() {
        assert!(resolve_runtime_with(RuntimeChoice::Compose, false).is_err());
    }
}
