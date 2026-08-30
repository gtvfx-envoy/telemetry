//! Tempo retention configuration.
//!
//! Tempo's own config file remains the durable source of truth for the
//! active retention period; `telemetry-controller configure
//! --retention-days=N` is a thin convenience wrapper that edits this one
//! field (and, when the server is running, reloads Tempo) rather than
//! requiring an operator to hand-edit YAML. The config file survives a
//! controller reinstall since it lives in the published bundle, not under
//! the config root.
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
