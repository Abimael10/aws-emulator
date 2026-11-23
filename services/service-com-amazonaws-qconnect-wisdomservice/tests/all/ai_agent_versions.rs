// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Amazon Q in Connect AI agent VERSION plane
//! (`CreateAIAgentVersion`, `ListAIAgentVersions`, `DeleteAIAgentVersion`, the
//! version-addressed read of `GetAIAgent`, and the `UpdateAssistantAIAgent` binding that
//! names a version).
//!
//! Every assertion goes over the wire, through the same [`Emulator`] harness the rest of
//! this crate's suites drive the production router with: only the HTTP status, the
//! `x-amzn-errortype` header and the JSON body are inspected, so a response that is not
//! computed from persisted state cannot pass.
//!
//! The rule under test, asserted over a TABLE of inputs rather than one recorded request:
//! publishing a version of a versioned resource mints an IMMUTABLE, numbered snapshot of
//! that resource's current working copy — the first version of a resource is 1 and each
//! later publish is the previous number plus 1, the snapshot is addressable as
//! `<id>:<versionNumber>` and reports exactly what was published under it however often the
//! working copy changes afterwards, the listing reports the versions still held in
//! ascending order, and a delete removes one whatever still NAMES it. What a version reports
//! for the agent's TAGS is `ai_agent_version_tags`'s own suite.
//!
//! The rows differ along every axis the rule generalises over:
//!
//! * which `AIAgentType` the agent is, and so which configuration union member it carries;
//! * its `visibilityStatus`, and whether it carries a description at all;
//! * which member a later update changes — the description, the visibility status or the
//!   configuration;
//! * how many versions the agent has published, which ones a delete has released, and how
//!   the listing is paged and filtered;
//! * whether the publish states a `modifiedTime` precondition, and whether that precondition
//!   still holds.
//!
//! Beside the round trip it pins the refusals: an assistant this account does not hold, an
//! agent that assistant does not hold, an agent of ANOTHER assistant, a label that names a
//! published version where the working copy is what publishes, one of the SYSTEM agents the
//! service provisions, a version number nothing published, and a `nextToken` this service
//! did not mint.

use serde_json::{json, Value};

use super::ai_agent::{
    assistant_id, create_agent, full_configuration, get_agent, MESSAGE_AI_AGENT_NOT_FOUND,
    MESSAGE_ASSISTANT_NOT_FOUND, ORIGIN_CUSTOMER, QUALIFIER_LATEST, STATUS_ACTIVE,
    VISIBILITY_PUBLISHED, VISIBILITY_SAVED,
};
use super::ai_agent_update::update_agent;
use super::resource_tags::{label, Emulator, Reply, ABSENT_UUID};
use service_com_amazonaws_qconnect_wisdomservice::state::MAX_AI_AGENT_VERSIONS_PER_AGENT;

/// The modelled error shapes this surface refuses with, as the wire reports them.
const CODE_NOT_FOUND: &str = "ResourceNotFoundException";
const CODE_VALIDATION: &str = "ValidationException";
const CODE_CONFLICT: &str = "ConflictException";
const CODE_QUOTA: &str = "ServiceQuotaExceededException";

/// The sentences this endpoint refuses the version surface with. Written out here rather
/// than imported from the implementation, so the assertions pin the answer a client sees.
const MESSAGE_VERSION_NOT_FOUND: &str = "AI Agent version does not exist.";
const MESSAGE_SYSTEM_NOT_VERSIONABLE: &str =
    "A default AI Agent created by Amazon Q in Connect cannot be versioned. Create an AI \
     Agent of your own to publish versions of.";
const MESSAGE_MODIFIED_SINCE: &str =
    "The AI Agent has been modified after the specified modifiedTime, so no version was \
     created. Read the AI Agent again and retry with the modifiedTime that read reports.";
const MESSAGE_IDEMPOTENCY_CONFLICT: &str = "Idempotency conflict";
const MESSAGE_BAD_TOKEN: &str = "nextToken: the specified token is not valid.";

/// The `AIAgentType` most rows use, paired with the one whose binding a fresh assistant
/// pins floating — the one the binding rows configure.
const TYPE_MANUAL_SEARCH: &str = "MANUAL_SEARCH";

/// The low bound of the modelled `Version` shape
/// (`smithy_query(query="operation",operation="DeleteAIAgentVersion")`: `versionNumber` is
/// `range min 1`), and a number no agent of these tests ever publishes.
const VERSION_MIN: i64 = 1;
const VERSION_NEVER_PUBLISHED: i64 = 7;

/// The published bounds of `maxResults`
/// (`smithy_query(query="operation",operation="ListAIAgentVersions")`: `range 1..100`).
const MAX_RESULTS_MIN: i64 = 1;

/// Both values the modelled `Origin` declares
/// (`smithy_query(query="shape",shape="Origin")`), which `ListAIAgentVersions` filters by.
const ORIGIN_SYSTEM: &str = "SYSTEM";

/// A day in seconds, used to state a `modifiedTime` precondition that is unambiguously
/// older than the one the record carries without waiting for any clock.
const ONE_DAY_SECONDS: f64 = 86_400.0;

