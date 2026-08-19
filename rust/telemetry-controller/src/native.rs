//! Native (Docker-free) process launching for the Linux x86-64 runtime.
//!
//! The native artifacts (Collector, Tempo, Grafana) are fetched from
//! pre-staged studio storage at publish time (see
//! `.envoy/publish-manifest.yaml`), never vendored into this crate. This
//! module only knows the expected layout relative to the controller
//! binary's own location within the published bundle.

use std::path::PathBuf;

/// Subdirectory name (relative to the controller binary) holding native
/// artifacts, matching the destination the publish manifest assembles them
/// into.
const NATIVE_DIR_NAME: &str = "native";

pub struct NativeLayout {
    pub root: PathBuf,
}

impl NativeLayout {
    /// Search upward from the running binary's own location for a `native/`
    /// directory, matching how [`crate::compose::default_compose_file`]
    /// locates the Compose file.
    pub fn discover() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        Self::discover_from(&exe)
    }

    fn discover_from(start: &std::path::Path) -> Option<Self> {
        for ancestor in start.ancestors().take(6) {
            let candidate = ancestor.join(NATIVE_DIR_NAME);
            if candidate.is_dir() {
                return Some(Self { root: candidate });
            }
        }
        None
    }

    pub fn collector_binary(&self) -> PathBuf {
        self.root.join("otelcol-contrib")
    }

    pub fn tempo_binary(&self) -> PathBuf {
        self.root.join("tempo")
    }

    pub fn grafana_binary(&self) -> PathBuf {
        self.root.join("grafana-server")
    }

    pub fn collector_config(&self) -> PathBuf {
        self.root.join("otel-collector-config.yaml")
    }

    pub fn tempo_config(&self) -> PathBuf {
        self.root.join("tempo.yaml")
    }

    /// Human-readable descriptions of any required native artifacts that
    /// are missing, for an actionable `start` error message.
    pub fn missing_artifacts(&self) -> Vec<String> {
        [
            ("collector binary", self.collector_binary()),
            ("collector config", self.collector_config()),
            ("tempo binary", self.tempo_binary()),
            ("tempo config", self.tempo_config()),
            ("grafana binary", self.grafana_binary()),
        ]
        .into_iter()
        .filter(|(_, path)| !path.is_file())
        .map(|(name, path)| format!("{name} ({})", path.display()))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn discovers_native_directory_alongside_the_binary() {
        let temp_dir = tempdir().expect("tempdir should be created");
        let fake_exe = temp_dir.path().join("bin").join("telemetry-controller");
        fs::create_dir_all(fake_exe.parent().unwrap().join(NATIVE_DIR_NAME))
            .expect("dir should be created");

        let layout = NativeLayout::discover_from(&fake_exe).expect("should discover layout");
        assert_eq!(
            layout.root,
            fake_exe.parent().unwrap().join(NATIVE_DIR_NAME)
        );
    }

    #[test]
    fn returns_none_when_no_native_directory_exists() {
        let temp_dir = tempdir().expect("tempdir should be created");
        let fake_exe = temp_dir.path().join("bin").join("telemetry-controller");
        fs::create_dir_all(fake_exe.parent().unwrap()).expect("dir should be created");

        assert!(NativeLayout::discover_from(&fake_exe).is_none());
    }

    #[test]
    fn reports_all_artifacts_missing_from_an_empty_layout() {
        let temp_dir = tempdir().expect("tempdir should be created");
        let layout = NativeLayout {
            root: temp_dir.path().to_path_buf(),
        };

        let missing = layout.missing_artifacts();
        assert_eq!(missing.len(), 5, "missing was: {missing:?}");
    }

    #[test]
    fn reports_no_missing_artifacts_once_all_are_present() {
        let temp_dir = tempdir().expect("tempdir should be created");
        let layout = NativeLayout {
            root: temp_dir.path().to_path_buf(),
        };

        for path in [
            layout.collector_binary(),
            layout.collector_config(),
            layout.tempo_binary(),
            layout.tempo_config(),
            layout.grafana_binary(),
        ] {
            fs::write(&path, b"placeholder").expect("artifact should be written");
        }

        assert!(layout.missing_artifacts().is_empty());
    }
}
