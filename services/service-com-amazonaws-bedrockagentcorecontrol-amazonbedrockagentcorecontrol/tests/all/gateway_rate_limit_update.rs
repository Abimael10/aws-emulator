// SPDX-License-Identifier: CC0-1.0

//! `UpdateGatewayRateLimit`, and the read of the rate limit it rewrote, end to end over
//! HTTP.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against the one shared engine.
//!
//! The rule under test, read off the Smithy model rather than off any one recorded request
//! (`smithy_query(query="operation",operation="UpdateGatewayRateLimit,\
//! GetGatewayRateLimit")`): the update REPLACES the configurable members of a rate limit the
//! addressed gateway already holds — the rule entries it submits and the description, which
//! a body naming none clears — keeps the id it was addressed by, the instant it was created
//! and the dimension keys it was created with ("The dimension keys are immutable after
//! creation and cannot be changed"), and reports it `UPDATING` until the clock owes it
//! `ACTIVE`, so the limit reads back through `GetGatewayRateLimit` exactly as the update
//! reported it. A request the stored limit cannot take writes nothing: a gateway or a
//! `rateLimitId` naming nothing is the modelled 404, and a rule entry naming a dimension
//! the stored limit is not scoped by is the modelled validation failure ("Keys must match
//! the parent rate limit's dimension keys").
//!
//! The rows differ along what the rule generalises over: how many dimension keys scope the
//! stored limit and of which published kind, how many rule entries the update submits and
//! whether that is more or fewer than the limit held, which of the three rate metrics each
//! configures, over which period and at which rate, and whether the update sets, replaces,
//! keeps away or clears the description.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for.
const REGION: &str = "us-east-1";

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/GatewayRole";

/// How long a row waits for a clock-owed transition before failing. Far longer than the
/// span `src/state.rs` gives the gateway family, so a loaded or slow host cannot fail a
/// row: a row waits for the CONDITION it needs, never for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a row waits between two looks at the same resource.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header for this service, which is what the router routes on.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// One answer: the HTTP status and the parsed body.
struct Answer {
    status: u16,
    body: Value,
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.us-east-1.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        },
    }
}

/// The error code `restJson1` puts in the body's `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The one sentence this service refuses a lookup of a resource it does not hold in.
fn not_found_message(resource: &str) -> String {
    format!(
        "Failed to retrieve {resource} because it doesn't exist. Retry the request with a \
         different resource identifier."
    )
}

async fn create_rate_limit(router: &MultiServiceRouter, gateway: &str, body: &Value) -> Answer {
    send(
        router,
        "POST",
        &format!("/gateways/{gateway}/rate-limits"),
        Some(body),
    )
    .await
}

async fn update_rate_limit(
    router: &MultiServiceRouter,
    gateway: &str,
    rate_limit: &str,
    body: &Value,
) -> Answer {
    send(
        router,
        "PATCH",
        &format!("/gateways/{gateway}/rate-limits/{rate_limit}"),
        Some(body),
    )
    .await
}

async fn get_rate_limit(router: &MultiServiceRouter, gateway: &str, rate_limit: &str) -> Answer {
    send(
        router,
        "GET",
        &format!("/gateways/{gateway}/rate-limits/{rate_limit}"),
        None,
    )
    .await
}

/// Create a gateway and hand back its `gatewayId`.
async fn gateway(router: &MultiServiceRouter, name: &str) -> String {
    let created = send(
        router,
        "POST",
        "/gateways/",
        Some(&json!({"name": name, "roleArn": ROLE_ARN, "authorizerType": "AWS_IAM"})),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "setup {name:?}: expected HTTP 202 from CreateGateway, got {} ({})",
        created.status, created.body
    );
    text(&created.body, "gatewayId")
}