/// `CreateAIAgentVersion` on one agent, as the model's URI binds it
/// (`POST /assistants/{assistantId}/aiagents/{aiAgentId}/versions`).
pub(crate) async fn publish(emulator: &Emulator, assistant: &str, agent: &str, body: Value) -> Reply {
    emulator
        .send(
            "POST",
            &format!(
                "/assistants/{}/aiagents/{}/versions",
                label(assistant),
                label(agent)
            ),
            Some(body),
        )
        .await
}

/// `ListAIAgentVersions` (`GET /assistants/{assistantId}/aiagents/{aiAgentId}/versions`),
/// with `query` appended verbatim so a row can page and filter.
pub(crate) async fn list_versions(
    emulator: &Emulator,
    assistant: &str,
    agent: &str,
    query: &str,
) -> Reply {
    emulator
        .send(
            "GET",
            &format!(
                "/assistants/{}/aiagents/{}/versions{query}",
                label(assistant),
                label(agent)
            ),
            None,
        )
        .await
}

/// `DeleteAIAgentVersion`
/// (`DELETE /assistants/{assistantId}/aiagents/{aiAgentId}/versions/{versionNumber}`).
async fn delete_version(
    emulator: &Emulator,
    assistant: &str,
    agent: &str,
    version_number: i64,
) -> Reply {
    emulator
        .send(
            "DELETE",
            &format!(
                "/assistants/{}/aiagents/{}/versions/{version_number}",
                label(assistant),
                label(agent)
            ),
            None,
        )
        .await
}

/// `UpdateAssistantAIAgent` (`POST /assistants/{assistantId}/aiagentConfiguration`).
async fn bind_agent(emulator: &Emulator, assistant: &str, body: Value) -> Reply {
    emulator
        .send(
            "POST",
            &format!("/assistants/{}/aiagentConfiguration", label(assistant)),
            Some(body),
        )
        .await
}

/// `GetAssistant` (`GET /assistants/{assistantId}`).
async fn get_assistant(emulator: &Emulator, assistant: &str) -> Reply {
    emulator
        .send("GET", &format!("/assistants/{}", label(assistant)), None)
        .await
}

/// The `AIAgentData` a read of one VERSION must answer with: the record the working copy
/// carried when that version was published, with the one member a version renders
/// differently — the ARN names the version addressed.
fn versioned(record: &Value, version_number: i64) -> Value {
    let mut expected = record.clone();
    let arn = record
        .get("aiAgentArn")
        .and_then(Value::as_str)
        .unwrap_or_default();
    expected["aiAgentArn"] = json!(format!("{arn}:{version_number}"));
    expected
}

/// A create body for one row: every member a caller may send, with `description` left out
/// when the row carries none.
fn create_body(name: &str, agent_type: &str, visibility: &str, description: Option<&str>) -> Value {
    let mut body = json!({
        "name": name,
        "type": agent_type,
        "visibilityStatus": visibility,
        "configuration": full_configuration(agent_type),
    });
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    body
}

/// Create one agent and answer with (its id, the `AIAgentData` the create stored).
async fn created_agent(
    emulator: &Emulator,
    assistant: &str,
    case: &str,
    body: Value,
) -> (String, Value) {
    let reply = create_agent(emulator, assistant, body).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the agent this row versions must be created, got HTTP {} with body {}",
        reply.status, reply.body
    );
    let record = reply
        .field("/aiAgent")
        .unwrap_or_else(|| panic!("case {case:?}: the create answered no aiAgent, body {}", reply.body))
        .clone();
    let id = reply.string("/aiAgent/aiAgentId");
    assert!(
        !id.is_empty(),
        "case {case:?}: the create answered no aiAgentId, body {}",
        reply.body
    );
    (id, record)
}

/// The `versionNumber`s one listing reports, in the order it reported them.
fn listed_numbers(case: &str, reply: &Reply) -> Vec<i64> {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the listing must be served, got HTTP {} with body {}",
        reply.status, reply.body
    );
    reply
        .field("/aiAgentVersionSummaries")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the listing must report the required aiAgentVersionSummaries, \
                 body {}",
                reply.body
            )
        })
        .iter()
        .map(|summary| {
            summary
                .get("versionNumber")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a summary must carry its versionNumber, got {summary}")
                })
        })
        .collect()
}

/// A reply refusing a resource this account does not hold: HTTP 404, the modelled error and
/// the sentence this endpoint words that class with, naming the resource addressed.
fn assert_refused(
    case: &str,
    reply: &Reply,
    status: u16,
    code: &str,
    message: &str,
) {
    assert_eq!(
        reply.status, status,
        "case {case:?}: expected HTTP {status}, got {} with body {}",
        reply.status, reply.body
    );
    assert!(
        reply.error_type.contains(code),
        "case {case:?}: expected the modelled {code}, got x-amzn-errortype {:?} with body {}",
        reply.error_type,
        reply.body
    );
    assert_eq!(
        reply.string("/message"),
        message,
        "case {case:?}: expected the sentence this endpoint words that refusal with, body {}",
        reply.body
    );
}

