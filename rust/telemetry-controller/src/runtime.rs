//! Runtime selection: prefer Docker Compose when available, otherwise
//! native (Linux x86-64 only in v1). Windows and macOS report a clear
//! Docker requirement when neither Compose is available nor native mode
//! applies.

use std::process::Command;
use std::str::FromStr;

use crate::state::RuntimeKind;

/// The `--runtime` flag's value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeChoice {
    Auto,
    Compose,
    Native,
}

impl FromStr for RuntimeChoice {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Ok(RuntimeChoice::Auto),
            "compose" => Ok(RuntimeChoice::Compose),
            "native" => Ok(RuntimeChoice::Native),
            other => Err(format!(
                "unknown runtime '{other}' (expected auto, compose, or native)"
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
    resolve_runtime_with(
        choice,
        docker_compose_available(),
        cfg!(target_os = "linux"),
        cfg!(target_arch = "x86_64"),
    )
}

/// The actual decision logic, parameterized over environment facts so it
/// can be exercised in tests without needing a real Docker installation.
fn resolve_runtime_with(
    choice: RuntimeChoice,
    docker_available: bool,
    is_linux: bool,
    is_x86_64: bool,
) -> Result<RuntimeKind, String> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    // v1 native support is Linux x86-64 only (see module docs); OS alone
    // isn't sufficient -- e.g. Linux aarch64 must not be treated as
    // supported here.
    let native_supported = is_linux && is_x86_64;
    match choice {
        RuntimeChoice::Compose => {
            if docker_available {
                Ok(RuntimeKind::Compose)
            } else {
                Err(
                    "Docker Compose was requested (--runtime compose) but `docker compose \
version` failed. Install Docker Desktop (Windows/macOS) or Docker Engine plus the compose \
plugin (Linux), then retry."
                        .to_string(),
                )
            }
        }
        RuntimeChoice::Native => {
            if native_supported {
                Ok(RuntimeKind::Native)
            } else {
                Err(format!(
                    "Native runtime is only supported on Linux x86-64 in this release; this \
machine is {os}/{arch}. Use --runtime compose (or --runtime auto) instead."
                ))
            }
        }
        RuntimeChoice::Auto => {
            if docker_available {
                Ok(RuntimeKind::Compose)
            } else if native_supported {
                Ok(RuntimeKind::Native)
            } else {
                Err(format!(
                    "No usable runtime found: Docker Compose is unavailable (`docker compose \
version` failed) and native mode is Linux x86-64-only. This machine is {os}/{arch}. Install \
Docker Desktop to continue."
                ))
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
        assert_eq!("native".parse::<RuntimeChoice>(), Ok(RuntimeChoice::Native));
        assert!("bogus".parse::<RuntimeChoice>().is_err());
    }

    #[test]
    fn auto_prefers_compose_when_docker_is_available() {
        assert_eq!(
            resolve_runtime_with(RuntimeChoice::Auto, true, true, true),
            Ok(RuntimeKind::Compose)
        );
        assert_eq!(
            resolve_runtime_with(RuntimeChoice::Auto, true, false, true),
            Ok(RuntimeKind::Compose)
        );
    }

    #[test]
    fn auto_falls_back_to_native_on_linux_x86_64_without_docker() {
        assert_eq!(
            resolve_runtime_with(RuntimeChoice::Auto, false, true, true),
            Ok(RuntimeKind::Native)
        );
    }

    #[test]
    fn auto_does_not_pick_native_on_linux_non_x86_64() {
        // e.g. Linux aarch64: is_linux is true, but native mode is v1
        // x86-64-only, so falling back to it here would fail later from
        // missing artifacts instead of a clear up-front error.
        assert!(resolve_runtime_with(RuntimeChoice::Auto, false, true, false).is_err());
    }

    #[test]
    fn auto_fails_on_non_linux_without_docker() {
        assert!(resolve_runtime_with(RuntimeChoice::Auto, false, false, true).is_err());
    }

    #[test]
    fn explicit_compose_fails_without_docker() {
        assert!(resolve_runtime_with(RuntimeChoice::Compose, false, true, true).is_err());
    }

    #[test]
    fn explicit_native_fails_on_non_linux() {
        assert!(resolve_runtime_with(RuntimeChoice::Native, true, false, true).is_err());
    }

    #[test]
    fn explicit_native_fails_on_linux_non_x86_64() {
        assert!(resolve_runtime_with(RuntimeChoice::Native, false, true, false).is_err());
    }

    #[test]
    fn explicit_native_succeeds_on_linux_x86_64_regardless_of_docker() {
        assert_eq!(
            resolve_runtime_with(RuntimeChoice::Native, false, true, true),
            Ok(RuntimeKind::Native)
        );
        assert_eq!(
            resolve_runtime_with(RuntimeChoice::Native, true, true, true),
            Ok(RuntimeKind::Native)
        );
    }
}
