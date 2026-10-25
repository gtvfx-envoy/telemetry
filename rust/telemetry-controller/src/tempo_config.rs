//! Tempo retention configuration.
//!
//! Tempo's mutable runtime config lives under the controller state directory,
//! outside the replaceable published bundle. On first start it is copied from
//! the bundle template; later starts preserve the runtime copy and reassert
//! the retention value recorded in controller state.
//!
//! Retention lives at `overrides.block_retention` (flat, no `defaults:`
//! wrapper, requiring `overrides.enable_legacy_overrides: true`
//! alongside it), not under a `compactor:` block -- Tempo 3.x removed
//! `ingester:`/`compactor:` entirely, in every deployment mode including
//! monolithic, per the official "Migrate from Tempo 2.x to 3.0" guide.
//! Confirmed directly against the real `grafana/tempo:3.0.3` binary via
//! `-config.verify=true`: `compactor.compaction.block_retention` fails
//! ("field compactor not found"), `overrides.defaults.block_retention`
//! also fails ("unknown extension key" / "field defaults not found in
//! type overrides.legacyConfig" -- the legacy overrides struct has no
//! `defaults` wrapper), and only the flat `overrides.block_retention`
//! shape parses successfully (with an expected, harmless deprecation
//! warning about the legacy overrides format).

use std::fs;
use std::path::Path;

use serde_yaml::{Mapping, Value};

const RETENTION_PATH: &[&str] = &["overrides", "block_retention"];
const ENABLE_LEGACY_OVERRIDES_PATH: &[&str] = &["overrides", "enable_legacy_overrides"];

/// Create or update the mutable Tempo runtime configuration.
///
/// The bundle template is copied only when the runtime file does not yet
/// exist. This preserves operator configuration across bundle upgrades while
/// keeping the controller's recorded retention value authoritative.
pub fn prepare_runtime_config(
    template_path: &Path,
    runtime_path: &Path,
    retention_days: u32,
) -> Result<(), String> {
    if !runtime_path.is_file() {
        let parent = runtime_path.parent().ok_or_else(|| {
            format!(
                "runtime Tempo config has no parent directory: {}",
                runtime_path.display()
            )
        })?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        fs::copy(template_path, runtime_path).map_err(|error| {
            format!(
                "failed to copy Tempo config template {} to {}: {error}",
                template_path.display(),
                runtime_path.display()
            )
        })?;
    }
    set_retention_days(runtime_path, retention_days)?;
    set_container_readable_permissions(runtime_path)
}