#[tokio::test]
async fn publishing_a_version_numbers_it_from_one_and_snapshots_the_working_copy() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai agent version publish").await;

    // The rows span the agent's own axes: its type (and so its configuration union member),
    // its visibility status, and whether it carries a description at all.
    let rows: [(&str, &str, &str, Option<&str>); 4] = [
        (
            "manual search, published, described",
            TYPE_MANUAL_SEARCH,
            VISIBILITY_PUBLISHED,
            Some("original-description"),
        ),
        (
            "answer recommendation, saved, described",
            "ANSWER_RECOMMENDATION",
            VISIBILITY_SAVED,
            Some("recommendation-agent"),
        ),
        (
            "email overview, published, no description",
            "EMAIL_OVERVIEW",
            VISIBILITY_PUBLISHED,
            None,
        ),
        (
            "case summarization, saved, no description",
            "CASE_SUMMARIZATION",
            VISIBILITY_SAVED,
            None,
        ),
    ];

    for (case, agent_type, visibility, description) in rows {
        let (agent, record) = created_agent(
            &emulator,
            &assistant,
            case,
            create_body(&format!("agent-{agent_type}"), agent_type, visibility, description),
        )
        .await;

        // A fresh agent has published nothing, which is an empty page rather than a refusal.
        let listed = list_versions(&emulator, &assistant, &agent, "").await;
        assert_eq!(
            listed_numbers(case, &listed),
            Vec::<i64>::new(),
            "case {case:?}: an agent that published nothing must list no versions, body {}",
            listed.body
        );
        assert!(
            listed.field("/nextToken").is_none(),
            "case {case:?}: an empty page must carry no nextToken, body {}",
            listed.body
        );

        // Two publishes in a row: 1, then 2.
        for expected_number in [1_i64, 2] {
            let published = publish(&emulator, &assistant, &agent, json!({})).await;
            assert_eq!(
                published.status, 200,
                "case {case:?}: the publish must be served, got HTTP {} with body {}",
                published.status, published.body
            );
            assert_eq!(
                published.field("/versionNumber").and_then(Value::as_i64),
                Some(expected_number),
                "case {case:?}: expected versionNumber {expected_number}, body {}",
                published.body
            );
            let expected = versioned(&record, expected_number);
            assert_eq!(
                published.field("/aiAgent"),
                Some(&expected),
                "case {case:?}: the publish must answer with the working copy it snapshotted, \
                 expected {expected}, body {}",
                published.body
            );

            // The same snapshot, read back through the version-addressed read.
            let read = get_agent(&emulator, &assistant, &format!("{agent}:{expected_number}")).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the version-addressed read must be served, got HTTP {} with \
                 body {}",
                read.status, read.body
            );
            assert_eq!(
                read.field("/aiAgent"),
                Some(&expected),
                "case {case:?}: the read of version {expected_number} must answer with what was \
                 published under it, expected {expected}, body {}",
                read.body
            );
            assert_eq!(
                read.field("/versionNumber").and_then(Value::as_i64),
                Some(expected_number),
                "case {case:?}: a read addressed through a numbered qualifier must report that \
                 version number, body {}",
                read.body
            );
        }

        // The listing reports both, ascending, each summary carrying what the version holds.
        let listed = list_versions(&emulator, &assistant, &agent, "").await;
        assert_eq!(
            listed_numbers(case, &listed),
            vec![1, 2],
            "case {case:?}: expected the published versions in ascending order, body {}",
            listed.body
        );
        for number in [1_i64, 2] {
            let expected = versioned(&record, number);
            let summary = listed.field(&format!(
                "/aiAgentVersionSummaries/{}/aiAgentSummary",
                number - 1
            ));
            assert_eq!(
                summary,
                Some(&expected),
                "case {case:?}: the summary of version {number} must report the snapshot, \
                 expected {expected}, got {summary:?}"
            );
        }

        // The working copy itself still reports no version number: the floating alias names
        // the agent, not one of its versions.
        let working_copy = get_agent(&emulator, &assistant, &agent).await;
        assert!(
            working_copy.field("/versionNumber").is_none(),
            "case {case:?}: a read of the working copy must report no versionNumber, body {}",
            working_copy.body
        );
        let latest = get_agent(&emulator, &assistant, &format!("{agent}:{QUALIFIER_LATEST}")).await;
        assert!(
            latest.field("/versionNumber").is_none(),
            "case {case:?}: a read addressed through the floating alias must report no \
             versionNumber, body {}",
            latest.body
        );
    }
}

