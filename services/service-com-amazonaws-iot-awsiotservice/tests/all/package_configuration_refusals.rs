// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of what the account's SOFTWARE PACKAGE CONFIGURATION reports
//! after a write the model does not admit: `UpdatePackageConfiguration` refused outside
//! its constraints, read back through `GetPackageConfiguration`.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: a request outside the constraints its model declares is refused
//! before any handler runs, under the error shape THAT operation models for an invalid
//! request — `ValidationException` for the software-package plane
//! (`smithy_query(query="operation",operation="UpdatePackageConfiguration")`) — and such a
//! refusal persists nothing, so the one account-wide record
//! (`GET /package-configuration`) keeps reporting what the last ACCEPTED update wrote;
//! reading it never changes it either.
//!
//! The table spans what that rule generalises over rather than one request: which member
//! breaks its constraint (`versionUpdateByJobsConfig.roleArn`, bounded 20..2048 with no
//! expression, and the `clientToken` query parameter, bounded 36..64 under
//! `^\S{36,64}$`), which end of a bound it breaks, a length clause alone and beside an
//! expression clause, and the record the account starts from (never written, and written
//! with reporting on under a role).

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request in this suite is signed for. The configuration is
/// account-wide and carries nothing regional, so no assertion depends on it.
const REGION: &str = "ap-south-1";

/// The error shape the software-package plane models for an invalid request, as it is
/// named on the wire: declared in this service's own namespace, so it carries no API doc
/// namespace.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// A `clientToken` of the modelled form — length 36..64, pattern `^\S{36,64}$` — for the
/// rows whose broken constraint is on another member.
const WELL_FORMED_CLIENT_TOKEN: &str = "00000000-0000-4000-8000-000000000001";

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
    /// The `x-amzn-errortype` header, verbatim — including any namespace the shape name
    /// is qualified with.
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// The `versionUpdateByJobsConfig` the body reports, or `Value::Null` when it reports
    /// none.
    fn version_update_by_jobs_config(&self) -> Value {
        self.body
            .get("versionUpdateByJobsConfig")
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// The `message` the body carries, or `""` when it carries none.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it.
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
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_type,
            body,
        }
    }

    /// `UpdatePackageConfiguration`: `PATCH /package-configuration`, with `clientToken`
    /// bound to the query string.
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
/// (`smithy_query(query="shape",shape="RoleArn")`), so a case at or past a bound pads the
/// path segment to reach it.
fn role_arn_of_length(length: usize) -> String {
    let prefix = "arn:aws:iam::000000000000:role/";
    let base = &prefix[..prefix.len().min(length)];
    format!("{base}{}", "r".repeat(length.saturating_sub(prefix.len())))
}

/// A `clientToken` of `length` non-whitespace characters.
fn client_token_of_length(length: usize) -> String {
    "t".repeat(length)
}

/// The clause a refusal states for a member past its `@length` maximum.
fn past_the_maximum(member: &str, maximum: usize) -> String {
    format!(
        "Value at '{member}' failed to satisfy constraint: Member must have length less \
         than or equal to {maximum}"
    )
}

/// The clause a refusal states for a member short of its `@length` minimum.
fn short_of_the_minimum(member: &str, minimum: usize) -> String {
    format!(
        "Value at '{member}' failed to satisfy constraint: Member must have length greater \
         than or equal to {minimum}"
    )
}

/// The clause a refusal states for a member outside its `@pattern`, whose expression is
/// quoted without the whole-value anchors the model wraps it in.
fn outside_the_pattern(member: &str, expression: &str) -> String {
    format!(
        "Value at '{member}' failed to satisfy constraint: Member must satisfy regular \
         expression pattern: {expression}"
    )
}

/// One update this service's validation layer will not accept, and the clauses the
/// refusal must state for it, in the order it states them.
struct RefusedUpdate {
    /// What this row is, for the assertion messages.
    case: String,
    /// The body the update carries.
    body: Value,
    /// The `clientToken` the request binds to the query string, if any.
    client_token: Option<String>,
    clauses: Vec<String>,
}

/// Every row of the table. The bounds named here are the published ones:
/// `VersionUpdateByJobsConfig$roleArn` targets `RoleArn`, length 20..2048 with no
/// expression, and `UpdatePackageConfiguration$clientToken` targets `ClientToken`, length
/// 36..64 under `^\S{36,64}$`
/// (`smithy_query(query="shape",shape="RoleArn,ClientToken")`).
fn refused_updates() -> Vec<RefusedUpdate> {
    let mut rows = Vec::new();

    let role_member = "versionUpdateByJobsConfig.roleArn";
    for (case, role, clauses) in [
        (
            "a role one character short of the published minimum",
            role_arn_of_length(19),
            vec![short_of_the_minimum(role_member, 20)],
        ),
        (
            "a role one character past the published maximum",
            role_arn_of_length(2049),
            vec![past_the_maximum(role_member, 2048)],
        ),
    ] {
        // The same broken bound with and without a well-formed client token: which
        // member is refused does not depend on the idempotency token beside it.
        rows.push(RefusedUpdate {
            case: format!("{case}, no client token"),
            body: json!({ "versionUpdateByJobsConfig": { "enabled": true, "roleArn": role } }),
            client_token: None,
            clauses: clauses.clone(),
        });
        rows.push(RefusedUpdate {
            case: format!("{case}, with a well-formed client token"),
            body: json!({ "versionUpdateByJobsConfig": { "roleArn": role } }),
            client_token: Some(WELL_FORMED_CLIENT_TOKEN.to_string()),
            clauses,
        });
    }

    let token_member = "clientToken";
    for (case, token, clauses) in [
        (
            "a client token one character short of its minimum",
            client_token_of_length(35),
            vec![
                short_of_the_minimum(token_member, 36),
                outside_the_pattern(token_member, r"\S{36,64}"),
            ],
        ),
        (
            "a client token one character past its maximum",
            client_token_of_length(65),
            vec![
                past_the_maximum(token_member, 64),
                outside_the_pattern(token_member, r"\S{36,64}"),
            ],
        ),
    ] {
        rows.push(RefusedUpdate {
            case: case.to_string(),
            body: json!({ "versionUpdateByJobsConfig": { "enabled": true } }),
            client_token: Some(token),
            clauses,
        });
    }

    rows
}

