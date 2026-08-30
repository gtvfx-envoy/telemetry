//! File-drop ingestion sweep.
//!
//! Polls a configured drop path, parses each dropped OTLP-JSON-shaped file
//! (written by `envoy-core`'s `FileDropSink`), forwards it to the local
//! Collector's OTLP/HTTP receiver as a minimal valid OTLP JSON span
//! envelope, and deletes the file on success. Failed files are retried up
//! to a bounded limit, then moved to a dead-letter subdirectory instead of
//! retried forever.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rand::Rng;
use serde::Deserialize;
use serde_json::Value;

const DEAD_LETTER_DIR_NAME: &str = "dead-letter";

/// Default bound on per-file retry attempts before a file is moved to the
/// dead-letter subdirectory instead of retried forever.
pub const DEFAULT_MAX_RETRIES: u32 = 5;

/// Flat payload shape written by `envoy_core::telemetry::file_drop::FileDropSink`.
#[derive(Debug, Deserialize)]
struct DroppedPayload {
    name: String,
    attributes: HashMap<String, Value>,
    timestamp_unix_millis: u128,
}

/// Counts from one [`sweep_once`] pass, for `status`/logging.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct SweepOutcome {
    pub forwarded: usize,
    pub retried: usize,
    pub dead_lettered: usize,
    pub not_ready: usize,
}

/// One pass over `drop_dir`: parse every `.json` file, forward it to
/// `collector_endpoint`, and clean up. `retry_counts` is owned by the
/// caller and threaded across repeated calls (e.g. successive polls in a
/// long-running daemon loop) so retry limits are enforced across passes,
/// not just within one.
pub fn sweep_once(
    drop_dir: &Path,
    collector_endpoint: &str,
    bearer_token: Option<&str>,
    max_retries: u32,
    retry_counts: &mut HashMap<PathBuf, u32>,
) -> SweepOutcome {
    let mut outcome = SweepOutcome::default();

    let entries = match fs::read_dir(drop_dir) {
        Ok(entries) => entries,
        Err(_) => return outcome,
    };

    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    // Oldest-first, matching the file-drop sink's timestamp-prefixed
    // naming, so ingestion-health metrics (oldest-unswept-file age) are
    // meaningful.
    paths.sort();

    for path in &paths {
        process_one_file(
            path,
            collector_endpoint,
            bearer_token,
            max_retries,
            retry_counts,
            &mut outcome,
        );
    }

    // Drop stale retry-count entries for files that no longer exist
    // (delivered or dead-lettered), so the map doesn't grow unbounded
    // across a long-running daemon's lifetime.
    let existing: std::collections::HashSet<&PathBuf> = paths.iter().collect();
    retry_counts.retain(|path, _| existing.contains(path));

    outcome
}

fn process_one_file(
    path: &Path,
    collector_endpoint: &str,
    bearer_token: Option<&str>,
    max_retries: u32,
    retry_counts: &mut HashMap<PathBuf, u32>,
    outcome: &mut SweepOutcome,
) {
    let Ok(contents) = fs::read_to_string(path) else {
        outcome.not_ready += 1;
        return;
    };
    let Ok(payload) = serde_json::from_str::<DroppedPayload>(&contents) else {
        // Could be a file caught mid-write (transient) or permanently
        // malformed content -- track it with the same bounded retry
        // mechanism used for forwarding failures below, so a permanently
        // invalid file eventually gets dead-lettered instead of retried
        // forever and growing the drop directory's backlog indefinitely.
        record_retry_or_dead_letter(path, max_retries, retry_counts, outcome);
        return;
    };

    match forward_to_collector(&payload, collector_endpoint, bearer_token) {
        Ok(()) => {
            let _ = fs::remove_file(path);
            retry_counts.remove(path);
            outcome.forwarded += 1;
        }
        Err(_) => record_retry_or_dead_letter(path, max_retries, retry_counts, outcome),
    }
}

/// Bump `path`'s retry count and either dead-letter it (once `max_retries`
/// is reached) or record it as retried this pass. Shared by both the
/// JSON-parse-failure and forwarding-failure paths in [`process_one_file`].
fn record_retry_or_dead_letter(
    path: &Path,
    max_retries: u32,
    retry_counts: &mut HashMap<PathBuf, u32>,
    outcome: &mut SweepOutcome,
) {
    let count = retry_counts.entry(path.to_path_buf()).or_insert(0);
    *count += 1;
    if *count >= max_retries {
        move_to_dead_letter(path);
        retry_counts.remove(path);
        outcome.dead_lettered += 1;
    } else {
        outcome.retried += 1;
    }
}