#[tokio::test]
async fn a_published_version_is_immutable_under_a_later_update_of_the_working_copy() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai agent version immutability").await;

    // One row per member an update of an AI agent may change, so the rule is asserted over
    // the whole set rather than over the description the recorded check happened to change.
    let rows: [(&str, Value); 3] = [
        (
            "the description",
            json!({
                "visibilityStatus": VISIBILITY_SAVED,
                "description": "updated-description",
            }),
        ),
        (
            "the visibility status",
            json!({ "visibilityStatus": VISIBILITY_PUBLISHED }),
        ),
        (
            "the configuration",
            json!({
                "visibilityStatus": VISIBILITY_SAVED,
                "configuration": json!({ "manualSearchAIAgentConfiguration": { "locale": "de_DE" } }),
            }),
        ),
    ];

    for (case, update) in rows {
        let (agent, record) = created_agent(
            &emulator,
            &assistant,
            case,
            create_body(
                &format!("immutable-{}", case.replace(' ', "-")),
                TYPE_MANUAL_SEARCH,
                VISIBILITY_SAVED,
                Some("original-description"),
            ),
        )
        .await;

        let published = publish(&emulator, &assistant, &agent, json!({})).await;
        assert_eq!(
            published.status, 200,
            "case {case:?}: the publish must be served, got HTTP {} with body {}",
            published.status, published.body
        );

        let updated = update_agent(&emulator, &assistant, &agent, update.clone()).await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: the update of the working copy must be served, got HTTP {} with \
             body {}",
            updated.status, updated.body
        );
        let updated_record = updated
            .field("/aiAgent")
            .unwrap_or_else(|| panic!("case {case:?}: the update answered no aiAgent"))
            .clone();

        // The published version still reports what was published under it, member for
        // member — the update reached the working copy alone.
        let expected = versioned(&record, 1);
        let read = get_agent(&emulator, &assistant, &format!("{agent}:1")).await;
        assert_eq!(
            read.field("/aiAgent"),
            Some(&expected),
            "case {case:?}: version 1 must be immutable under an update of {case}, expected \
             {expected}, body {}",
            read.body
        );

        // The next publish snapshots what the working copy now holds, under the next number.
        let republished = publish(&emulator, &assistant, &agent, json!({})).await;
        assert_eq!(
            republished.field("/versionNumber").and_then(Value::as_i64),
            Some(2),
            "case {case:?}: the publish after the update must be version 2, body {}",
            republished.body
        );
        let expected_after = versioned(&updated_record, 2);
        assert_eq!(
            republished.field("/aiAgent"),
            Some(&expected_after),
            "case {case:?}: version 2 must carry the updated working copy, expected \
             {expected_after}, body {}",
            republished.body
        );
    }
}

#[tokio::test]
async fn deleting_a_version_an_assistant_binding_names_succeeds_and_leaves_the_assistant_active() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai agent version delete under binding").await;
    let (agent, _) = created_agent(
        &emulator,
        &assistant,
        "bound version",
        create_body(
            "bound-version-agent",
            TYPE_MANUAL_SEARCH,
            VISIBILITY_PUBLISHED,
            Some("bound-description"),
        ),
    )
    .await;

    let published = publish(&emulator, &assistant, &agent, json!({})).await;
    assert_eq!(
        published.field("/versionNumber").and_then(Value::as_i64),
        Some(VERSION_MIN),
        "case {:?}: the first publish must be version {VERSION_MIN}, body {}",
        "bound version",
        published.body
    );

    // The binding names that exact version, which is the form the modelled
    // `UuidWithQualifier` admits.
    let pinned = format!("{agent}:{VERSION_MIN}");
    let bound = bind_agent(
        &emulator,
        &assistant,
        json!({
            "aiAgentType": TYPE_MANUAL_SEARCH,
            "configuration": { "aiAgentId": pinned },
        }),
    )
    .await;
    assert_eq!(
        bound.status, 200,
        "case {:?}: binding a published version must be served, got HTTP {} with body {}",
        "bound version", bound.status, bound.body
    );
    assert_eq!(
        bound.string(&format!(
            "/assistant/aiAgentConfiguration/{TYPE_MANUAL_SEARCH}/aiAgentId"
        )),
        pinned,
        "case {:?}: the assistant must report the binding as sent, body {}",
        "bound version",
        bound.body
    );

    // The delete succeeds despite the binding, and answers 204 with an empty body.
    let deleted = delete_version(&emulator, &assistant, &agent, VERSION_MIN).await;
    assert_eq!(
        deleted.status, 204,
        "case {:?}: deleting a bound version must answer HTTP 204, got {} with body {}",
        "bound version", deleted.status, deleted.body
    );
    // A 204 carries no members: the framework renders this service's empty
    // `DeleteAIAgentVersionResponse` as an empty JSON object.
    let members = match &deleted.body {
        Value::Null => 0,
        Value::Object(members) => members.len(),
        other => panic!("case {:?}: expected an empty body, got {other}", "bound version"),
    };
    assert_eq!(
        members, 0,
        "case {:?}: the 204 must carry no members, got {}",
        "bound version", deleted.body
    );

    // The version is gone from the listing and from the version-addressed read.
    let listed = list_versions(&emulator, &assistant, &agent, "").await;
    assert_eq!(
        listed_numbers("bound version", &listed),
        Vec::<i64>::new(),
        "case {:?}: the deleted version must not be listed, body {}",
        "bound version",
        listed.body
    );
    let read = get_agent(&emulator, &assistant, &pinned).await;
    assert_refused(
        "a read of a deleted version",
        &read,
        404,
        CODE_NOT_FOUND,
        MESSAGE_AI_AGENT_NOT_FOUND,
    );

    // The assistant keeps the binding it was given and the status it had: this delete is no
    // referential-integrity refusal and no write to the assistant.
    let assistant_read = get_assistant(&emulator, &assistant).await;
    assert_eq!(
        assistant_read.string("/assistant/status"),
        STATUS_ACTIVE,
        "case {:?}: the assistant must still report {STATUS_ACTIVE}, body {}",
        "bound version",
        assistant_read.body
    );
    assert_eq!(
        assistant_read.string(&format!(
            "/assistant/aiAgentConfiguration/{TYPE_MANUAL_SEARCH}/aiAgentId"
        )),
        pinned,
        "case {:?}: the binding must be reported unchanged after the delete, body {}",
        "bound version",
        assistant_read.body
    );

    // A second delete of the same number removes nothing: there is no version under it.
    let again = delete_version(&emulator, &assistant, &agent, VERSION_MIN).await;
    assert_refused(
        "a second delete of the same version",
        &again,
        404,
        CODE_NOT_FOUND,
        MESSAGE_VERSION_NOT_FOUND,
    );
}