/// Look again until the rate limit reports `wanted`, which is what a caller polling the
/// status field does. The deadline only catches a limit that never settles at all.
async fn poll_until_status(
    router: &MultiServiceRouter,
    gateway: &str,
    rate_limit: &str,
    wanted: &str,
    case: &str,
) -> Answer {
    let started = Instant::now();
    loop {
        let read = get_rate_limit(router, gateway, rate_limit).await;
        if read.status == 200 && text(&read.body, "status") == wanted {
            return read;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the rate limit never reached {wanted}, last answer {} ({})",
            read.status,
            read.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A rate configuration: `RateConfig` needs a rate in 0..10000000 and a `Period`, whose
/// values are `second | minute` (`smithy_query(query="shape",shape="Period")`).
fn rate(value: f64, period: &str) -> Value {
    json!([{"rate": value, "period": period}])
}

/// What an update does to the description it finds.
#[derive(Debug, Clone, Copy)]
enum Description {
    /// The body carries no `description`, so the limit is left without one.
    Omitted,
    /// The body carries this value.
    Given(&'static str),
}

impl Description {
    fn value(self) -> Option<&'static str> {
        match self {
            Description::Omitted => None,
            Description::Given(value) => Some(value),
        }
    }
}

/// One row of the update table.
struct Row {
    case: &'static str,
    /// The gateway name the row creates (`GatewayName` is `^([0-9a-zA-Z][-]?){1,48}$`).
    gateway: &'static str,
    /// The `rateLimitId` the create names, which the update then addresses.
    rate_limit_id: &'static str,
    /// The dimension keys the limit is created with — immutable afterwards.
    dimension_keys: Vec<&'static str>,
    /// The description and rule entries the create stores.
    created_description: Description,
    created_entries: Vec<Value>,
    /// The description and rule entries the update submits.
    updated_description: Description,
    updated_entries: Vec<Value>,
}

/// The rows. Every `dimensionKeys` value below is from `DimensionKey`'s own published
/// catalogue — `targetName | toolName | qualifiedModelId | $.context.iam.principal |
/// $.context.iam.sourceIdentity | $.context.jwt.<claim> |
/// $.context.request.header.<header> | $.context.request.body.<path>`
/// (`smithy_query(query="shape",shape="DimensionKeys")`) — including entries no recorded
/// request used. The rates include both published bounds of `RateConfig`'s `rate`
/// (`@range 0..10000000`).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one rule entry's rate is raised, the description replaced",
            gateway: "updatelimitrate",
            rate_limit_id: "per-target",
            dimension_keys: vec!["targetName"],
            created_description: Description::Given("the first description"),
            created_entries: vec![json!({
                "dimensions": {"targetName": "orders"},
                "requests": rate(25.0, "second"),
            })],
            updated_description: Description::Given("the second description"),
            updated_entries: vec![json!({
                "dimensions": {"targetName": "orders"},
                "requests": rate(50.0, "second"),
            })],
        },
        Row {
            case: "a limit with no description takes one, and a second rule entry",
            gateway: "updatelimitgrowing",
            rate_limit_id: "by.tool_name",
            dimension_keys: vec!["toolName"],
            created_description: Description::Omitted,
            created_entries: vec![json!({
                "dimensions": {"toolName": "search"},
                "tokens": rate(100_000.0, "minute"),
            })],
            updated_description: Description::Given("now described"),
            updated_entries: vec![
                json!({
                    "dimensions": {"toolName": "search"},
                    "tokens": rate(200_000.0, "minute"),
                }),
                json!({
                    "dimensions": {"toolName": "summarize"},
                    "connections": rate(4.0, "second"),
                }),
            ],
        },
        Row {
            case: "three rule entries are replaced by one, the description cleared",
            gateway: "updatelimitshrinking",
            rate_limit_id: "ab",
            dimension_keys: vec!["targetName", "toolName"],
            created_description: Description::Given("about to be cleared"),
            created_entries: vec![
                json!({
                    "dimensions": {"targetName": "orders", "toolName": "create"},
                    "requests": rate(5.0, "second"),
                }),
                json!({
                    "dimensions": {"targetName": "orders", "toolName": "read"},
                    "tokens": rate(50.0, "minute"),
                    "connections": rate(10.0, "second"),
                }),
                json!({
                    "dimensions": {"targetName": "*"},
                    "requests": rate(1.0, "minute"),
                }),
            ],
            updated_description: Description::Omitted,
            updated_entries: vec![json!({
                "dimensions": {"toolName": "create"},
                "requests": rate(7.0, "minute"),
            })],
        },
        Row {
            case: "a JWT-claim limit moves from the published lower bound to the upper one",
            gateway: "updatelimitjwtclaim",
            rate_limit_id: "jwt_sub.limit",
            dimension_keys: vec!["$.context.jwt.sub"],
            created_description: Description::Given("per caller"),
            created_entries: vec![json!({
                "dimensions": {"$.context.jwt.sub": "caller-1"},
                "connections": rate(0.0, "second"),
            })],
            updated_description: Description::Given(""),
            updated_entries: vec![json!({
                "dimensions": {"$.context.jwt.sub": "caller-1"},
                "requests": rate(10_000_000.0, "minute"),
            })],
        },
        Row {
            case: "a request-scoped limit keeps both its dimension keys across the change",
            gateway: "updatelimitrequestscoped",
            rate_limit_id: "tenant-and-account",
            dimension_keys: vec![
                "$.context.request.header.x-tenant",
                "$.context.request.body.account_id",
            ],
            created_description: Description::Given("per tenant and per account"),
            created_entries: vec![json!({
                "dimensions": {
                    "$.context.request.header.x-tenant": "acme",
                    "$.context.request.body.account_id": "a-1",
                },
                "requests": rate(3.0, "second"),
            })],
            updated_description: Description::Given("per tenant only"),
            updated_entries: vec![json!({
                "dimensions": {"$.context.request.header.x-tenant": "acme"},
                "tokens": rate(9.0, "second"),
            })],
        },
        Row {
            case: "an IAM-principal limit is rewritten with the same entries it held",
            gateway: "updatelimitiamprincipal",
            rate_limit_id: "iam-principal-limit",
            dimension_keys: vec!["$.context.iam.principal"],
            created_description: Description::Omitted,
            created_entries: vec![json!({
                "dimensions": {"$.context.iam.principal": "arn:aws:iam::000000000000:role/App"},
                "requests": rate(11.0, "second"),
            })],
            updated_description: Description::Omitted,
            updated_entries: vec![json!({
                "dimensions": {"$.context.iam.principal": "arn:aws:iam::000000000000:role/App"},
                "requests": rate(11.0, "second"),
            })],
        },
    ]
}

