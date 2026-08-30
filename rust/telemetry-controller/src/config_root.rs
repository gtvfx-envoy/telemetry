//! Platform-specific Envoy config root resolution.
//!
//! Mirrors `envoy-core::user_config::config_root()`'s convention exactly
//! (this crate intentionally does not depend on `envoy-core` as a library,
//! matching how `engit` is fully independent from `envoy-cli` today) so the
//! controller's own state lives in the same place operators already know:
//! `~/.envoy`, overridable via `ENVOY_CONFIG_ROOT`.

use std::env;
use std::path::PathBuf;

const CONFIG_ROOT_VAR: &str = "ENVOY_CONFIG_ROOT";

/// Envoy's effective shared config root: `ENVOY_CONFIG_ROOT` if set and
/// non-empty, otherwise the platform default `~/.envoy`.
pub fn config_root() -> PathBuf {
    if let Ok(value) = env::var(CONFIG_ROOT_VAR) {
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    default_config_root()
}

#[cfg(target_os = "windows")]
fn default_config_root() -> PathBuf {
    let home = env::var("USERPROFILE")
        .or_else(|_| {
            let drive = env::var("HOMEDRIVE")?;
            let path = env::var("HOMEPATH")?;
            Ok::<_, env::VarError>(drive + &path)
        })
        .or_else(|_| env::var("HOME"))
        .unwrap_or_else(|_| String::from("."));
    PathBuf::from(home).join(".envoy")
}

#[cfg(not(target_os = "windows"))]
fn default_config_root() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| String::from("."));
    PathBuf::from(home).join(".envoy")
}

/// Directory under the config root holding this controller's own state:
/// generated credentials, PID files, and installation metadata. Deliberately
/// distinct from the client-side `telemetry/` spool/installation-id
/// directory every workstation's `envoy` binary uses, and never inside the
/// published bundle.
pub fn server_state_dir() -> PathBuf {
    config_root().join("telemetry-server")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = env::var_os(key);
            env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = env::var_os(key);
            env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }

    // These tests mutate the real process environment, so they must not
    // run concurrently with each other.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn config_root_honors_override() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let _guard = EnvVarGuard::set(CONFIG_ROOT_VAR, "custom-root");
        assert_eq!(config_root(), PathBuf::from("custom-root"));
    }

    #[test]
    fn config_root_ignores_empty_override() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let _guard = EnvVarGuard::set(CONFIG_ROOT_VAR, "");
        assert_eq!(config_root(), default_config_root());
    }

    #[test]
    fn server_state_dir_is_nested_under_config_root() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let _guard = EnvVarGuard::set(CONFIG_ROOT_VAR, "custom-root");
        assert_eq!(
            server_state_dir(),
            PathBuf::from("custom-root").join("telemetry-server")
        );
    }

    #[test]
    fn config_root_falls_back_to_default_when_unset() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let _guard = EnvVarGuard::remove(CONFIG_ROOT_VAR);
        assert_eq!(config_root(), default_config_root());
    }
}
