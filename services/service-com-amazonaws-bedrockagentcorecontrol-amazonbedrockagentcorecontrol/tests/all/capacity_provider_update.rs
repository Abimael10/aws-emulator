// SPDX-License-Identifier: CC0-1.0

//! `UpdateCapacityProvider` end to end over HTTP: the description it patches, the members it
//! leaves alone, the `UPDATING` -> `READY` transition the accepted change owes, and the
//! refusals a change asked of the wrong provider gets.
//!
//! Every row drives the operations as a client does — one shared engine behind one router —
//! so an update and the reads that follow it are asserted against the same store rather than
//! against each other's envelopes.
//!
//! Sources: `smithy_query(query="operation",
//! operation="UpdateCapacityProvider,GetCapacityProvider")` for every member and status
//! asserted here, `smithy_query(query="doc", shape="UpdatedDescription")` for the PATCH rule
//! the `description` wrapper carries ("When present in an update request, the description is
//! replaced with optionalValue. When absent, the description is left unchanged. To unset the
//! description, include the wrapper with optionalValue not specified"), the operation's own
//! documentation for what an update may change ("Only the description can be changed. To
//! change other configuration, such as instance types, networking, or storage, create a new
//! capacity provider") and `smithy_query(query="shape", shape="CapacityProviderStatus")` for
//! the statuses a provider reports.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for, which the ARNs they assert carry.
const REGION: &str = "us-east-1";

/// How long a poll for a clock-owed status keeps looking, and how long it waits between
/// looks. Generous: the condition is what the test waits for, not a duration.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const SETTLE_POLL: Duration = Duration::from_millis(50);

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
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
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

async fn create_provider(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    send(router, "PUT", "/capacity-providers", Some(body)).await
}

async fn update_provider(router: &MultiServiceRouter, id: &str, body: &Value) -> (u16, Value) {
    send(
        router,
        "PUT",
        &format!("/capacity-providers/{id}"),
        Some(body),
    )
    .await
}

