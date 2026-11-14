// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the account's SOFTWARE PACKAGE CONFIGURATION:
//! `UpdatePackageConfiguration` and the `GetPackageConfiguration` reader that proves
//! what it persisted.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what an
//! AWS SDK client would see, not an internal function's return value.
//!
//! The rule under test: an account-wide configuration both operations address with no
//! resource name (`PATCH /package-configuration` and `GET /package-configuration`,
//! `smithy_query(query="operation",operation="GetPackageConfiguration,UpdatePackageConfiguration")`)
//! is ONE record: the reader reports it for an account that has never written it — the
//! version reporting off, since `VersionUpdateByJobsConfig$enabled` "indicates whether
//! the Job is enabled or not" — the update patches it, so each member a request supplies
//! replaces the stored value while one it leaves out leaves that value alone, the update
//! answers an empty body, and the same request applied twice leaves the same record
//! (the operation is `@idempotent`).
//!
//! The table spans what that rule generalises over — which members the update carries
//! (both, `enabled` alone, `roleArn` alone, and none at all), the value of `enabled`,
//! the form and length of `roleArn` including its published bounds, and whether the
//! request carries a `clientToken` — rather than one recorded request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request in this suite is signed for. The configuration is
/// account-wide and carries nothing regional, so no assertion depends on it.
const REGION: &str = "us-east-1";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name:
/// the scope is how the router picks the claiming service. The signature itself is never
/// checked here — the harness turns request authentication off, exactly as an
/// unauthenticated emulator runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    /// The `versionUpdateByJobsConfig` the body reports, or `Value::Null` when it
    /// reports none.
    fn version_update_by_jobs_config(&self) -> Value {
        self.body
            .get("versionUpdateByJobsConfig")
            .cloned()
            .unwrap_or(Value::Null)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it,
/// reached the way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply { status, body }
    }

    /// `UpdatePackageConfiguration`: `PATCH /package-configuration`, with `clientToken`
    /// bound to the query string
    /// (`smithy_query(query="operation",operation="UpdatePackageConfiguration")`).
    async fn update_configuration(&self, body: &Value, client_token: Option<&str>) -> Reply {
        let path = match client_token {
            Some(token) => format!("/package-configuration?clientToken={token}"),
            None => "/package-configuration".to_string(),
        };
        self.call("PATCH", &path, Some(body)).await
    }

    /// `GetPackageConfiguration`: `GET /package-configuration`.
    async fn get_configuration(&self) -> Reply {
        self.call("GET", "/package-configuration", None).await
    }
}

/// A role ARN of `length` characters: `RoleArn` is bounded 20..2048 with no pattern
/// (`smithy_query(query="shape",shape="RoleArn")`), so a case at a bound pads the path
/// segment to reach it.
fn role_arn_of_length(length: usize) -> String {
    let prefix = "arn:aws:iam::000000000000:role/";
    let base = &prefix[..prefix.len().min(length)];
    format!("{base}{}", "r".repeat(length.saturating_sub(prefix.len())))
}

/// One update, and the configuration the reader must report after it.
#[derive(Debug)]
struct Case {
    /// What this row is, for the assertion messages.
    case: &'static str,
    /// The `versionUpdateByJobsConfig` the update carries, or `None` for a request that
    /// carries no configuration member at all.
    requested: Option<Value>,
    /// A `clientToken` for the rows that carry one (`ClientToken`: length 36..64,
    /// pattern `^\S{36,64}$`).
    client_token: Option<&'static str>,
    /// What `GetPackageConfiguration` must report once the update has been applied.
    expected: Value,
}

/// Every row of the table, each applied to an account whose configuration has never
/// been written.
fn cases() -> Vec<Case> {
    let shortest_role = role_arn_of_length(20);
    let longest_role = role_arn_of_length(2048);
    vec![
        Case {
            case: "both members, reporting enabled",
            requested: Some(json!({
                "enabled": true,
                "roleArn": "arn:aws:iam::000000000000:role/iot-package-version-reporting",
            })),
            client_token: None,
            expected: json!({
                "enabled": true,
                "roleArn": "arn:aws:iam::000000000000:role/iot-package-version-reporting",
            }),
        },
        Case {
            case: "both members, reporting disabled, with a client token",
            requested: Some(json!({
                "enabled": false,
                "roleArn": "arn:aws:iam::000000000000:role/iot-jobs",
            })),
            client_token: Some("00000000-0000-4000-8000-000000000001-token"),
            expected: json!({
                "enabled": false,
                "roleArn": "arn:aws:iam::000000000000:role/iot-jobs",
            }),
        },
        Case {
            case: "enabled alone, leaving the account with no role",
            requested: Some(json!({ "enabled": true })),
            client_token: None,
            expected: json!({ "enabled": true }),
        },
        Case {
            // 20 is the published minimum length of `RoleArn`.
            case: "a role at the published minimum length alone, leaving reporting off",
            requested: Some(json!({ "roleArn": shortest_role })),
            client_token: None,
            expected: json!({ "enabled": false, "roleArn": shortest_role }),
        },
        Case {
            // 2048 is the published maximum length of `RoleArn`.
            case: "a role at the published maximum length, reporting enabled",
            requested: Some(json!({ "enabled": true, "roleArn": longest_role })),
            client_token: Some("00000000-0000-4000-8000-000000000002-token"),
            expected: json!({ "enabled": true, "roleArn": longest_role }),
        },
        Case {
            case: "an empty configuration, which changes nothing",
            requested: Some(json!({})),
            client_token: None,
            expected: json!({ "enabled": false }),
        },
        Case {
            case: "no configuration member at all, which changes nothing",
            requested: None,
            client_token: None,
            expected: json!({ "enabled": false }),
        },
    ]
}

