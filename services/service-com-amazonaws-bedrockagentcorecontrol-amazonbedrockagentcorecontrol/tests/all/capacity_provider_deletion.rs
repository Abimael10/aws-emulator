// SPDX-License-Identifier: CC0-1.0

//! `DeleteCapacityProvider` end to end over HTTP.
//!
//! Two rules are under test:
//!
//! 1. A capacity provider any stored agent-runtime VERSION is still configured with is NOT
//!    deleted: "Before you delete a capacity provider, disassociate all agent runtimes and
//!    runtime versions that reference it. If any references remain, the operation fails"
//!    (`smithy_query(query="doc", shape="DeleteCapacityProvider")`). Such a request is
//!    refused HTTP 400 `ValidationException` and writes nothing, so the provider is still
//!    readable in the status it had.
//! 2. A provider no version references is ACCEPTED (HTTP 202, `status` `DELETING` —
//!    `smithy_query(query="operation", operation="DeleteCapacityProvider")` binds the
//!    deletion to a 202 carrying a `CapacityProviderStatus`), stays readable in `DELETING`,
//!    and is gone — the modelled 404 — once the removal the clock owes it comes due.
//!
//! The rows differ along what the rules generalise over: how many versions reference the
//! provider and whether they belong to one runtime or several, how the last reference went
//! away (a version deleted on its own, or the whole runtime deleted and its removal come
//! due), whether the delete carries a `clientToken`, how many times it is repeated, and the
//! shape of an id that names no provider at all.
//!
//! Every wait here waits for the CONDITION, polling until it holds or a generous deadline
//! passes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region every request here signs for.
const REGION: &str = "us-east-1";

/// A role ARN inside `RoleArn`'s pattern. No IAM is mounted behind this engine, so nothing is
/// known about it and no create is refused on an assumption.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/AgentCoreRole";

/// How long a poll for a clock-owed removal keeps looking, and how long it waits between
/// looks. Generous: the condition is what these tests wait for, not a duration.
const REMOVAL_DEADLINE: Duration = Duration::from_secs(60);
const REMOVAL_POLL: Duration = Duration::from_millis(50);

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

/// A compute configuration every value of which satisfies its own modelled constraint
/// (`smithy_query(query="closure", operation="CreateCapacityProvider")`).
fn compute_configuration() -> Value {
    json!({
        "ec2Configuration": {
            "launchTemplateSource": {
                "launchParameters": {
                    "operatingSystem": "LINUX_X86_64",
                    "instanceRequirements": {"allowedInstanceTypes": ["m5.large"]}
                }
            },
            "vpcConfiguration": {
                "subnets": ["subnet-0123456789abcdef0"],
                "securityGroups": ["sg-0123456789abcdef0"]
            },
            "lifecycleConfiguration": {"idleInstanceTimeout": 900}
        }
    })
}

/// The provider a create mints: the id every route addresses it by, and the ARN a runtime
/// version is associated with it through.
struct Provider {
    id: String,
    arn: String,
}