/// The record an account starts from, and how it is written.
struct StartingRecord {
    case: &'static str,
    /// The accepted update that establishes it, or `None` for an account whose
    /// configuration has never been written.
    written: Option<Value>,
    /// What `GetPackageConfiguration` reports for it.
    reported: Value,
}

/// Both starting records every refused row is applied to.
fn starting_records() -> Vec<StartingRecord> {
    let role = "arn:aws:iam::000000000000:role/iot-package-version-reporting";
    vec![
        StartingRecord {
            case: "an account whose configuration has never been written",
            written: None,
            reported: json!({ "enabled": false }),
        },
        StartingRecord {
            case: "an account reporting versions under a role",
            written: Some(json!({
                "versionUpdateByJobsConfig": { "enabled": true, "roleArn": role },
            })),
            reported: json!({ "enabled": true, "roleArn": role }),
        },
    ]
}

/// RULE: an update outside the model's constraints is refused with 400 under the shape
/// the operation models, and persists nothing — the reader keeps reporting the record the
/// last accepted update wrote, whatever that record is.
#[tokio::test]
async fn a_refused_update_leaves_the_reported_configuration_untouched() {
    for start in starting_records() {
        for refused in refused_updates() {
            let label = format!("{} / {}", start.case, refused.case);

            // A fresh emulator per row: what a refusal persists must not depend on what
            // another row left behind.
            let emulator = Emulator::new();
            if let Some(written) = start.written.as_ref() {
                let accepted = emulator.update_configuration(written, None).await;
                assert_eq!(
                    accepted.status, 200,
                    "case {label:?}: the accepted update must answer 200, got {accepted:?}"
                );
            }

            let reply = emulator
                .update_configuration(&refused.body, refused.client_token.as_deref())
                .await;
            assert_eq!(
                reply.status, 400,
                "case {label:?}: expected status 400, got {reply:?}"
            );
            assert_eq!(
                reply.error_type.as_deref(),
                Some(VALIDATION_EXCEPTION),
                "case {label:?}: expected x-amzn-errortype {VALIDATION_EXCEPTION:?}, got {:?}",
                reply.error_type
            );
            let envelope = match refused.clauses.len() {
                1 => "1 validation error detected: ".to_string(),
                n => format!("{n} validation errors detected: "),
            };
            let expected_message = format!("{envelope}{}", refused.clauses.join("; "));
            assert_eq!(
                reply.message(),
                expected_message,
                "case {label:?}: expected message {expected_message:?}, got {:?}",
                reply.message()
            );

            // What the refusal did NOT do: the account-wide record is the one the last
            // accepted update wrote, and a read leaves it that way for the next reader.
            for attempt in ["first read", "second read"] {
                let read = emulator.get_configuration().await;
                assert_eq!(
                    read.status, 200,
                    "case {label:?} ({attempt}): the read must answer 200, got {read:?}"
                );
                assert_eq!(
                    read.version_update_by_jobs_config(),
                    start.reported,
                    "case {label:?} ({attempt}): expected {:?}, got {read:?}",
                    start.reported
                );
            }
        }
    }
}

/// The other side of the same decision: a role at each published bound is ACCEPTED and
/// read back, so the refusals above are about the lengths the requests sent and not about
/// the operation.
#[tokio::test]
async fn a_role_at_either_published_bound_is_accepted_and_read_back() {
    // 20 and 2048 are the published ends of `RoleArn`'s length bound.
    for (case, length) in [("the published minimum", 20), ("the published maximum", 2048)] {
        let emulator = Emulator::new();
        let role = role_arn_of_length(length);
        let accepted = emulator
            .update_configuration(
                &json!({ "versionUpdateByJobsConfig": { "enabled": true, "roleArn": role } }),
                Some(WELL_FORMED_CLIENT_TOKEN),
            )
            .await;
        assert_eq!(
            accepted.status, 200,
            "case {case:?}: a role of {length} characters must be accepted, got {accepted:?}"
        );

        let read = emulator.get_configuration().await;
        let expected = json!({ "enabled": true, "roleArn": role });
        assert_eq!(
            read.version_update_by_jobs_config(),
            expected,
            "case {case:?}: expected {expected:?}, got {read:?}"
        );
    }
}