#[tokio::test]
async fn a_version_number_a_delete_released_is_never_handed_out_again() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai agent version numbering").await;
    let (agent, _) = created_agent(
        &emulator,
        &assistant,
        "numbering",
        create_body(
            "numbering-agent",
            TYPE_MANUAL_SEARCH,
            VISIBILITY_SAVED,
            Some("numbering-description"),
        ),
    )
    .await;

    for expected_number in [1_i64, 2, 3] {
        let published = publish(&emulator, &assistant, &agent, json!({})).await;
        assert_eq!(
            published.field("/versionNumber").and_then(Value::as_i64),
            Some(expected_number),
            "case {:?}: expected versionNumber {expected_number}, body {}",
            "numbering",
            published.body
        );
    }

    // Release the newest and the oldest, then publish again: the next number follows the
    // highest ever published, so neither released number comes back.
    for released in [3_i64, 1] {
        let deleted = delete_version(&emulator, &assistant, &agent, released).await;
        assert_eq!(
            deleted.status, 204,
            "case {:?}: deleting version {released} must answer HTTP 204, got {} with body {}",
            "numbering", deleted.status, deleted.body
        );
    }
    let published = publish(&emulator, &assistant, &agent, json!({})).await;
    assert_eq!(
        published.field("/versionNumber").and_then(Value::as_i64),
        Some(4),
        "case {:?}: the publish after a delete must take the next unused number, body {}",
        "numbering",
        published.body
    );
    let listed = list_versions(&emulator, &assistant, &agent, "").await;
    assert_eq!(
        listed_numbers("numbering", &listed),
        vec![2, 4],
        "case {:?}: the listing must report the versions still held, ascending, body {}",
        "numbering",
        listed.body
    );
}

#[tokio::test]
async fn deleting_the_agent_takes_every_version_of_it_along() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai agent version agent delete").await;
    let (agent, _) = created_agent(
        &emulator,
        &assistant,
        "agent delete",
        create_body(
            "agent-delete-versions",
            TYPE_MANUAL_SEARCH,
            VISIBILITY_SAVED,
            Some("agent-delete-description"),
        ),
    )
    .await;
    for _ in 0..2 {
        let published = publish(&emulator, &assistant, &agent, json!({})).await;
        assert_eq!(
            published.status, 200,
            "case {:?}: the publish must be served, got HTTP {} with body {}",
            "agent delete", published.status, published.body
        );
    }

    let deleted = emulator
        .send(
            "DELETE",
            &format!(
                "/assistants/{}/aiagents/{}",
                label(&assistant),
                label(&agent)
            ),
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 204,
        "case {:?}: deleting the agent must answer HTTP 204, got {} with body {}",
        "agent delete", deleted.status, deleted.body
    );

    // Nothing of the agent is reachable afterwards — the agent, its versions, and the
    // listing of them are all gone.
    let listed = list_versions(&emulator, &assistant, &agent, "").await;
    assert_refused(
        "listing the versions of a deleted agent",
        &listed,
        404,
        CODE_NOT_FOUND,
        MESSAGE_AI_AGENT_NOT_FOUND,
    );
    let read = get_agent(&emulator, &assistant, &format!("{agent}:1")).await;
    assert_refused(
        "reading a version of a deleted agent",
        &read,
        404,
        CODE_NOT_FOUND,
        MESSAGE_AI_AGENT_NOT_FOUND,
    );

    // Numbering is per agent: a fresh agent starts at 1 again.
    let (replacement, _) = created_agent(
        &emulator,
        &assistant,
        "agent delete replacement",
        create_body(
            "agent-delete-versions-again",
            TYPE_MANUAL_SEARCH,
            VISIBILITY_SAVED,
            Some("agent-delete-description"),
        ),
    )
    .await;
    let published = publish(&emulator, &assistant, &replacement, json!({})).await;
    assert_eq!(
        published.field("/versionNumber").and_then(Value::as_i64),
        Some(VERSION_MIN),
        "case {:?}: a fresh agent's first version must be {VERSION_MIN}, body {}",
        "agent delete replacement",
        published.body
    );
}