#[cfg(unix)]
fn set_container_readable_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let parent = path
        .parent()
        .ok_or_else(|| format!("Tempo config has no parent directory: {}", path.display()))?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o755))
        .map_err(|error| format!("failed to set permissions on {}: {error}", parent.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))
        .map_err(|error| format!("failed to set permissions on {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_container_readable_permissions(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Set the retention period, in days, in the Tempo config file at
/// `tempo_config_path`. Stored as Tempo's own Go-duration-string format
/// (e.g. `720h` for 30 days), since that is what Tempo itself expects.
pub fn set_retention_days(tempo_config_path: &Path, days: u32) -> Result<(), String> {
    let hours = days
        .checked_mul(24)
        .ok_or_else(|| format!("retention_days={days} is too large (overflows hours)"))?;

    let text = fs::read_to_string(tempo_config_path)
        .map_err(|error| format!("failed to read {}: {error}", tempo_config_path.display()))?;
    let mut root: Value = serde_yaml::from_str(&text)
        .map_err(|error| format!("failed to parse {}: {error}", tempo_config_path.display()))?;

    set_nested(
        &mut root,
        RETENTION_PATH,
        Value::String(format!("{hours}h")),
    )?;
    // Tempo 3.x's legacy overrides struct (the only shape that accepts a
    // flat `block_retention` -- see module docs) requires this flag
    // alongside it, or the config is rejected outright. Always (re)assert
    // it here, not just when the `overrides` map is created fresh, so a
    // config that's merely missing this one field also gets repaired.
    set_nested(&mut root, ENABLE_LEGACY_OVERRIDES_PATH, Value::Bool(true))?;

    let updated = serde_yaml::to_string(&root)
        .map_err(|error| format!("failed to serialize updated Tempo config: {error}"))?;
    fs::write(tempo_config_path, updated)
        .map_err(|error| format!("failed to write {}: {error}", tempo_config_path.display()))?;
    Ok(())
}

/// Read the currently-configured retention period, in days, from the Tempo
/// config file, if present, parseable, and expressible in whole days (the
/// stored hour count is a multiple of 24). A hand-edited value that isn't a
/// whole number of days (e.g. `25h`) returns `None` rather than a silently
/// rounded-down day count.
pub fn get_retention_days(tempo_config_path: &Path) -> Option<u32> {
    let text = fs::read_to_string(tempo_config_path).ok()?;
    let root: Value = serde_yaml::from_str(&text).ok()?;
    let value = get_nested(&root, RETENTION_PATH)?;
    parse_hours_suffix(value.as_str()?)
}

fn parse_hours_suffix(text: &str) -> Option<u32> {
    let hours: u32 = text.strip_suffix('h')?.parse().ok()?;
    hours.is_multiple_of(24).then_some(hours / 24)
}

fn get_nested<'a>(root: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = root;
    for key in path {
        current = current
            .as_mapping()?
            .get(Value::String((*key).to_string()))?;
    }
    Some(current)
}

fn set_nested(root: &mut Value, path: &[&str], new_value: Value) -> Result<(), String> {
    if !root.is_mapping() {
        *root = Value::Mapping(Mapping::new());
    }

    let mut current = root;
    for (index, key) in path.iter().enumerate() {
        let key_value = Value::String((*key).to_string());
        let mapping = current
            .as_mapping_mut()
            .ok_or_else(|| "expected a YAML mapping while updating Tempo config".to_string())?;

        if index == path.len() - 1 {
            mapping.insert(key_value, new_value);
            return Ok(());
        }

        if !mapping.contains_key(&key_value) {
            mapping.insert(key_value.clone(), Value::Mapping(Mapping::new()));
        }
        current = mapping
            .get_mut(&key_value)
            .expect("key was just inserted or already present");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn write_base_config(path: &Path) {
        fs::write(
            path,
            "server:\n  http_listen_port: 3200\noverrides:\n  enable_legacy_overrides: true\n  block_retention: 720h\n",
        )
        .expect("base config should be written");
    }

    #[test]
    fn set_retention_days_updates_existing_field() {
        let temp_dir = tempdir().expect("tempdir should be created");
        let config_path = temp_dir.path().join("tempo.yaml");
        write_base_config(&config_path);

        set_retention_days(&config_path, 14).expect("should update retention");

        assert_eq!(get_retention_days(&config_path), Some(14));
    }

    #[test]
    fn prepare_runtime_config_copies_template_once_and_preserves_runtime_changes() {
        let temp_dir = tempdir().expect("tempdir should be created");
        let template_path = temp_dir.path().join("template.yaml");
        let runtime_path = temp_dir.path().join("state").join("tempo.yaml");
        write_base_config(&template_path);

        prepare_runtime_config(&template_path, &runtime_path, 14)
            .expect("runtime config should be prepared");
        let mut runtime_text =
            fs::read_to_string(&runtime_path).expect("runtime config should be readable");
        runtime_text.push_str("custom_runtime_field: preserved\n");
        fs::write(&runtime_path, runtime_text).expect("runtime config should be editable");

        fs::write(&template_path, "template_replacement: true\n")
            .expect("template should be replaceable");
        prepare_runtime_config(&template_path, &runtime_path, 45)
            .expect("existing runtime config should be updated");

        let updated =
            fs::read_to_string(&runtime_path).expect("runtime config should remain readable");
        assert!(updated.contains("custom_runtime_field: preserved"));
        assert!(!updated.contains("template_replacement"));
        assert_eq!(get_retention_days(&runtime_path), Some(45));
    }

    #[cfg(unix)]
    #[test]
    fn prepare_runtime_config_is_readable_by_the_tempo_container_user() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempdir().expect("tempdir should be created");
        let template_path = temp_dir.path().join("template.yaml");
        let runtime_path = temp_dir.path().join("runtime").join("tempo.yaml");
        write_base_config(&template_path);

        prepare_runtime_config(&template_path, &runtime_path, 30)
            .expect("runtime config should be prepared");

        let directory_mode = fs::metadata(runtime_path.parent().unwrap())
            .expect("runtime directory metadata should be available")
            .permissions()
            .mode()
            & 0o777;
        let file_mode = fs::metadata(&runtime_path)
            .expect("runtime file metadata should be available")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(directory_mode, 0o755);
        assert_eq!(file_mode, 0o644);
    }

    #[test]
    fn set_retention_days_creates_missing_nested_keys() {
        let temp_dir = tempdir().expect("tempdir should be created");
        let config_path = temp_dir.path().join("tempo.yaml");
        fs::write(&config_path, "server:\n  http_listen_port: 3200\n")
            .expect("base config should be written");

        set_retention_days(&config_path, 45).expect("should update retention");

        assert_eq!(get_retention_days(&config_path), Some(45));
    }

    #[test]
    fn set_retention_days_preserves_unrelated_fields() {
        let temp_dir = tempdir().expect("tempdir should be created");
        let config_path = temp_dir.path().join("tempo.yaml");
        write_base_config(&config_path);

        set_retention_days(&config_path, 60).expect("should update retention");

        let updated = fs::read_to_string(&config_path).expect("file should be readable");
        assert!(updated.contains("http_listen_port: 3200"));
    }

    #[test]
    fn get_retention_days_returns_none_for_missing_file() {
        assert_eq!(get_retention_days(Path::new("does-not-exist.yaml")), None);
    }

    #[test]
    fn default_retention_matches_thirty_day_default() {
        let temp_dir = tempdir().expect("tempdir should be created");
        let config_path = temp_dir.path().join("tempo.yaml");
        write_base_config(&config_path);

        assert_eq!(get_retention_days(&config_path), Some(30));
    }

    #[test]
    fn set_retention_days_asserts_enable_legacy_overrides_when_creating_keys() {
        let temp_dir = tempdir().expect("tempdir should be created");
        let config_path = temp_dir.path().join("tempo.yaml");
        fs::write(&config_path, "server:\n  http_listen_port: 3200\n")
            .expect("base config should be written");

        set_retention_days(&config_path, 45).expect("should update retention");

        let updated = fs::read_to_string(&config_path).expect("file should be readable");
        let root: Value = serde_yaml::from_str(&updated).expect("updated config should parse");
        assert_eq!(
            get_nested(&root, ENABLE_LEGACY_OVERRIDES_PATH),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn set_retention_days_rejects_overflowing_input() {
        let temp_dir = tempdir().expect("tempdir should be created");
        let config_path = temp_dir.path().join("tempo.yaml");
        write_base_config(&config_path);

        let result = set_retention_days(&config_path, u32::MAX);

        assert!(result.is_err());
    }

    #[test]
    fn get_retention_days_returns_none_for_non_multiple_of_24_hours() {
        let temp_dir = tempdir().expect("tempdir should be created");
        let config_path = temp_dir.path().join("tempo.yaml");
        fs::write(
            &config_path,
            "server:\n  http_listen_port: 3200\noverrides:\n  enable_legacy_overrides: true\n  block_retention: 25h\n",
        )
        .expect("hand-edited config should be written");

        assert_eq!(get_retention_days(&config_path), None);
    }
}