async fn get_provider(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    send(router, "GET", &format!("/capacity-providers/{id}"), None).await
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The error code `restJson1` puts in `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The compute configuration a provider is created with, as one union member carrying the
/// EC2 configuration — every value inside its own modelled constraint.
fn compute_configuration() -> Value {
    json!({
        "ec2Configuration": {
            "launchTemplateSource": {
                "launchParameters": {
                    "operatingSystem": "LINUX_X86_64",
                    "instanceRequirements": {"allowedInstanceTypes": ["m5.large"]},
                    "monitoring": "DETAILED"
                }
            },
            "vpcConfiguration": {
                "subnets": ["subnet-0123456789abcdef0"],
                "securityGroups": ["sg-0123456789abcdef0"]
            },
            "lifecycleConfiguration": {"idleInstanceTimeout": 900},
            "rootVolume": {"volumeType": "gp3", "iops": 3000, "throughput": 125}
        }
    })
}

fn permissions_configuration() -> Value {
    json!({"capacityProviderOperatorRoleArn":
        "arn:aws:iam::000000000000:role/AgentCoreCapacityProvider"})
}

/// The body of a create, with or without a description.
fn create_body(name: &str, description: Option<&str>) -> Value {
    let mut body = json!({
        "name": name,
        "permissionsConfiguration": permissions_configuration(),
        "computeConfiguration": compute_configuration()
    });
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    body
}

/// Create one provider and answer with its id, failing the case loudly when the create does
/// not serve.
async fn created_id(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    description: Option<&str>,
) -> String {
    let (status, created) = create_provider(router, &create_body(name, description)).await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected HTTP 202 from CreateCapacityProvider, got {status} ({created})"
    );
    text(&created, "capacityProviderId")
}

/// Poll the read of `id` until it reports `wanted`, answering with the last envelope seen.
/// The CONDITION is what this waits for; the deadline is generous and only bounds a failure.
async fn poll_until_status(router: &MultiServiceRouter, id: &str, wanted: &str) -> Value {
    let started = Instant::now();
    let (_, mut last) = get_provider(router, id).await;
    while started.elapsed() < SETTLE_DEADLINE && text(&last, "status") != wanted {
        tokio::time::sleep(SETTLE_POLL).await;
        let (_, fresh) = get_provider(router, id).await;
        last = fresh;
    }
    assert_eq!(
        text(&last, "status"),
        wanted,
        "the provider {id} reaches {wanted}, got {last}"
    );
    last
}

/// One row of the PATCH table: what the create carried, what the update's `description`
/// member carries, and the description the provider holds afterwards.
struct PatchRow {
    case: &'static str,
    name: &'static str,
    created: Option<String>,
    /// The update request's `description` member, or `None` for a request that omits it.
    sent: Option<Value>,
    expected: Option<String>,
}

/// The `description` wrapper's PATCH rule, over updates that differ in whether the member is
/// sent at all, whether it carries a value, the length of that value (both published bounds
/// of `Description`, @length 1..4096) and whether the provider had a description to begin
/// with. Everything else about the provider is left as the create persisted it.
#[tokio::test]
async fn an_update_patches_only_the_description() {
    let router = router();
    let rows = vec![
        PatchRow {
            case: "an omitted member leaves the stored description alone",
            name: "untouchedProvider",
            created: Some("the description the create carried".to_string()),
            sent: None,
            expected: Some("the description the create carried".to_string()),
        },
        PatchRow {
            case: "a wrapper carrying a value replaces the description",
            name: "replacedProvider",
            created: Some("the description the create carried".to_string()),
            sent: Some(json!({"optionalValue": "the description the update carries"})),
            expected: Some("the description the update carries".to_string()),
        },
        PatchRow {
            case: "a wrapper with no value clears the description",
            name: "clearedProvider",
            created: Some("the description the create carried".to_string()),
            sent: Some(json!({})),
            expected: None,
        },
        PatchRow {
            case: "a wrapper carrying a value describes a provider created without one",
            name: "describedLaterProvider",
            created: None,
            sent: Some(json!({"optionalValue": "described after the fact"})),
            expected: Some("described after the fact".to_string()),
        },
        PatchRow {
            case: "a wrapper with no value on a provider that had none stays undescribed",
            name: "stillUndescribedProvider",
            created: None,
            sent: Some(json!({})),
            expected: None,
        },
        PatchRow {
            // `Description` is @length 1..4096: both published bounds are rows of their own.
            case: "a value at the member's maximum length",
            name: "longestDescriptionProvider",
            created: Some("short".to_string()),
            sent: Some(json!({"optionalValue": "d".repeat(4096)})),
            expected: Some("d".repeat(4096)),
        },
        PatchRow {
            case: "a value at the member's minimum length",
            name: "shortestDescriptionProvider",
            created: None,
            sent: Some(json!({"optionalValue": "d"})),
            expected: Some("d".to_string()),
        },
    ];

    // Create every provider first, then wait for the one transition they are all owed: the
    // span runs from each create, so the waits overlap rather than add up.
    let mut ids: Vec<String> = Vec::new();
    for row in &rows {
        ids.push(created_id(&router, row.case, row.name, row.created.as_deref()).await);
    }
    let mut before: Vec<Value> = Vec::new();
    for id in &ids {
        before.push(poll_until_status(&router, id, "READY").await);
    }

    for ((row, id), settled) in rows.iter().zip(&ids).zip(&before) {
        let case = row.case;
        let mut body = json!({});
        if let Some(sent) = &row.sent {
            body["description"] = sent.clone();
        }
        let (status, updated) = update_provider(&router, id, &body).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202 from UpdateCapacityProvider, got {status} \
             ({updated})"
        );
        assert_eq!(
            (
                text(&updated, "capacityProviderId"),
                text(&updated, "capacityProviderArn"),
                text(&updated, "name"),
                text(&updated, "status")
            ),
            (
                id.clone(),
                text(settled, "capacityProviderArn"),
                row.name.to_string(),
                "UPDATING".to_string()
            ),
            "case {case:?}: the update reports the provider it changed and the status it \
             enters, got {updated}"
        );
        assert_eq!(
            updated.get("createdAt"),
            settled.get("createdAt"),
            "case {case:?}: an update does not change when the provider was created, got \
             {updated}"
        );
        assert!(
            updated.get("lastUpdatedAt").is_some(),
            "case {case:?}: the update reports lastUpdatedAt, got {updated}"
        );

        let (status, fetched) = get_provider(&router, id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetCapacityProvider, got {status} ({fetched})"
        );
        assert_eq!(
            fetched.get("description").and_then(Value::as_str),
            row.expected.as_deref(),
            "case {case:?}: expected the description {:?} the PATCH rule leaves, got {fetched}",
            row.expected
        );
        assert_eq!(
            (
                fetched.get("computeConfiguration"),
                fetched.get("permissionsConfiguration"),
                fetched.get("createdAt")
            ),
            (
                settled.get("computeConfiguration"),
                settled.get("permissionsConfiguration"),
                settled.get("createdAt")
            ),
            "case {case:?}: an update changes nothing but the description, got {fetched}"
        );
        assert_eq!(
            (
                text(&fetched, "capacityProviderId"),
                text(&fetched, "name"),
                text(&fetched, "status"),
                fetched.get("lastUpdatedAt"),
            ),
            (
                id.clone(),
                row.name.to_string(),
                "UPDATING".to_string(),
                updated.get("lastUpdatedAt"),
            ),
            "case {case:?}: the read reports the provider the update wrote, got {fetched}"
        );
    }
}

/// The transition an accepted update owes: the provider reports `UPDATING` to a caller acting
/// straight after the call, and `READY` once the clock owes it — with the description the
/// update left on it, over updates that set and that clear one.
#[tokio::test]
async fn an_accepted_update_settles_from_updating_to_ready() {
    // (case, the description member the update sends, the description afterwards)
    let rows: [(&str, Value, Option<&str>); 2] = [
        (
            "an update that sets a description",
            json!({"optionalValue": "the description the update carries"}),
            Some("the description the update carries"),
        ),
        ("an update that clears it", json!({}), None),
    ];

    for (case, sent, expected) in rows {
        let router = router();
        let id = created_id(&router, case, "settlingProvider", Some("the first description")).await;
        poll_until_status(&router, &id, "READY").await;

        let (status, updated) = update_provider(&router, &id, &json!({"description": sent})).await;
        assert_eq!(
            (status, text(&updated, "status").as_str()),
            (202, "UPDATING"),
            "case {case:?}: an accepted update enters UPDATING, got {status} ({updated})"
        );
        let (_, fetched) = get_provider(&router, &id).await;
        assert_eq!(
            text(&fetched, "status"),
            "UPDATING",
            "case {case:?}: a caller acting straight after the update still sees the transient \
             status, got {fetched}"
        );

        let settled = poll_until_status(&router, &id, "READY").await;
        assert_eq!(
            settled.get("description").and_then(Value::as_str),
            expected,
            "case {case:?}: the settled provider carries the description the update left, got \
             {settled}"
        );
    }
}

/// Several updates in flight at once: each one is accepted, and the LAST write is the
/// description the provider carries — once while the transition is still owed and again after
/// it. The rows differ in how many updates are sent and in what the last one does.
#[tokio::test]
async fn the_last_of_several_updates_is_the_one_the_provider_carries() {
    // (case, the `description` member each update sends in order, the description afterwards)
    let rows: [(&str, Vec<Value>, Option<&str>); 3] = [
        (
            "two updates",
            vec![
                json!({"optionalValue": "the first write"}),
                json!({"optionalValue": "the last write"}),
            ],
            Some("the last write"),
        ),
        (
            "four updates",
            vec![
                json!({"optionalValue": "one"}),
                json!({"optionalValue": "two"}),
                json!({"optionalValue": "three"}),
                json!({"optionalValue": "the last write"}),
            ],
            Some("the last write"),
        ),
        (
            "a last write that clears the description",
            vec![
                json!({"optionalValue": "the first write"}),
                json!({}),
            ],
            None,
        ),
    ];

    for (case, writes, expected) in rows {
        let router = router();
        let id = created_id(&router, case, "contendedProvider", None).await;
        poll_until_status(&router, &id, "READY").await;

        for (attempt, sent) in writes.iter().enumerate() {
            let (status, updated) =
                update_provider(&router, &id, &json!({"description": sent})).await;
            assert_eq!(
                (status, text(&updated, "status").as_str()),
                (202, "UPDATING"),
                "case {case:?} / write {attempt}: every update of the provider is accepted, got \
                 {status} ({updated})"
            );
        }

        let (_, fetched) = get_provider(&router, &id).await;
        assert_eq!(
            (
                fetched.get("description").and_then(Value::as_str),
                text(&fetched, "status").as_str()
            ),
            (expected, "UPDATING"),
            "case {case:?}: the provider carries the last write while the transition is still \
             owed, got {fetched}"
        );

        let settled = poll_until_status(&router, &id, "READY").await;
        assert_eq!(
            settled.get("description").and_then(Value::as_str),
            expected,
            "case {case:?}: the settled provider still carries the last write, got {settled}"
        );
    }
}

/// A replayed `clientToken` answers with the provider that token first updated and applies no
/// second change: "If this token matches a previous request, the service ignores the request,
/// but doesn't return an error."
#[tokio::test]
async fn a_repeated_client_token_applies_the_update_once() {
    let router = router();
    let case = "the replayed token";
    let id = created_id(&router, case, "idempotentUpdateProvider", Some("the create's")).await;
    poll_until_status(&router, &id, "READY").await;

    // `ClientToken` is @length 33..256 with pattern `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`.
    let token = "b".repeat(33);
    let (status, first) = update_provider(
        &router,
        &id,
        &json!({"clientToken": token, "description": {"optionalValue": "the first update's"}}),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected HTTP 202 for the first update, got {status} ({first})"
    );

    let (status, replayed) = update_provider(
        &router,
        &id,
        &json!({"clientToken": token, "description": {"optionalValue": "the second update's"}}),
    )
    .await;
    assert_eq!(
        (
            status,
            text(&replayed, "capacityProviderId"),
            text(&replayed, "status")
        ),
        (202, id.clone(), "UPDATING".to_string()),
        "case {case:?}: the replay answers for the provider the token first updated, got \
         {status} ({replayed})"
    );

    let (_, fetched) = get_provider(&router, &id).await;
    assert_eq!(
        fetched.get("description").and_then(Value::as_str),
        Some("the first update's"),
        "case {case:?}: the replayed request applied no second change, got {fetched}"
    );

    // The token is idempotent for the request it replays: aimed at ANOTHER provider it is a
    // different request, so that provider's own update is applied and reported.
    let case = "the same token against another provider";
    let other = created_id(&router, case, "otherUpdateProvider", Some("the create's")).await;
    poll_until_status(&router, &other, "READY").await;
    let (status, updated) = update_provider(
        &router,
        &other,
        &json!({"clientToken": token, "description": {"optionalValue": "the other provider's"}}),
    )
    .await;
    assert_eq!(
        (status, text(&updated, "capacityProviderId")),
        (202, other.clone()),
        "case {case:?}: the update answers for the provider the request names, got {status} \
         ({updated})"
    );
    let (_, fetched) = get_provider(&router, &other).await;
    assert_eq!(
        fetched.get("description").and_then(Value::as_str),
        Some("the other provider's"),
        "case {case:?}: the change is applied to the provider the request names, got {fetched}"
    );
    let (_, first_provider) = get_provider(&router, &id).await;
    assert_eq!(
        first_provider.get("description").and_then(Value::as_str),
        Some("the first update's"),
        "case {case:?}: the provider the token first updated is untouched, got {first_provider}"
    );
}

/// An id naming no provider this account holds is the modelled 404, for ids that differ only
/// in which part of a well-formed id they carry, and nothing is written for one.
#[tokio::test]
async fn an_update_naming_no_provider_is_not_found() {
    let router = router();
    let held = created_id(&router, "the create", "heldProvider", Some("the create's")).await;

    // (case, the id the update names) — each inside `CapacityProviderId`'s own format,
    // `^[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}$`, @length 12..59.
    let rows: [(&str, String); 3] = [
        (
            "a name this account never created",
            "otherProvider-abcdefghij".to_string(),
        ),
        (
            "the held provider's name with another suffix",
            format!("heldProvider-{}", "z".repeat(10)),
        ),
        (
            "an id at the member's minimum length",
            "a-bcdefghijk".to_string(),
        ),
    ];

    for (case, id) in rows {
        let (status, refused) = update_provider(
            &router,
            &id,
            &json!({"description": {"optionalValue": "a change nothing takes"}}),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?} ({id}): expected HTTP 404 ResourceNotFoundException, got {status} {} \
             ({refused})",
            error_code(&refused)
        );
        assert_eq!(
            text(&refused, "message"),
            format!("No capacity provider found with id {id}"),
            "case {case:?}: got {refused}"
        );
    }

    let (status, fetched) = get_provider(&router, &held).await;
    assert_eq!(
        (status, fetched.get("description").and_then(Value::as_str)),
        (200, Some("the create's")),
        "case \"the held provider\": the refusals wrote nothing, got {status} ({fetched})"
    );
}

/// A provider whose own provisioning is still under way takes no change: the request collides
/// with what the provider is already doing, so it is the modelled 409 and the stored provider
/// is left exactly as it was — and the same update serves once the provider is `READY`.
#[tokio::test]
async fn an_update_of_a_provider_that_is_still_creating_is_refused() {
    let router = router();
    let case = "an update while the create is still under way";
    let id = created_id(&router, case, "creatingProvider", Some("the create's")).await;

    let body = json!({"description": {"optionalValue": "a change the provider cannot take yet"}});
    let (status, refused) = update_provider(&router, &id, &body).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (409, "ConflictException"),
        "case {case:?}: expected HTTP 409 ConflictException, got {status} {} ({refused})",
        error_code(&refused)
    );
    assert_eq!(
        text(&refused, "message"),
        format!(
            "Capacity provider {id} is in CREATING status and cannot be modified. Retry the \
             request once its status is READY."
        ),
        "case {case:?}: got {refused}"
    );
    let (_, fetched) = get_provider(&router, &id).await;
    assert_eq!(
        (
            fetched.get("description").and_then(Value::as_str),
            text(&fetched, "status").as_str()
        ),
        (Some("the create's"), "CREATING"),
        "case {case:?}: the refusal wrote nothing, got {fetched}"
    );

    // The same request serves once the provider is available for use.
    poll_until_status(&router, &id, "READY").await;
    let (status, updated) = update_provider(&router, &id, &body).await;
    assert_eq!(
        (status, text(&updated, "status").as_str()),
        (202, "UPDATING"),
        "case \"the same update once the provider is READY\": got {status} ({updated})"
    );
    let (_, fetched) = get_provider(&router, &id).await;
    assert_eq!(
        fetched.get("description").and_then(Value::as_str),
        Some("a change the provider cannot take yet"),
        "case \"the same update once the provider is READY\": the change is applied, got \
         {fetched}"
    );
}