impl Row {
    /// The `CreateGatewayRateLimit` body this row starts from.
    fn create_body(&self) -> Value {
        let mut body = json!({
            "rateLimitId": self.rate_limit_id,
            "dimensionKeys": self.dimension_keys,
            "entries": self.created_entries,
        });
        if let Some(description) = self.created_description.value() {
            body["description"] = json!(description);
        }
        body
    }

    /// The `UpdateGatewayRateLimit` body this row submits.
    fn update_body(&self) -> Value {
        let mut body = json!({"entries": self.updated_entries});
        if let Some(description) = self.updated_description.value() {
            body["description"] = json!(description);
        }
        body
    }
}

/// An update stores exactly the submitted configuration under the limit it addressed,
/// keeps what the create decided, reports `UPDATING`, and reads back unchanged — settling
/// to `ACTIVE` only once the span the clock owes passes.
#[tokio::test]
async fn an_update_replaces_the_configuration_and_keeps_the_identity() {
    for row in rows() {
        let case = row.case;
        let router = router();
        let gateway_id = gateway(&router, row.gateway).await;

        let created = create_rate_limit(&router, &gateway_id, &row.create_body()).await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: setup expected HTTP 201 from CreateGatewayRateLimit, got {} ({})",
            created.status, created.body
        );
        // The limit settles before the change, so the status the update reports can only
        // be the one this call owes it.
        let before = poll_until_status(
            &router,
            &gateway_id,
            row.rate_limit_id,
            "ACTIVE",
            case,
        )
        .await;

        let updated =
            update_rate_limit(&router, &gateway_id, row.rate_limit_id, &row.update_body()).await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: expected HTTP 200 from UpdateGatewayRateLimit, got {} ({})",
            updated.status, updated.body
        );
        assert_eq!(
            updated.body.get("entries"),
            Some(&json!(row.updated_entries)),
            "case {case:?}: the limit reports the rule entries the update submitted, got {}",
            updated.body
        );
        assert_eq!(
            updated.body.get("description").cloned(),
            row.updated_description.value().map(|value| json!(value)),
            "case {case:?}: the description is what the update carried, got {}",
            updated.body
        );
        assert_eq!(
            text(&updated.body, "status"),
            "UPDATING",
            "case {case:?}: an accepted change reports UPDATING, got {}",
            updated.body
        );
        // The id it was addressed by, the gateway it is enforced on, the dimension keys it
        // was created with and the instant it was created all survive the change.
        for member in ["rateLimitId", "gatewayIdentifier", "dimensionKeys", "createdAt"] {
            assert_eq!(
                updated.body.get(member),
                before.body.get(member),
                "case {case:?}: the update leaves the {member} the create stored, got {}",
                updated.body
            );
        }
        assert_eq!(
            updated.body.get("dimensionKeys"),
            Some(&json!(row.dimension_keys)),
            "case {case:?}: the dimension keys are immutable after creation, got {}",
            updated.body
        );
        // The change is stamped: `updatedAt` is the instant this call wrote the limit,
        // which is not the instant the create left behind.
        assert!(
            updated
                .body
                .get("updatedAt")
                .is_some_and(|stamped| !stamped.is_null()),
            "case {case:?}: an updated limit reports when it was updated, got {}",
            updated.body
        );
        assert_ne!(
            updated.body.get("updatedAt"),
            before.body.get("updatedAt"),
            "case {case:?}: the update stamps the instant it wrote the limit, got {}",
            updated.body
        );

        // The round trip: the rewritten limit reads back through the single-limit read.
        let read = get_rate_limit(&router, &gateway_id, row.rate_limit_id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the updated limit must read back, got {} ({})",
            read.status, read.body
        );
        for member in [
            "rateLimitId",
            "gatewayIdentifier",
            "description",
            "dimensionKeys",
            "entries",
            "createdAt",
            "updatedAt",
        ] {
            assert_eq!(
                read.body.get(member),
                updated.body.get(member),
                "case {case:?}: the read reports the {member} the update reported, got {}",
                read.body
            );
        }

        // Nothing but the clock moves the limit back to its terminal status, and the
        // change outlives that transition.
        let settled = poll_until_status(
            &router,
            &gateway_id,
            row.rate_limit_id,
            "ACTIVE",
            case,
        )
        .await;
        assert_eq!(
            settled.body.get("entries"),
            Some(&json!(row.updated_entries)),
            "case {case:?}: the settled limit still holds the submitted entries, got {}",
            settled.body
        );
        assert_eq!(
            settled.body.get("createdAt"),
            before.body.get("createdAt"),
            "case {case:?}: the creation instant is not moved by an update, got {}",
            settled.body
        );
    }
}