#[tokio::test]
async fn a_modified_time_precondition_refuses_a_publish_of_a_working_copy_that_moved_on() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai agent version precondition").await;
    let (agent, record) = created_agent(
        &emulator,
        &assistant,
        "precondition",
        create_body(
            "precondition-agent",
            TYPE_MANUAL_SEARCH,
            VISIBILITY_SAVED,
            Some("precondition-description"),
        ),
    )
    .await;
    let created_modified_time = record
        .get("modifiedTime")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("the create must report a modifiedTime, got {record}"));

    // The precondition the caller read off the create still holds, so the publish is served.
    let published = publish(
        &emulator,
        &assistant,
        &agent,
        json!({ "modifiedTime": created_modified_time }),
    )
    .await;
    assert_eq!(
        published.field("/versionNumber").and_then(Value::as_i64),
        Some(VERSION_MIN),
        "case {:?}: a publish whose precondition holds must be served, body {}",
        "precondition holds",
        published.body
    );

    // A precondition naming an instant BEFORE the agent was last written does not hold.
    let stale = publish(
        &emulator,
        &assistant,
        &agent,
        json!({ "modifiedTime": created_modified_time - ONE_DAY_SECONDS }),
    )
    .await;
    assert_refused(
        "a precondition a day older than the record",
        &stale,
        409,
        CODE_CONFLICT,
        MESSAGE_MODIFIED_SINCE,
    );

    // An update moves the working copy on, so the create's own precondition no longer holds
    // either; the one the UPDATE reported does.
    let updated = update_agent(
        &emulator,
        &assistant,
        &agent,
        json!({
            "visibilityStatus": VISIBILITY_PUBLISHED,
            "description": "precondition-updated",
        }),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "case {:?}: the update must be served, got HTTP {} with body {}",
        "precondition", updated.status, updated.body
    );
    let updated_modified_time = updated
        .field("/aiAgent/modifiedTime")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("the update must report a modifiedTime, body {}", updated.body));
    let after_update = publish(
        &emulator,
        &assistant,
        &agent,
        json!({ "modifiedTime": updated_modified_time }),
    )
    .await;
    assert_eq!(
        after_update.field("/versionNumber").and_then(Value::as_i64),
        Some(2),
        "case {:?}: a publish stating the instant the update reported must be served, body {}",
        "precondition after update",
        after_update.body
    );

    // Only what the preconditions allowed was published.
    let listed = list_versions(&emulator, &assistant, &agent, "").await;
    assert_eq!(
        listed_numbers("precondition", &listed),
        vec![1, 2],
        "case {:?}: the refused publish must have published nothing, body {}",
        "precondition",
        listed.body
    );
}

#[tokio::test]
async fn a_replayed_client_token_answers_with_the_version_it_already_published() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai agent version idempotency").await;
    let (agent, record) = created_agent(
        &emulator,
        &assistant,
        "idempotency",
        create_body(
            "idempotency-agent",
            TYPE_MANUAL_SEARCH,
            VISIBILITY_SAVED,
            Some("idempotency-description"),
        ),
    )
    .await;
    let token = "publish-token-0001";

    let first = publish(
        &emulator,
        &assistant,
        &agent,
        json!({ "clientToken": token }),
    )
    .await;
    assert_eq!(
        first.field("/versionNumber").and_then(Value::as_i64),
        Some(VERSION_MIN),
        "case {:?}: the first publish must be version {VERSION_MIN}, body {}",
        "idempotency",
        first.body
    );

    let replay = publish(
        &emulator,
        &assistant,
        &agent,
        json!({ "clientToken": token }),
    )
    .await;
    assert_eq!(
        replay.field("/versionNumber").and_then(Value::as_i64),
        Some(VERSION_MIN),
        "case {:?}: a replayed token must answer with the version it already published, body {}",
        "idempotency",
        replay.body
    );
    let expected = versioned(&record, VERSION_MIN);
    assert_eq!(
        replay.field("/aiAgent"),
        Some(&expected),
        "case {:?}: the replay must answer with the published snapshot, expected {expected}, \
         body {}",
        "idempotency",
        replay.body
    );
    let listed = list_versions(&emulator, &assistant, &agent, "").await;
    assert_eq!(
        listed_numbers("idempotency", &listed),
        vec![VERSION_MIN],
        "case {:?}: a replay must not number a second version, body {}",
        "idempotency",
        listed.body
    );

    // The same token carrying a DIFFERENT request is the modelled idempotency conflict. The
    // precondition below is in the future, so the request itself is one the service would
    // otherwise serve.
    let future = record
        .get("modifiedTime")
        .and_then(Value::as_f64)
        .unwrap_or_default()
        + ONE_DAY_SECONDS;
    let conflicting = publish(
        &emulator,
        &assistant,
        &agent,
        json!({ "clientToken": token, "modifiedTime": future }),
    )
    .await;
    assert_refused(
        "a replayed token carrying a different request",
        &conflicting,
        409,
        CODE_CONFLICT,
        MESSAGE_IDEMPOTENCY_CONFLICT,
    );
}