fn move_to_dead_letter(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    let dead_letter_dir = parent.join(DEAD_LETTER_DIR_NAME);
    if fs::create_dir_all(&dead_letter_dir).is_err() {
        return;
    }
    if let Some(file_name) = path.file_name() {
        let _ = fs::rename(path, dead_letter_dir.join(file_name));
    }
}

fn forward_to_collector(
    payload: &DroppedPayload,
    endpoint: &str,
    bearer_token: Option<&str>,
) -> Result<(), String> {
    let body = build_otlp_json(payload);
    let mut request = ureq::post(endpoint)
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(5));
    if let Some(token) = bearer_token {
        request = request.set("Authorization", &format!("Bearer {token}"));
    }

    request
        .send_json(body)
        .map(|_response| ())
        .map_err(|error| error.to_string())
}

/// Build a minimal, valid OTLP/HTTP JSON trace export payload from the flat
/// file-drop payload. The sweep (part of the telemetry bundle, not envoy
/// itself) owns this OTLP-specific expansion, matching how backend
/// concerns live in the bundle rather than the client.
fn build_otlp_json(payload: &DroppedPayload) -> Value {
    let attributes: Vec<Value> = payload
        .attributes
        .iter()
        .map(|(key, value)| {
            serde_json::json!({"key": key, "value": telemetry_value_to_any_value(value)})
        })
        .collect();

    let end_nanos = payload.timestamp_unix_millis.saturating_mul(1_000_000);

    serde_json::json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [
                    {"key": "service.name", "value": {"stringValue": "envoy"}}
                ]
            },
            "scopeSpans": [{
                "scope": {"name": "envoy-telemetry-sweep"},
                "spans": [{
                    "traceId": random_hex(16),
                    "spanId": random_hex(8),
                    "name": payload.name,
                    "kind": 1,
                    "startTimeUnixNano": end_nanos.to_string(),
                    "endTimeUnixNano": end_nanos.to_string(),
                    "attributes": attributes,
                }]
            }]
        }]
    })
}

/// Convert `envoy_core::telemetry::TelemetryValue`'s serde-derived JSON
/// shape (`{"Str": "..."}`, `{"Bool": true}`, `{"Int": 5}`, `{"Float": 1.5}`)
/// into OTLP JSON's `AnyValue` shape.
fn telemetry_value_to_any_value(value: &Value) -> Value {
    if let Some(object) = value.as_object() {
        if let Some(text) = object.get("Str").and_then(Value::as_str) {
            return serde_json::json!({"stringValue": text});
        }
        if let Some(flag) = object.get("Bool").and_then(Value::as_bool) {
            return serde_json::json!({"boolValue": flag});
        }
        if let Some(number) = object.get("Int").and_then(Value::as_i64) {
            // OTLP JSON encodes 64-bit integers as strings, per protobuf's
            // JSON mapping for int64.
            return serde_json::json!({"intValue": number.to_string()});
        }
        if let Some(number) = object.get("Float").and_then(Value::as_f64) {
            return serde_json::json!({"doubleValue": number});
        }
    }
    serde_json::json!({"stringValue": value.to_string()})
}

