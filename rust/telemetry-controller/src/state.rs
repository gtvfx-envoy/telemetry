//! Persistent controller state: process/runtime tracking, generated
//! credentials, and the active retention setting -- always under the
//! platform config root (see [`crate::config_root`]), never inside the
//! published bundle.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config_root::server_state_dir;

const STATE_FILE_NAME: &str = "state.json";

/// Which runtime the controller is currently (or was last) managing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    Compose,
}

impl RuntimeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuntimeKind::Compose => "compose",
        }
    }
}

/// Controller state persisted as JSON under [`crate::config_root::server_state_dir`].
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ServerState {
    pub runtime: Option<RuntimeKind>,
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

/// Load persisted state, returning a fresh default only when no state file
/// exists yet. Unreadable or corrupt state is an error so a lifecycle command
/// cannot silently replace stored credentials.
pub fn load_state() -> Result<ServerState, String> {
    let path = state_path();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(ServerState::default());
        }
        Err(source) => {
            return Err(format!(
                "failed to read controller state at {}: {source}",
                path.display()
            ));
        }
    };
    serde_json::from_str(&text).map_err(|source| {
        format!(
            "controller state at {} is corrupt; refusing to replace stored credentials: {source}",
            path.display()
        )
    })
}

/// Atomically persist controller state with owner-only Unix permissions.
///
/// Windows files inherit the ACL of the per-user Envoy configuration
/// directory. Unix permissions are asserted explicitly for both the state
/// directory and file rather than relying on the process umask.
pub fn save_state(state: &ServerState) -> io::Result<()> {
    let dir = server_state_dir();
    fs::create_dir_all(&dir)?;
    set_private_directory_permissions(&dir)?;

    let mut json = serde_json::to_vec_pretty(state)
        .map_err(|source| io::Error::new(io::ErrorKind::InvalidData, source))?;
    json.push(b'\n');

    let path = state_path();
    let temp_path = dir.join(format!(
        "{STATE_FILE_NAME}.tmp-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>()
    ));

    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let mut temp_file = options.open(&temp_path)?;
        temp_file.write_all(&json)?;
        temp_file.sync_all()?;
        drop(temp_file);

        replace_file(&temp_path, &path)?;
        sync_parent_directory(&dir)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &std::path::Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn replace_file(source: &std::path::Path, destination: &std::path::Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &std::path::Path, destination: &std::path::Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "Kernel32")]
    extern "system" {
        #[link_name = "MoveFileExW"]
        fn move_file_ex_w(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();

    // SAFETY: both paths are encoded as owned, NUL-terminated UTF-16 buffers
    // that remain alive for the duration of the Win32 call.
    let moved = unsafe {
        move_file_ex_w(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_parent_directory(path: &std::path::Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &std::path::Path) -> io::Result<()> {
    Ok(())
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

        assert_eq!(
            load_state().expect("missing state should load"),
            ServerState::default()
        );
    }

    #[test]
    fn save_then_load_round_trips_state() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let temp_dir = tempdir().expect("tempdir should be created");
        let _guard = EnvVarGuard::set(temp_dir.path());

        let state = ServerState {
            runtime: Some(RuntimeKind::Compose),
            retention_days: Some(30),
            ..Default::default()
        };
        save_state(&state).expect("state should save");

        assert_eq!(load_state().expect("saved state should load"), state);
    }

    #[test]
    fn load_state_rejects_corrupt_file() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let temp_dir = tempdir().expect("tempdir should be created");
        let _guard = EnvVarGuard::set(temp_dir.path());

        fs::create_dir_all(server_state_dir()).expect("dir should be created");
        fs::write(state_path(), b"not valid json").expect("file should be written");

        let error = load_state().expect_err("corrupt state must not be replaced");
        assert!(error
            .to_string()
            .contains("refusing to replace stored credentials"));
    }

    #[test]
    fn save_state_atomically_replaces_existing_state() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let temp_dir = tempdir().expect("tempdir should be created");
        let _guard = EnvVarGuard::set(temp_dir.path());

        let mut state = ServerState {
            retention_days: Some(30),
            ..Default::default()
        };
        save_state(&state).expect("initial state should save");
        state.retention_days = Some(14);
        save_state(&state).expect("replacement state should save");

        assert_eq!(
            load_state()
                .expect("replacement state should load")
                .retention_days,
            Some(14)
        );
        let entries = fs::read_dir(server_state_dir())
            .expect("state directory should be readable")
            .collect::<Result<Vec<_>, _>>()
            .expect("directory entries should be readable");
        assert_eq!(entries.len(), 1, "temporary state files should be removed");
    }

    #[cfg(unix)]
    #[test]
    fn save_state_uses_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let temp_dir = tempdir().expect("tempdir should be created");
        let _guard = EnvVarGuard::set(temp_dir.path());

        save_state(&ServerState::default()).expect("state should save");

        let file_mode = fs::metadata(state_path())
            .expect("state metadata should be available")
            .permissions()
            .mode()
            & 0o777;
        let directory_mode = fs::metadata(server_state_dir())
            .expect("state directory metadata should be available")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(directory_mode, 0o700);
    }
}