#[tokio::test]
async fn the_listing_pages_in_ascending_order_and_filters_by_origin() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai agent version listing").await;
    let (agent, _) = created_agent(
        &emulator,
        &assistant,
        "listing",
        create_body(
            "listing-agent",
            TYPE_MANUAL_SEARCH,
            VISIBILITY_SAVED,
            Some("listing-description"),
        ),
    )
    .await;
    for _ in 0..3 {
        let published = publish(&emulator, &assistant, &agent, json!({})).await;
        assert_eq!(
            published.status, 200,
            "case {:?}: the publish must be served, got HTTP {} with body {}",
            "listing", published.status, published.body
        );
    }

    // One version per page, following the token the previous page handed out.
    let mut seen: Vec<i64> = Vec::new();
    let mut query = format!("?maxResults={MAX_RESULTS_MIN}");
    for _ in 0..3 {
        let page = list_versions(&emulator, &assistant, &agent, &query).await;
        let numbers = listed_numbers("listing", &page);
        assert_eq!(
            numbers.len(),
            1,
            "case {:?}: a page of maxResults={MAX_RESULTS_MIN} must carry one summary, body {}",
            "listing",
            page.body
        );
        seen.extend(numbers);
        match page.field("/nextToken").and_then(Value::as_str) {
            Some(token) => query = format!("?maxResults={MAX_RESULTS_MIN}&nextToken={token}"),
            None => break,
        }
    }
    assert_eq!(
        seen,
        vec![1, 2, 3],
        "case {:?}: paging must report every version once, ascending, got {seen:?}",
        "listing"
    );
    let last = list_versions(
        &emulator,
        &assistant,
        &agent,
        &format!("?maxResults={}", 100),
    )
    .await;
    assert!(
        last.field("/nextToken").is_none(),
        "case {:?}: a page that reports the last version must carry no nextToken, body {}",
        "listing",
        last.body
    );

    // `origin` selects by the origin of the agent the snapshot was taken of: every version
    // here is of an agent a caller created.
    let customer = list_versions(
        &emulator,
        &assistant,
        &agent,
        &format!("?origin={ORIGIN_CUSTOMER}"),
    )
    .await;
    assert_eq!(
        listed_numbers("origin CUSTOMER", &customer),
        vec![1, 2, 3],
        "case {:?}: every version of a caller's agent must be reported, body {}",
        "origin CUSTOMER",
        customer.body
    );
    let system = list_versions(
        &emulator,
        &assistant,
        &agent,
        &format!("?origin={ORIGIN_SYSTEM}"),
    )
    .await;
    assert_eq!(
        listed_numbers("origin SYSTEM", &system),
        Vec::<i64>::new(),
        "case {:?}: no version of a caller's agent has the service's origin, body {}",
        "origin SYSTEM",
        system.body
    );

    // A token this service did not mint names no position.
    let bogus = list_versions(&emulator, &assistant, &agent, "?nextToken=not-a-token").await;
    assert_refused(
        "a nextToken this service did not mint",
        &bogus,
        400,
        CODE_VALIDATION,
        MESSAGE_BAD_TOKEN,
    );
}