/// What the update records is what the reader reports, over every row of the table —
/// and applying the same request twice leaves the same record.
#[tokio::test]
async fn update_package_configuration_records_what_get_package_configuration_reports() {
    for case in cases() {
        // A fresh emulator per row: each row starts from an account whose configuration
        // has never been written.
        let emulator = Emulator::new();
        let Case {
            case: label,
            requested,
            client_token,
            expected,
        } = case;

        let body = match requested.as_ref() {
            Some(config) => json!({ "versionUpdateByJobsConfig": config }),
            None => json!({}),
        };
        let updated = emulator.update_configuration(&body, client_token).await;
        assert_eq!(
            updated.status, 200,
            "case {label:?}: the update must answer 200, got {updated:?}"
        );
        assert!(
            updated.body == Value::Null || updated.body == json!({}),
            "case {label:?}: the update's response binds no members, got {updated:?}"
        );

        let read = emulator.get_configuration().await;
        assert_eq!(
            read.status, 200,
            "case {label:?}: the read must answer 200, got {read:?}"
        );
        assert_eq!(
            read.version_update_by_jobs_config(),
            expected,
            "case {label:?}: expected {expected:?}, got {read:?}"
        );

        // The same request applied again leaves the same record, and a read does not
        // change what a later reader sees.
        let again = emulator.update_configuration(&body, client_token).await;
        assert_eq!(
            again.status, 200,
            "case {label:?}: the update repeated must answer 200, got {again:?}"
        );
        let read_again = emulator.get_configuration().await;
        assert_eq!(
            read_again.body, read.body,
            "case {label:?}: the record must be stable across reads, got {read_again:?}"
        );
    }
}

/// An account that has never written the configuration still HAS one: the reader
/// reports version reporting off and names no role, instead of refusing.
#[tokio::test]
async fn an_unconfigured_account_reports_reporting_turned_off() {
    let emulator = Emulator::new();

    for attempt in ["first read", "second read"] {
        let read = emulator.get_configuration().await;
        assert_eq!(
            read.status, 200,
            "case {attempt:?}: an unconfigured account must be reported, got {read:?}"
        );
        assert_eq!(
            read.version_update_by_jobs_config(),
            json!({ "enabled": false }),
            "case {attempt:?}: expected reporting off and no role, got {read:?}"
        );
    }
}

/// A later update PATCHES the record: a member it supplies replaces the stored value and
/// one it leaves out leaves that value alone, so turning reporting off does not discard
/// the role the account configured.
#[tokio::test]
async fn each_update_patches_only_the_members_it_carries() {
    let emulator = Emulator::new();
    let role = "arn:aws:iam::000000000000:role/iot-package-version-reporting";
    let other_role = "arn:aws:iam::000000000000:role/iot-package-version-reporting-2";

    // (case, the configuration sent, what the reader must report afterwards)
    let steps = [
        (
            "both members turn reporting on under a role",
            json!({ "enabled": true, "roleArn": role }),
            json!({ "enabled": true, "roleArn": role }),
        ),
        (
            "enabled alone turns reporting off and keeps the role",
            json!({ "enabled": false }),
            json!({ "enabled": false, "roleArn": role }),
        ),
        (
            "a role alone replaces the role and keeps reporting off",
            json!({ "roleArn": other_role }),
            json!({ "enabled": false, "roleArn": other_role }),
        ),
        (
            "an empty configuration keeps both",
            json!({}),
            json!({ "enabled": false, "roleArn": other_role }),
        ),
        (
            "enabled alone turns reporting back on under the stored role",
            json!({ "enabled": true }),
            json!({ "enabled": true, "roleArn": other_role }),
        ),
    ];

    for (label, sent, expected) in steps {
        let updated = emulator
            .update_configuration(&json!({ "versionUpdateByJobsConfig": sent }), None)
            .await;
        assert_eq!(
            updated.status, 200,
            "case {label:?}: the update must answer 200, got {updated:?}"
        );
        let read = emulator.get_configuration().await;
        assert_eq!(
            read.version_update_by_jobs_config(),
            expected,
            "case {label:?}: expected {expected:?}, got {read:?}"
        );
    }
}