/// A rule entry naming a dimension the stored limit is not scoped by is the modelled
/// validation failure, and the limit is left exactly as it was: the dimension keys are the
/// limit's own, and the update cannot name others.
#[tokio::test]
async fn an_entry_outside_the_stored_dimension_keys_is_refused() {
    let router = router();
    let gateway_id = gateway(&router, "updatelimitrefused").await;
    let created = create_rate_limit(
        &router,
        &gateway_id,
        &json!({
            "rateLimitId": "scoped-by-target",
            "description": "left exactly as it was",
            "dimensionKeys": ["targetName"],
            "entries": [{
                "dimensions": {"targetName": "orders"},
                "requests": rate(5.0, "second"),
            }],
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: the limit must be created, got {} ({})",
        created.status, created.body
    );
    let before = poll_until_status(
        &router,
        &gateway_id,
        "scoped-by-target",
        "ACTIVE",
        "the stored limit",
    )
    .await;

    // Each row names a dimension the stored limit does not declare — one from the
    // published catalogue, one a different limit could be scoped by, and one inside an
    // entry that also names the declared key.
    for (case, entries) in [
        (
            "a catalogue key the limit is not scoped by",
            json!([{"dimensions": {"toolName": "search"}, "requests": rate(5.0, "second")}]),
        ),
        (
            "a JWT claim the limit is not scoped by",
            json!([{
                "dimensions": {"$.context.jwt.sub": "caller-1"},
                "tokens": rate(50.0, "minute"),
            }]),
        ),
        (
            "the declared key beside an undeclared one",
            json!([{
                "dimensions": {"targetName": "orders", "qualifiedModelId": "anthropic.claude"},
                "connections": rate(2.0, "second"),
            }]),
        ),
    ] {
        let refused = update_rate_limit(
            &router,
            &gateway_id,
            "scoped-by-target",
            &json!({"entries": entries}),
        )
        .await;
        assert_eq!(
            (refused.status, error_code(&refused.body)),
            (400, "ValidationException".to_string()),
            "case {case:?}: expected HTTP 400 ValidationException, got {} ({})",
            refused.status,
            refused.body
        );
        assert!(
            text(&refused.body, "message").starts_with("entries: names the dimension "),
            "case {case:?}: the refusal names the member and the dimension it found, got {}",
            refused.body
        );

        let unchanged = get_rate_limit(&router, &gateway_id, "scoped-by-target").await;
        for member in ["description", "entries", "dimensionKeys", "createdAt", "updatedAt"] {
            assert_eq!(
                unchanged.body.get(member),
                before.body.get(member),
                "case {case:?}: the refused update left the {member} alone, got {}",
                unchanged.body
            );
        }
    }
}

/// A gateway or a `rateLimitId` naming nothing refuses the change with the modelled 404 in
/// this service's own sentence — and brings no rate limit into being by being written to.
#[tokio::test]
async fn an_unknown_gateway_or_rate_limit_is_refused() {
    let router = router();
    let gateway_id = gateway(&router, "updatelimitunknown").await;
    let created = create_rate_limit(
        &router,
        &gateway_id,
        &json!({
            "rateLimitId": "held",
            "dimensionKeys": ["toolName"],
            "entries": [{"dimensions": {"toolName": "search"}, "tokens": rate(9.0, "minute")}],
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: the limit must be created, got {} ({})",
        created.status, created.body
    );
    let body = json!({
        "entries": [{"dimensions": {"toolName": "search"}, "tokens": rate(10.0, "minute")}],
    });

    // Every identifier satisfies `GatewayIdentifier`'s
    // `^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$`, so the refusal is the handler's lookup and
    // not the boundary constraint walk.
    for unknown_gateway in ["a-0123456789", "missing-gateway-abcdefghij", "z-zz00zz00zz"] {
        let refused = update_rate_limit(&router, unknown_gateway, "held", &body).await;
        assert_eq!(
            (
                refused.status,
                error_code(&refused.body),
                text(&refused.body, "message")
            ),
            (
                404,
                "ResourceNotFoundException".to_string(),
                not_found_message("gateway")
            ),
            "case {unknown_gateway:?}: expected HTTP 404 naming the gateway, got {} ({})",
            refused.status,
            refused.body
        );
        let read = get_rate_limit(&router, unknown_gateway, "held").await;
        assert_eq!(
            read.status, 404,
            "case {unknown_gateway:?}: the refused change left no gateway behind, got {} ({})",
            read.status, read.body
        );
    }

    // Every id satisfies `GatewayRateLimitId`'s 2..64 length and its
    // `^[a-zA-Z0-9][a-zA-Z0-9\-_\.]{0,62}[a-zA-Z0-9]$`: the shortest the pattern admits,
    // one carrying the punctuation it allows, and the longest.
    for unknown_limit in [
        "ab",
        "not.a_held-limit",
        "l0123456789012345678901234567890123456789012345678901234567890f",
    ] {
        let refused = update_rate_limit(&router, &gateway_id, unknown_limit, &body).await;
        assert_eq!(
            (
                refused.status,
                error_code(&refused.body),
                text(&refused.body, "message")
            ),
            (
                404,
                "ResourceNotFoundException".to_string(),
                not_found_message("rate limit")
            ),
            "case {unknown_limit:?}: expected HTTP 404 naming the rate limit, got {} ({})",
            refused.status,
            refused.body
        );
        let read = get_rate_limit(&router, &gateway_id, unknown_limit).await;
        assert_eq!(
            read.status, 404,
            "case {unknown_limit:?}: the refused change minted no rate limit, got {} ({})",
            read.status, read.body
        );
    }
}

/// Two updaters addressing one rate limit at the same time: the limit takes a change
/// whichever of them the service serves first, so a caller is never left with both of its
/// updaters rejected, and the limit ends up holding one updater's whole submission rather
/// than a mixture of the two.
#[tokio::test]
async fn concurrent_updaters_leave_one_whole_submission_behind() {
    let router = router();
    let gateway_id = gateway(&router, "updatelimitconcurrent").await;
    let created = create_rate_limit(
        &router,
        &gateway_id,
        &json!({
            "rateLimitId": "contended",
            "dimensionKeys": ["targetName"],
            "entries": [{"dimensions": {"targetName": "orders"}, "requests": rate(1.0, "second")}],
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: the limit must be created, got {} ({})",
        created.status, created.body
    );
    poll_until_status(&router, &gateway_id, "contended", "ACTIVE", "the setup").await;

    let submissions = [
        json!({
            "description": "the first updater",
            "entries": [{
                "dimensions": {"targetName": "orders"},
                "requests": rate(10.0, "second"),
            }],
        }),
        json!({
            "description": "the second updater",
            "entries": [{
                "dimensions": {"targetName": "payments"},
                "tokens": rate(20.0, "minute"),
            }],
        }),
    ];
    // Both updaters are in flight at once: neither awaits the other's answer.
    let (first, second) = tokio::join!(
        update_rate_limit(&router, &gateway_id, "contended", &submissions[0]),
        update_rate_limit(&router, &gateway_id, "contended", &submissions[1]),
    );
    assert!(
        first.status == 200 || second.status == 200,
        "at least one of two concurrent updaters is served, got {} ({}) and {} ({})",
        first.status,
        first.body,
        second.status,
        second.body
    );

    // Whatever the order was, the limit holds ONE updater's description together with that
    // same updater's rule entries.
    let settled = poll_until_status(
        &router,
        &gateway_id,
        "contended",
        "ACTIVE",
        "after the contention",
    )
    .await;
    let held = submissions.iter().find(|submission| {
        settled.body.get("description") == submission.get("description")
            && settled.body.get("entries") == submission.get("entries")
    });
    assert!(
        held.is_some(),
        "the limit holds one updater's whole submission, got {}",
        settled.body
    );
}