#[tokio::test]
async fn the_version_surface_refuses_what_it_does_not_hold() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai agent version refusals").await;
    let other_assistant = assistant_id(&emulator, "ai agent version refusals elsewhere").await;
    let (agent, _) = created_agent(
        &emulator,
        &assistant,
        "refusals",
        create_body(
            "refusal-agent",
            TYPE_MANUAL_SEARCH,
            VISIBILITY_SAVED,
            Some("refusal-description"),
        ),
    )
    .await;
    let published = publish(&emulator, &assistant, &agent, json!({})).await;
    assert_eq!(
        published.status, 200,
        "case {:?}: the publish the refusals are stated against must be served, got HTTP {} \
         with body {}",
        "refusals", published.status, published.body
    );

    // The id of one agent the SERVICE provisioned, read off the binding a fresh assistant
    // reports for the type rather than written out here.
    let assistant_read = get_assistant(&emulator, &assistant).await;
    let built_in_binding = assistant_read.string(&format!(
        "/assistant/aiAgentConfiguration/{TYPE_MANUAL_SEARCH}/aiAgentId"
    ));
    let built_in = built_in_binding
        .split_once(':')
        .map_or(built_in_binding.as_str(), |(id, _)| id)
        .to_string();

    // Publishes.
    let rows: [(&str, String, String, u16, &str, String); 5] = [
        (
            "an assistant this account does not hold",
            ABSENT_UUID.to_string(),
            agent.clone(),
            404,
            CODE_NOT_FOUND,
            MESSAGE_ASSISTANT_NOT_FOUND.to_string(),
        ),
        (
            "an agent this assistant does not hold",
            assistant.clone(),
            ABSENT_UUID.to_string(),
            404,
            CODE_NOT_FOUND,
            MESSAGE_AI_AGENT_NOT_FOUND.to_string(),
        ),
        (
            "an agent of another assistant",
            other_assistant.clone(),
            agent.clone(),
            404,
            CODE_NOT_FOUND,
            MESSAGE_AI_AGENT_NOT_FOUND.to_string(),
        ),
        (
            "a label naming a published version rather than the working copy",
            assistant.clone(),
            format!("{agent}:{VERSION_MIN}"),
            404,
            CODE_NOT_FOUND,
            MESSAGE_AI_AGENT_NOT_FOUND.to_string(),
        ),
        (
            "an agent the service provisioned",
            assistant.clone(),
            built_in.clone(),
            400,
            CODE_VALIDATION,
            MESSAGE_SYSTEM_NOT_VERSIONABLE.to_string(),
        ),
    ];
    for (case, on_assistant, on_agent, status, code, message) in rows {
        let reply = publish(&emulator, &on_assistant, &on_agent, json!({})).await;
        assert_refused(
            &format!("publishing {case}"),
            &reply,
            status,
            code,
            &message,
        );
    }

    // Listings and deletes refuse the same parents.
    for (case, on_assistant, on_agent, message) in [
        (
            "an assistant this account does not hold",
            ABSENT_UUID.to_string(),
            agent.clone(),
            MESSAGE_ASSISTANT_NOT_FOUND,
        ),
        (
            "an agent this assistant does not hold",
            assistant.clone(),
            ABSENT_UUID.to_string(),
            MESSAGE_AI_AGENT_NOT_FOUND,
        ),
        (
            "an agent of another assistant",
            other_assistant.clone(),
            agent.clone(),
            MESSAGE_AI_AGENT_NOT_FOUND,
        ),
    ] {
        let listed = list_versions(&emulator, &on_assistant, &on_agent, "").await;
        assert_refused(
            &format!("listing the versions of {case}"),
            &listed,
            404,
            CODE_NOT_FOUND,
            message,
        );
        let deleted = delete_version(&emulator, &on_assistant, &on_agent, VERSION_MIN).await;
        assert_refused(
            &format!("deleting a version of {case}"),
            &deleted,
            404,
            CODE_NOT_FOUND,
            message,
        );
    }

    // A version number the agent never published names nothing to remove.
    let missing = delete_version(&emulator, &assistant, &agent, VERSION_NEVER_PUBLISHED).await;
    assert_refused(
        "deleting a version number nothing published",
        &missing,
        404,
        CODE_NOT_FOUND,
        MESSAGE_VERSION_NOT_FOUND,
    );
    // And the refusals wrote nothing: the one published version is still the one held.
    let listed = list_versions(&emulator, &assistant, &agent, "").await;
    assert_eq!(
        listed_numbers("refusals", &listed),
        vec![VERSION_MIN],
        "case {:?}: a refused request must publish and remove nothing, body {}",
        "refusals",
        listed.body
    );
}

#[tokio::test]
async fn one_agent_holds_at_most_the_stores_version_ceiling() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai agent version ceiling").await;
    let (agent, _) = created_agent(
        &emulator,
        &assistant,
        "ceiling",
        create_body(
            "ceiling-agent",
            TYPE_MANUAL_SEARCH,
            VISIBILITY_SAVED,
            Some("ceiling-description"),
        ),
    )
    .await;

    for expected_number in 1..=MAX_AI_AGENT_VERSIONS_PER_AGENT as i64 {
        let published = publish(&emulator, &assistant, &agent, json!({})).await;
        assert_eq!(
            published.field("/versionNumber").and_then(Value::as_i64),
            Some(expected_number),
            "case {:?}: expected versionNumber {expected_number}, body {}",
            "ceiling",
            published.body
        );
    }
    let past = publish(&emulator, &assistant, &agent, json!({})).await;
    assert_refused(
        "a publish past the ceiling",
        &past,
        402,
        CODE_QUOTA,
        &format!(
            "The maximum number of AI Agent versions ({MAX_AI_AGENT_VERSIONS_PER_AGENT}) for \
             this AI Agent has been reached. Delete an AI Agent version before creating \
             another."
        ),
    );

    // Releasing one makes room for exactly one more, and that one takes the next unused
    // number rather than the released one.
    let deleted = delete_version(&emulator, &assistant, &agent, VERSION_MIN).await;
    assert_eq!(
        deleted.status, 204,
        "case {:?}: deleting a version must answer HTTP 204, got {} with body {}",
        "ceiling", deleted.status, deleted.body
    );
    let after = publish(&emulator, &assistant, &agent, json!({})).await;
    assert_eq!(
        after.field("/versionNumber").and_then(Value::as_i64),
        Some(MAX_AI_AGENT_VERSIONS_PER_AGENT as i64 + 1),
        "case {:?}: the publish after a release must take the next unused number, body {}",
        "ceiling",
        after.body
    );
}
