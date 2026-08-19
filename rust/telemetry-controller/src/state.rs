//! Persistent controller state: process/runtime tracking, generated
//! credentials, and the active retention setting -- always under the
//! platform config root (see [`crate::config_root`]), never inside the
//! published bundle.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config_root::server_state_dir;

const STATE_FILE_NAME: &str = "state.json";

/// Which runtime the controller is currently (or was last) managing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    Compose,
    Native,
}

impl RuntimeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuntimeKind::Compose => "compose",
            RuntimeKind::Native => "native",
        }
    }
}

/// Controller state persisted as JSON under [`crate::config_root::server_state_dir`].
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ServerState {
    pub runtime: Option<RuntimeKind>,
    /// PIDs of natively-launched processes, keyed by service name
    /// (`collector`, `tempo`, `grafana`, `sweep`). Empty under Compose,
    /// since Docker tracks container liveness itself.
    #[serde(default)]
    pub native_pids: HashMap<String, u32>,
    pub started_at: Option<String>,
    /// Grafana's generated break-glass admin password (operator-only).
    pub grafana_admin_password: Option<String>,
    /// Grafana's generated shared "studio-viewer" password -- the
    /// documented default for studios without SSO/LDAP.
    pub grafana_viewer_password: Option<String>,
    /// Shared bearer token the Collector's OTLP receiver requires on every
    /// request.
    pub collector_bearer_token: Option<String>,
    pub retention_days: Option<u32>,
}

pub fn state_path() -> PathBuf {
    server_state_dir().join(STATE_FILE_NAME)
}

/// Load persisted state, or a fresh default if none exists yet or the file
/// is unreadable/corrupt (never fails the caller).
pub fn load_state() -> ServerState {
    fs::read_to_string(state_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_state(state: &ServerState) -> std::io::Result<()> {
    let dir = server_state_dir();
    fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(state)
        .map_err(|source| std::io::Error::new(std::io::ErrorKind::InvalidData, source))?;
    fs::write(state_path(), json)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use tempfile::tempdir;

    use super::*;

    struct EnvVarGuard {
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(value: &std::path::Path) -> Self {
            let previous = std::env::var_os("ENVOY_CONFIG_ROOT");
            std::env::set_var("ENVOY_CONFIG_ROOT", value);
            Self { previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var("ENVOY_CONFIG_ROOT", value),
                None => std::env::remove_var("ENVOY_CONFIG_ROOT"),
            }
        }
    }

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn load_state_returns_default_when_nothing_persisted() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let temp_dir = tempdir().expect("tempdir should be created");
        let _guard = EnvVarGuard::set(temp_dir.path());

        assert_eq!(load_state(), ServerState::default());
    }

    #[test]
    fn save_then_load_round_trips_state() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let temp_dir = tempdir().expect("tempdir should be created");
        let _guard = EnvVarGuard::set(temp_dir.path());

        let mut state = ServerState {
            runtime: Some(RuntimeKind::Compose),
            retention_days: Some(30),
            ..Default::default()
        };
        state.native_pids.insert("collector".to_string(), 1234);
        save_state(&state).expect("state should save");

        assert_eq!(load_state(), state);
    }

    #[test]
    fn load_state_recovers_from_corrupt_file() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let temp_dir = tempdir().expect("tempdir should be created");
        let _guard = EnvVarGuard::set(temp_dir.path());

        fs::create_dir_all(server_state_dir()).expect("dir should be created");
        fs::write(state_path(), b"not valid json").expect("file should be written");

        assert_eq!(load_state(), ServerState::default());
    }
}