async fn create_provider(router: &MultiServiceRouter, name: &str) -> Provider {
    let (status, created) = send(
        router,
        "PUT",
        "/capacity-providers",
        Some(&json!({
            "name": name,
            "permissionsConfiguration": {"capacityProviderOperatorRoleArn":
                "arn:aws:iam::000000000000:role/AgentCoreCapacityProvider"},
            "computeConfiguration": compute_configuration()
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup {name:?}: CreateCapacityProvider must succeed, got {status} ({created})"
    );
    Provider {
        id: text(&created, "capacityProviderId"),
        arn: text(&created, "capacityProviderArn"),
    }
}

/// A `CreateAgentRuntime` body, optionally configured with a capacity provider by its ARN.
fn runtime_body(name: &str, capacity_provider_arn: Option<&str>) -> Value {
    let mut body = json!({
        "agentRuntimeName": name,
        "agentRuntimeArtifact": {"containerConfiguration": {
            "containerUri": "public.ecr.aws/agentcore/demo:v1"
        }},
        "networkConfiguration": {"networkMode": "PUBLIC"},
        "roleArn": ROLE_ARN,
    });
    if let Some(arn) = capacity_provider_arn {
        body["capacityProviderConfiguration"] = json!({"capacityProviderArn": arn});
    }
    body
}

/// Create a runtime whose first version is configured with `capacity_provider_arn`, and
/// answer with its `agentRuntimeId`.
async fn create_runtime(
    router: &MultiServiceRouter,
    name: &str,
    capacity_provider_arn: &str,
) -> String {
    let (status, created) = send(
        router,
        "PUT",
        "/runtimes/",
        Some(&runtime_body(name, Some(capacity_provider_arn))),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup {name:?}: CreateAgentRuntime must succeed, got {status} ({created})"
    );
    text(&created, "agentRuntimeId")
}

/// Publish another version of `runtime_id`, configured with `capacity_provider_arn` when one
/// is given, and answer with the version it published.
async fn publish_version(
    router: &MultiServiceRouter,
    runtime_id: &str,
    capacity_provider_arn: Option<&str>,
) -> String {
    let mut body = json!({
        "agentRuntimeArtifact": {"containerConfiguration": {
            "containerUri": "public.ecr.aws/agentcore/demo:v2"
        }},
        "roleArn": ROLE_ARN,
    });
    if let Some(arn) = capacity_provider_arn {
        body["capacityProviderConfiguration"] = json!({"capacityProviderArn": arn});
    }
    let (status, updated) = send(
        router,
        "PUT",
        &format!("/runtimes/{runtime_id}/"),
        Some(&body),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup {runtime_id}: UpdateAgentRuntime must publish a version, got {status} \
         ({updated})"
    );
    text(&updated, "agentRuntimeVersion")
}

async fn delete_provider(router: &MultiServiceRouter, id: &str, query: &str) -> (u16, Value) {
    send(
        router,
        "DELETE",
        &format!("/capacity-providers/{id}{query}"),
        None,
    )
    .await
}

async fn get_provider(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    send(router, "GET", &format!("/capacity-providers/{id}"), None).await
}

/// The sentence this family refuses the delete of a referenced provider with.
fn in_use_message(id: &str) -> String {
    format!(
        "Cannot delete capacity provider '{id}' because it still has attached agent runtime \
         versions. Delete all attached agent runtime versions before deleting the capacity \
         provider."
    )
}

/// The sentence every read of a capacity provider this account does not hold answers with.
fn not_found_message(id: &str) -> String {
    format!("No capacity provider found with id {id}")
}

/// Poll `GetCapacityProvider` until the provider is gone, and answer with the body of the
/// answer that reported it gone.
async fn poll_until_removed(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let started = Instant::now();
    loop {
        let (status, body) = get_provider(router, id).await;
        if status == 404 {
            return body;
        }
        assert!(
            started.elapsed() < REMOVAL_DEADLINE,
            "case {case:?}: the accepted deletion's removal never came due; the last read \
             answered {status} ({body})"
        );
        tokio::time::sleep(REMOVAL_POLL).await;
    }
}

/// RULE: while any stored agent-runtime version is configured with the provider, the delete
/// is refused 400 `ValidationException` and nothing is written.
#[tokio::test]
async fn a_referenced_provider_is_not_deleted() {
    // (case, how many runtimes reference the provider, how many versions each of them puts
    // on it) — the rule generalises over the NUMBER of remaining references and over
    // whether they belong to one runtime or several.
    let rows: [(&str, usize, usize); 4] = [
        ("one runtime on one version", 1, 1),
        ("one runtime on two of its versions", 1, 2),
        ("two runtimes on one version each", 2, 1),
        ("three runtimes on two versions each", 3, 2),
    ];

    for (case, runtimes, versions) in rows {
        // One router per row, so the account each row starts from holds nothing.
        let router = router();
        let provider = create_provider(&router, "referencedProvider").await;
        for runtime in 0..runtimes {
            let name = format!("referencing_runtime_{runtime}");
            let runtime_id = create_runtime(&router, &name, &provider.arn).await;
            for _ in 1..versions {
                publish_version(&router, &runtime_id, Some(&provider.arn)).await;
            }
        }

        let (status, refused) = delete_provider(&router, &provider.id, "").await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {case:?}: a referenced provider's delete is refused 400 ValidationException, \
             got {status} {} ({refused})",
            error_code(&refused)
        );
        assert_eq!(
            text(&refused, "message"),
            in_use_message(&provider.id),
            "case {case:?}: expected {:?}, got {:?}",
            in_use_message(&provider.id),
            text(&refused, "message")
        );
        assert_eq!(
            text(&refused, "reason"),
            "ResourceConflict",
            "case {case:?}: nothing is wrong with the request's members, so the refusal \
             reports a resource conflict, got {refused}"
        );

        // Nothing was written: the provider is still readable, and NOT on its way out.
        let (status, fetched) = get_provider(&router, &provider.id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the refused delete left the provider readable, got {status} \
             ({fetched})"
        );
        assert_ne!(
            text(&fetched, "status"),
            "DELETING",
            "case {case:?}: the refused delete started no deletion, got {fetched}"
        );
    }
}

/// RULE: a provider no version references is accepted (202, `DELETING`), stays readable in
/// `DELETING`, and is gone once the clock owes its removal — however the last reference went
/// away.
#[tokio::test]
async fn a_provider_nothing_references_is_deleted() {
    // (case, how the provider ends up with no reference, whether the delete carries a
    // clientToken)
    //
    // `ClientToken` is @length 33..256 with pattern `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`.
    let token = "d".repeat(33);
    let rows: [(&str, &str, &str); 3] = [
        ("a provider no runtime ever named", "never-referenced", ""),
        (
            "a provider whose only referencing version was deleted",
            "version-deleted",
            "",
        ),
        (
            "a provider whose referencing runtime was deleted",
            "runtime-deleted",
            "token",
        ),
    ];

    for (case, how, carries_token) in rows {
        let router = router();
        let provider = create_provider(&router, "deletableProvider").await;
        match how {
            "never-referenced" => {
                // Another provider carries the references, so this one is not the only
                // provider the account holds.
                let other = create_provider(&router, "otherProvider").await;
                create_runtime(&router, "other_runtime", &other.arn).await;
            }
            "version-deleted" => {
                // A runtime whose first version names this provider and whose second names
                // another: deleting the first version removes the only reference at once,
                // while the runtime itself stays.
                let other = create_provider(&router, "otherProvider").await;
                let runtime_id = create_runtime(&router, "moved_runtime", &provider.arn).await;
                publish_version(&router, &runtime_id, Some(&other.arn)).await;
                let (status, deleted) = send(
                    &router,
                    "DELETE",
                    &format!("/runtimes/{runtime_id}/?version=1"),
                    None,
                )
                .await;
                assert_eq!(
                    status, 202,
                    "case {case:?}: deleting the referencing version must succeed, got \
                     {status} ({deleted})"
                );
            }
            _ => {
                // The whole runtime goes; its removal is owed by the clock, so the provider's
                // delete is polled until the reference is gone.
                let runtime_id = create_runtime(&router, "doomed_runtime", &provider.arn).await;
                let (status, deleted) = send(
                    &router,
                    "DELETE",
                    &format!("/runtimes/{runtime_id}/"),
                    None,
                )
                .await;
                assert_eq!(
                    status, 202,
                    "case {case:?}: deleting the referencing runtime must succeed, got \
                     {status} ({deleted})"
                );
            }
        }

        let query = if carries_token == "token" {
            format!("?clientToken={token}")
        } else {
            String::new()
        };

        // Poll the delete until the reference is gone: a runtime still on its way out still
        // references the provider, and that refusal is rule 1's.
        let started = Instant::now();
        let (status, accepted) = loop {
            let answer = delete_provider(&router, &provider.id, &query).await;
            if answer.0 != 400 {
                break answer;
            }
            assert!(
                started.elapsed() < REMOVAL_DEADLINE,
                "case {case:?}: the last reference never went away; the last delete answered \
                 {} ({})",
                answer.0,
                answer.1
            );
            tokio::time::sleep(REMOVAL_POLL).await;
        };
        assert_eq!(
            status, 202,
            "case {case:?}: an unreferenced provider's delete is accepted 202, got {status} \
             ({accepted})"
        );
        assert_eq!(
            (
                text(&accepted, "capacityProviderId"),
                text(&accepted, "status")
            ),
            (provider.id.clone(), "DELETING".to_string()),
            "case {case:?}: the accepted delete reports the provider it addressed, going \
             away, got {accepted}"
        );

        // A caller acting straight after the delete still sees the provider, DELETING.
        let (status, fetched) = get_provider(&router, &provider.id).await;
        assert_eq!(
            (status, text(&fetched, "status")),
            (200, "DELETING".to_string()),
            "case {case:?}: the provider stays readable while the deletion is under way, got \
             {status} ({fetched})"
        );
        let (status, listed) = send(&router, "POST", "/capacity-providers", None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListCapacityProviders must answer, got {status} ({listed})"
        );
        let listed_status = listed["capacityProviders"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .find(|summary| text(summary, "capacityProviderId") == provider.id)
            .map(|summary| text(&summary, "status"));
        assert_eq!(
            listed_status,
            Some("DELETING".to_string()),
            "case {case:?}: the listing and the read report the same provider, got {listed}"
        );

        // And once the removal the clock owes comes due, every read is the modelled 404.
        let gone = poll_until_removed(&router, case, &provider.id).await;
        assert_eq!(
            (error_code(&gone).as_str(), text(&gone, "message")),
            ("ResourceNotFoundException", not_found_message(&provider.id)),
            "case {case:?}: a removed provider is reported missing in this family's own \
             sentence, got {gone}"
        );
        let (_, listed) = send(&router, "POST", "/capacity-providers", None).await;
        assert!(
            !listed["capacityProviders"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .any(|summary| text(summary, "capacityProviderId") == provider.id),
            "case {case:?}: a removed provider is listed no more, got {listed}"
        );
    }
}

/// The delete is `@idempotent` (`smithy_query(query="operation",
/// operation="DeleteCapacityProvider")`): a repeat reports the same provider going away and
/// does not push its removal further out — whether or not it carries the token the first one
/// did.
#[tokio::test]
async fn a_repeated_delete_reports_the_same_provider_going_away() {
    let router = router();
    let provider = create_provider(&router, "idempotentDelete").await;
    let token = "e".repeat(33);

    // (case, the query the delete carries)
    let rows: [(&str, String); 3] = [
        ("the first delete, with a token", format!("?clientToken={token}")),
        ("the same delete replayed with that token", format!("?clientToken={token}")),
        ("a repeat carrying no token at all", String::new()),
    ];

    for (case, query) in &rows {
        let (status, accepted) = delete_provider(&router, &provider.id, query).await;
        assert_eq!(
            (status, text(&accepted, "capacityProviderId"), text(&accepted, "status")),
            (202, provider.id.clone(), "DELETING".to_string()),
            "case {case:?}: expected the same provider going away, got {status} ({accepted})"
        );
    }

    // The repeats did not re-arm the removal: it still comes due, within the deadline.
    let gone = poll_until_removed(&router, "the repeated delete", &provider.id).await;
    assert_eq!(
        error_code(&gone),
        "ResourceNotFoundException",
        "case \"the repeated delete\": the removal still came due, got {gone}"
    );
}

/// An id naming no provider this account holds is the modelled 404 — for ids that differ in
/// which part of a well-formed `CapacityProviderId` they carry, and for one whose provider
/// this suite already deleted.
#[tokio::test]
async fn deleting_an_id_naming_no_provider_is_not_found() {
    let router = router();
    let held = create_provider(&router, "heldProvider").await;

    // (case, the id the delete names) — each inside `CapacityProviderId`'s own format
    // (@length 12..59, pattern `^[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}$`).
    let rows: [(&str, String); 3] = [
        (
            "a name this account never created",
            "otherProvider-abcdefghij".to_string(),
        ),
        (
            "the held provider's name with another suffix",
            "heldProvider-zzzzzzzzzz".to_string(),
        ),
        (
            "an id at the member's minimum length",
            "a-bcdefghijk".to_string(),
        ),
    ];

    for (case, id) in &rows {
        let (status, refused) = delete_provider(&router, id, "").await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?} ({id}): expected HTTP 404 ResourceNotFoundException, got {status} \
             {} ({refused})",
            error_code(&refused)
        );
        assert_eq!(
            text(&refused, "message"),
            not_found_message(id),
            "case {case:?}: expected {:?}, got {:?}",
            not_found_message(id),
            text(&refused, "message")
        );
    }

    // The provider the create minted is still deletable, so the refusals above are about the
    // id and not about the route.
    let (status, accepted) = delete_provider(&router, &held.id, "").await;
    assert_eq!(
        status, 202,
        "case \"the held provider\": its own delete is accepted, got {status} ({accepted})"
    );
    // And once it is gone, its own id answers the same 404 the ids above did.
    poll_until_removed(&router, "the held provider", &held.id).await;
    let (status, refused) = delete_provider(&router, &held.id, "").await;
    assert_eq!(
        (status, text(&refused, "message")),
        (404, not_found_message(&held.id)),
        "case \"a provider removed after its deletion\": expected the modelled 404, got \
         {status} ({refused})"
    );
}
