//! Credential generation and account provisioning for Grafana's no-SSO
//! default auth path.
//!
//! Per the plan: a generated random break-glass admin account (operator
//! only), and a generated shared "studio-viewer" account so every artist
//! gets the same read-only credential without per-user account management.
//! SSO/LDAP remains an optional upgrade path configured separately.

use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};

const PASSWORD_LENGTH: usize = 24;
const VIEWER_LOGIN: &str = "studio-viewer";

/// Build a `Basic` auth header value for `username`/`password`, since
/// `ureq` 2.x has no built-in basic-auth helper.
fn basic_auth_header(username: &str, password: &str) -> String {
    format!("Basic {}", BASE64.encode(format!("{username}:{password}")))
}

/// Generate a random alphanumeric password suitable for Grafana's built-in
/// basic-auth user provisioning.
pub fn generate_password() -> String {
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(PASSWORD_LENGTH)
        .map(char::from)
        .collect()
}

/// After Grafana is up, ensure the shared "studio-viewer" account exists
/// with the generated password -- the documented no-SSO default so every
/// artist gets the same read-only credential without per-user account
/// management.
///
/// Waits for Grafana's own unauthenticated `/api/health` endpoint before
/// ever attempting an authenticated call. This is load-bearing, not just a
/// courtesy: Grafana applies `GF_SECURITY_ADMIN_PASSWORD` synchronously
/// during its own startup migration, strictly before its HTTP server
/// starts listening -- so racing an authenticated request against a
/// still-booting Grafana risks a genuine (if transient) 401. Confirmed
/// directly that retrying *through* that 401 every few seconds is actively
/// harmful, not just ineffective: Grafana's brute-force login protection
/// then locks the `admin` account out ("too many consecutive incorrect
/// login attempts") for its own cooldown window, which a short fixed-
/// interval retry loop keeps re-triggering indefinitely, meaning the
/// account is never provisioned even though the password was correct the
/// whole time. Waiting for `/api/health` first avoids ever sending that
/// first premature (and self-defeating) authenticated request.
///
/// Best-effort: `start` should still report success even if this fails
/// (e.g. Grafana is unusually slow to become healthy) -- the operator can
/// always re-run `start` to retry provisioning, and it's the log-out-loud
/// case `status`/`start` output should surface, not a hard failure that
/// prevents the rest of the stack from being usable.
pub fn ensure_viewer_account(
    grafana_url: &str,
    admin_password: &str,
    viewer_password: &str,
) -> Result<(), String> {
    wait_for_health(grafana_url, Duration::from_secs(60))?;
    try_ensure_viewer_account(grafana_url, admin_password, viewer_password)
}

/// Poll Grafana's unauthenticated `/api/health` endpoint until it responds
/// successfully or `budget` elapses.
fn wait_for_health(grafana_url: &str, budget: Duration) -> Result<(), String> {
    let deadline = Instant::now() + budget;
    let health_url = format!("{grafana_url}/api/health");
    let mut last_error = String::from("Grafana did not become reachable in time");

    while Instant::now() < deadline {
        match ureq::get(&health_url).call() {
            Ok(_) => return Ok(()),
            Err(error) => last_error = error.to_string(),
        }
        std::thread::sleep(Duration::from_secs(2));
    }

    Err(format!(
        "Grafana never became healthy at {health_url}: {last_error}"
    ))
}

fn try_ensure_viewer_account(
    grafana_url: &str,
    admin_password: &str,
    viewer_password: &str,
) -> Result<(), String> {
    let create_url = format!("{grafana_url}/api/admin/users");
    let body = serde_json::json!({
        "name": "Studio Viewer",
        "login": VIEWER_LOGIN,
        "password": viewer_password,
        "OrgId": 1,
    });

    let response = ureq::post(&create_url)
        .set("Content-Type", "application/json")
        .set("Authorization", &basic_auth_header("admin", admin_password))
        .send_json(body);

    match response {
        Ok(_) => Ok(()),
        // Grafana returns 412 (Precondition Failed) for a login that
        // already exists -- converge by updating its password instead, so
        // re-running `start` after a password rotation still works.
        Err(ureq::Error::Status(412, _)) => {
            update_viewer_password(grafana_url, admin_password, viewer_password)
        }
        Err(error) => Err(error.to_string()),
    }
}

fn update_viewer_password(
    grafana_url: &str,
    admin_password: &str,
    viewer_password: &str,
) -> Result<(), String> {
    let lookup_url = format!("{grafana_url}/api/users/lookup?loginOrEmail={VIEWER_LOGIN}");
    let user: serde_json::Value = ureq::get(&lookup_url)
        .set("Authorization", &basic_auth_header("admin", admin_password))
        .call()
        .map_err(|error| error.to_string())?
        .into_json()
        .map_err(|error| error.to_string())?;
    let user_id = user["id"]
        .as_u64()
        .ok_or_else(|| "Grafana user lookup response had no 'id' field".to_string())?;

    let update_url = format!("{grafana_url}/api/admin/users/{user_id}/password");
    ureq::put(&update_url)
        .set("Content-Type", "application/json")
        .set("Authorization", &basic_auth_header("admin", admin_password))
        .send_json(serde_json::json!({ "password": viewer_password }))
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn generated_password_has_the_expected_length() {
        assert_eq!(generate_password().len(), PASSWORD_LENGTH);
    }

    #[test]
    fn generated_password_is_alphanumeric() {
        let password = generate_password();
        assert!(password.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn repeated_generation_is_not_constant() {
        let passwords: HashSet<String> = (0..10).map(|_| generate_password()).collect();
        assert!(
            passwords.len() > 1,
            "expected varied passwords, got {passwords:?}"
        );
    }

    #[test]
    fn ensure_viewer_account_reports_an_error_when_grafana_is_unreachable() {
        // No server listening on this port -- exercises the
        // no-account-created failure path without needing a real Grafana
        // instance. Uses `try_ensure_viewer_account` directly (bypassing
        // `wait_for_health`'s own 60s public-API budget) so the test stays
        // fast.
        let result = try_ensure_viewer_account("http://127.0.0.1:1", "admin", "viewer-pass");
        assert!(result.is_err());
    }

    #[test]
    fn wait_for_health_times_out_when_nothing_is_listening() {
        // A near-zero budget keeps this test fast while still exercising
        // the real timeout path (no server listening on this port).
        let result = wait_for_health("http://127.0.0.1:1", Duration::from_millis(50));
        assert!(result.is_err());
    }
}