fn random_hex(byte_len: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..byte_len)
        .map(|_| format!("{:02x}", rng.gen::<u8>()))
        .collect()
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use tiny_http::{Response, Server};

    use super::*;

    fn write_drop_file(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).expect("drop file should be written");
    }

    fn sample_payload_json() -> String {
        serde_json::json!({
            "name": "envoy.command.run",
            "attributes": {
                "envoy.command.name": {"Str": "unreal"},
                "envoy.success": {"Bool": true},
                "envoy.exit_code": {"Int": 0},
            },
            "timestamp_unix_millis": 1_700_000_000_000u128,
        })
        .to_string()
    }

    #[test]
    fn forwards_a_valid_file_and_removes_it() {
        let server = Server::http("127.0.0.1:0").expect("mock server should bind");
        let addr = server
            .server_addr()
            .to_ip()
            .expect("should have an IP addr");
        let endpoint = format!("http://{addr}/v1/traces");

        let handle = std::thread::spawn(move || {
            if let Ok(request) = server.recv() {
                let _ = request.respond(Response::from_string("ok"));
            }
        });

        let drop_dir = tempdir().expect("tempdir should be created");
        write_drop_file(drop_dir.path(), "1.json", &sample_payload_json());

        let mut retry_counts = HashMap::new();
        let outcome = sweep_once(
            drop_dir.path(),
            &endpoint,
            None,
            DEFAULT_MAX_RETRIES,
            &mut retry_counts,
        );

        handle.join().expect("server thread should finish");
        assert_eq!(outcome.forwarded, 1);
        assert_eq!(fs::read_dir(drop_dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn retries_bounded_then_dead_letters_a_poison_destination() {
        // No server listening at this address -- every attempt fails.
        let endpoint = "http://127.0.0.1:1/v1/traces";
        let drop_dir = tempdir().expect("tempdir should be created");
        write_drop_file(drop_dir.path(), "1.json", &sample_payload_json());

        let mut retry_counts = HashMap::new();
        for _ in 0..2 {
            let outcome = sweep_once(drop_dir.path(), endpoint, None, 3, &mut retry_counts);
            assert_eq!(outcome.retried, 1);
        }
        let final_outcome = sweep_once(drop_dir.path(), endpoint, None, 3, &mut retry_counts);
        assert_eq!(final_outcome.dead_lettered, 1);

        let dead_letter_file = drop_dir.path().join(DEAD_LETTER_DIR_NAME).join("1.json");
        assert!(dead_letter_file.exists());
        assert!(!drop_dir.path().join("1.json").exists());
    }

    #[test]
    fn non_json_file_is_left_for_a_later_pass() {
        let endpoint = "http://127.0.0.1:1/v1/traces";
        let drop_dir = tempdir().expect("tempdir should be created");
        write_drop_file(drop_dir.path(), "1.json", "not valid json");

        let mut retry_counts = HashMap::new();
        let outcome = sweep_once(drop_dir.path(), endpoint, None, 3, &mut retry_counts);

        // Tracked via the same bounded retry mechanism as forwarding
        // failures now (see permanently_invalid_json_is_eventually_dead_lettered),
        // so a single pass still just retries -- it isn't dead-lettered
        // immediately.
        assert_eq!(outcome.retried, 1);
        assert!(drop_dir.path().join("1.json").exists());
    }

    #[test]
    fn permanently_invalid_json_is_eventually_dead_lettered() {
        let endpoint = "http://127.0.0.1:1/v1/traces";
        let drop_dir = tempdir().expect("tempdir should be created");
        write_drop_file(drop_dir.path(), "1.json", "not valid json");

        let mut retry_counts = HashMap::new();
        for _ in 0..2 {
            let outcome = sweep_once(drop_dir.path(), endpoint, None, 3, &mut retry_counts);
            assert_eq!(outcome.retried, 1);
        }
        let final_outcome = sweep_once(drop_dir.path(), endpoint, None, 3, &mut retry_counts);
        assert_eq!(final_outcome.dead_lettered, 1);

        let dead_letter_file = drop_dir.path().join(DEAD_LETTER_DIR_NAME).join("1.json");
        assert!(dead_letter_file.exists());
        assert!(!drop_dir.path().join("1.json").exists());
    }

    #[test]
    fn ignores_non_json_files_in_the_drop_directory() {
        let endpoint = "http://127.0.0.1:1/v1/traces";
        let drop_dir = tempdir().expect("tempdir should be created");
        write_drop_file(drop_dir.path(), "readme.txt", "not telemetry");

        let mut retry_counts = HashMap::new();
        let outcome = sweep_once(drop_dir.path(), endpoint, None, 3, &mut retry_counts);

        assert_eq!(outcome, SweepOutcome::default());
    }

    #[test]
    fn telemetry_value_conversion_maps_every_variant() {
        assert_eq!(
            telemetry_value_to_any_value(&serde_json::json!({"Str": "hi"})),
            serde_json::json!({"stringValue": "hi"})
        );
        assert_eq!(
            telemetry_value_to_any_value(&serde_json::json!({"Bool": true})),
            serde_json::json!({"boolValue": true})
        );
        assert_eq!(
            telemetry_value_to_any_value(&serde_json::json!({"Int": 42})),
            serde_json::json!({"intValue": "42"})
        );
        assert_eq!(
            telemetry_value_to_any_value(&serde_json::json!({"Float": 1.5})),
            serde_json::json!({"doubleValue": 1.5})
        );
    }
}
