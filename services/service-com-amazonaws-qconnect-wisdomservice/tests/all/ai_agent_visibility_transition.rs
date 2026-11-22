// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the visibility transition of an AI agent, driven over HTTP through
//! the same router the emulator serves with.
//!
//! The rule under test, asserted over a TABLE of inputs rather than the one recorded
//! request: a write of the required `visibilityStatus` of an existing agent is SERVED with
//! the modelled success — never an unmodelled refusal — it stores the value the request
//! carried whichever value the record held before, and every later read of that agent
//! reports that same value until the next write, so a client polling for the transition
//! converges on the first attempt.
//!
//! The rows differ along every axis the rule generalises over:
//!
//! * every `AIAgentType` the model declares, created with the `AIAgentConfiguration` union
//!   member that type is configured through;
//! * both values the modelled `VisibilityStatus` declares, as the value created with and as
//!   the value written, so both directions of the transition and both no-op writes (a value
//!   written over itself) are walked;
//! * a single write, and a walk of several writes layered on one record;
//! * each addressing form the modelled `aiAgentId` label admits for the read-back — the
//!   bare id, the ARN, and either with the qualifier that names the working copy — read
//!   more than once, since a read must not change what a later reader sees.
//!
//! Beside the write it pins that no request in this class is answered with a status or an
//! error code the operation does not model: the write of an agent this account holds is the
//! modelled 200, and a write naming an absent assistant or an absent agent is the modelled
//! 404 `ResourceNotFoundException`
//! (`smithy_query(query="operation",operation="UpdateAIAgent")` declares 200 and the client
//! errors 403/409/404/400/401/400 and nothing else).
//!
//! Every assertion goes over the wire — only the HTTP status, the `x-amzn-errortype` header
//! and the JSON body are inspected — so a response that is not computed from persisted
//! state cannot pass. The harness (the router, the reply reader, the assistant create, the
//! agent create and read and the configuration bodies) is the one `ai_agent.rs` and
//! `ai_agent_update.rs` already drive this service with.

use serde_json::{json, Value};

use super::ai_agent::{
    assert_not_found, assert_reads_the_record, assistant_id, create_agent, expected_agent_arn,
    expected_read_arn, full_configuration, get_agent, MESSAGE_AI_AGENT_NOT_FOUND,
    MESSAGE_ASSISTANT_NOT_FOUND, ORIGIN_CUSTOMER, QUALIFIER_LATEST, STATUS_ACTIVE,
    TYPE_CONFIGURATIONS, VISIBILITY_PUBLISHED, VISIBILITY_SAVED,
};
use super::ai_agent_update::update_agent;
use super::resource_tags::{Emulator, Reply, ABSENT_UUID};

/// How many times each record is read back after a write: more than once, because a read is
/// a report and must leave the stored value where the write put it.
const READS_PER_WRITE: usize = 3;

/// The members of `AIAgentData` a visibility write never changes: the agent's identity and
/// parentage, the name, type and configuration it was created with, and the members the
/// SERVICE owns (`smithy_query(query="shape",shape="AIAgentData")`). `modifiedTime` is left
/// out on purpose — it reports when the record last changed, which a write is.
const PRESERVED_MEMBERS: [&str; 10] = [
    "aiAgentId",
    "aiAgentArn",
    "assistantId",
    "assistantArn",
    "name",
    "type",
    "configuration",
    "origin",
    "status",
    "tags",
];

/// One row of the table: the agent it creates, and the visibility values written over it in
/// order.
#[derive(Debug, Clone)]
struct Row {
    label: String,
    agent_type: &'static str,
    created_with: &'static str,
    writes: Vec<&'static str>,
}

/// The table: one row per `AIAgentType`, with the walk of visibility values rotating over
/// the row index so that each direction of the transition (SAVED -> PUBLISHED and
/// PUBLISHED -> SAVED), each no-op write (a value written over the value already stored),
/// a single write and a longer walk are all covered across the catalog.
fn rows() -> Vec<Row> {
    TYPE_CONFIGURATIONS
        .iter()
        .enumerate()
        .map(|(index, (agent_type, _))| {
            let (created_with, writes): (&str, Vec<&str>) = match index % 4 {
                // The transition the publishing client asks for: created SAVED, written
                // PUBLISHED once.
                0 => (VISIBILITY_SAVED, vec![VISIBILITY_PUBLISHED]),
                // The same transition the other way round.
                1 => (VISIBILITY_PUBLISHED, vec![VISIBILITY_SAVED]),
                // A value written over itself first, then the transition, then that value
                // written over itself again: a repeated publish is still a publish.
                2 => (
                    VISIBILITY_SAVED,
                    vec![
                        VISIBILITY_SAVED,
                        VISIBILITY_PUBLISHED,
                        VISIBILITY_PUBLISHED,
                    ],
                ),
                // A walk that ends where it started, so no row's outcome can come from a
                // value fixed at create time.
                _ => (
                    VISIBILITY_PUBLISHED,
                    vec![VISIBILITY_SAVED, VISIBILITY_PUBLISHED, VISIBILITY_SAVED],
                ),
            };
            Row {
                label: format!("{agent_type} created {created_with} then {writes:?}"),
                agent_type,
                created_with,
                writes,
            }
        })
        .collect()
}

/// The create body of one row: the agent carries a description and the full configuration
/// of its type, so a visibility write can be shown to leave every other member alone.
fn create_body(row: &Row) -> Value {
    json!({
        "name": format!("visibility {}", row.agent_type),
        "type": row.agent_type,
        "visibilityStatus": row.created_with,
        "description": "The agent whose visibility this suite walks.",
        "configuration": full_configuration(row.agent_type),
    })
}

/// A reply this operation models: the success it declares, or one of the client errors it
/// declares. Anything else — the 501 of an unimplemented operation above all — is a status
/// no caller of this operation can interpret.
fn assert_modelled_status(case: &str, what: &str, reply: &Reply) {
    const MODELLED_STATUSES: [u16; 6] = [200, 400, 401, 403, 404, 409];
    assert!(
        MODELLED_STATUSES.contains(&reply.status),
        "case {case:?}: {what} must answer a status this operation models, got HTTP {} with \
         x-amzn-errortype {:?} and body {}",
        reply.status,
        reply.error_type,
        reply.body
    );
    assert!(
        !reply.error_type.contains("NotImplemented"),
        "case {case:?}: {what} must answer an error code this operation models, got \
         x-amzn-errortype {:?} with body {}",
        reply.error_type,
        reply.body
    );
}

/// What the write answered: the modelled success, the visibility value the request carried,
/// and every member a visibility write does not own still holding the value it had.
fn assert_wrote_visibility(
    case: &str,
    what: &str,
    previous: &Value,
    written: &str,
    reply: &Reply,
) -> Value {
    assert_modelled_status(case, what, reply);
    assert_eq!(
        reply.status, 200,
        "case {case:?}: {what} must be served, got HTTP {} with body {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.string("/aiAgent/visibilityStatus"),
        written,
        "case {case:?}: {what} must report the visibilityStatus the request carried, body {}",
        reply.body
    );
    for member in PRESERVED_MEMBERS {
        let before = previous.get(member);
        let after = reply.field(&format!("/aiAgent/{member}"));
        assert_eq!(
            after,
            before,
            "case {case:?}: {what} must leave {member:?} as it was, expected {before:?}, got \
             {after:?}"
        );
    }
    // The description this row created the agent with is not a member a visibility-only
    // write carries, so it keeps the value it had.
    let description = reply.field("/aiAgent/description");
    assert_eq!(
        description,
        previous.get("description"),
        "case {case:?}: {what} must keep the stored description, got {description:?}"
    );
    reply
        .field("/aiAgent")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: {what} answered no aiAgent, body {}", reply.body))
}

#[tokio::test]
async fn a_visibility_write_is_served_and_every_later_read_reports_the_value_it_wrote() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai agent visibility transition").await;

    for row in rows() {
        let created = create_agent(&emulator, &assistant, create_body(&row)).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: the agent this row walks must be created, got HTTP {} with body {}",
            row.label, created.status, created.body
        );
        assert_eq!(
            created.string("/aiAgent/visibilityStatus"),
            row.created_with,
            "case {:?}: the create must report the visibilityStatus it carried, body {}",
            row.label,
            created.body
        );
        let agent_id = created.string("/aiAgent/aiAgentId");
        let mut record = created
            .field("/aiAgent")
            .cloned()
            .unwrap_or_else(|| panic!("case {:?}: the create answered no aiAgent", row.label));

        for (step, written) in row.writes.iter().enumerate() {
            let case = format!("{} step {step}", row.label);
            // The required member alone: the smallest request that moves an agent's
            // visibility is the one a publishing client sends.
            let reply = update_agent(
                &emulator,
                &assistant,
                &agent_id,
                json!({ "visibilityStatus": written }),
            )
            .await;
            record = assert_wrote_visibility(&case, "the write", &record, written, &reply);

            // The value the write stored is what every later read reports, however the
            // agent is addressed and however often it is read: the transition is durable,
            // and a read is a report rather than a write.
            for addressed_by in [
                agent_id.clone(),
                expected_agent_arn(&assistant, &agent_id),
                format!("{agent_id}:{QUALIFIER_LATEST}"),
                expected_read_arn(&assistant, &agent_id, QUALIFIER_LATEST),
            ] {
                for read in 0..READS_PER_WRITE {
                    let reply = get_agent(&emulator, &assistant, &addressed_by).await;
                    assert_eq!(
                        reply.string("/aiAgent/visibilityStatus"),
                        *written,
                        "case {case:?}: read {read} addressed by {addressed_by:?} must report \
                         the visibilityStatus the write stored, body {}",
                        reply.body
                    );
                    // The whole record, not just the one member: a read answers with what
                    // the write left, member for member.
                    assert_reads_the_record(
                        &case,
                        &format!("read {read} addressed by {addressed_by:?}"),
                        &record,
                        &reply,
                    );
                }
            }
        }

        // The members the service owns are still the service's own after the walk.
        let reply = get_agent(&emulator, &assistant, &agent_id).await;
        assert_eq!(
            reply.string("/aiAgent/origin"),
            ORIGIN_CUSTOMER,
            "case {:?}: an agent a caller created stays CUSTOMER-origin, body {}",
            row.label,
            reply.body
        );
        assert_eq!(
            reply.string("/aiAgent/status"),
            STATUS_ACTIVE,
            "case {:?}: a visibility write does not move the agent's status, body {}",
            row.label,
            reply.body
        );
        assert_eq!(
            reply.string("/aiAgent/visibilityStatus"),
            *row.writes
                .last()
                .expect("every row writes at least one visibility value"),
            "case {:?}: the record must hold the last value written, body {}",
            row.label,
            reply.body
        );
    }
}

#[tokio::test]
async fn a_visibility_write_naming_nothing_this_account_holds_is_the_modelled_not_found() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai agent visibility misses").await;
    let (agent_type, _) = TYPE_CONFIGURATIONS[0];
    let created = create_agent(
        &emulator,
        &assistant,
        json!({
            "name": "visibility misses",
            "type": agent_type,
            "visibilityStatus": VISIBILITY_SAVED,
            "configuration": full_configuration(agent_type),
        }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "the agent this test writes to must be created, got HTTP {} with body {}",
        created.status, created.body
    );
    let agent_id = created.string("/aiAgent/aiAgentId");
    let body = json!({ "visibilityStatus": VISIBILITY_PUBLISHED });

    // The parent is resolved first: a write under an assistant this account does not hold
    // names nothing to write, whatever the agent label says.
    let reply = update_agent(&emulator, ABSENT_UUID, &agent_id, body.clone()).await;
    assert_modelled_status("absent assistant", "the write", &reply);
    assert_not_found(
        "absent assistant",
        MESSAGE_ASSISTANT_NOT_FOUND,
        ABSENT_UUID,
        &reply,
    );

    // An agent its assistant does not hold is likewise nothing to write.
    let reply = update_agent(&emulator, &assistant, ABSENT_UUID, body.clone()).await;
    assert_modelled_status("absent agent", "the write", &reply);
    assert_not_found("absent agent", MESSAGE_AI_AGENT_NOT_FOUND, ABSENT_UUID, &reply);

    // Neither refusal moved the record: it still reports the value it was created with.
    let reply = get_agent(&emulator, &assistant, &agent_id).await;
    assert_eq!(
        reply.string("/aiAgent/visibilityStatus"),
        VISIBILITY_SAVED,
        "a refused write must leave the stored visibilityStatus alone, body {}",
        reply.body
    );

    // And the same request against the agent this account does hold is served, so the
    // refusals above are about the resource addressed and not about the operation.
    let reply = update_agent(&emulator, &assistant, &agent_id, body).await;
    assert_modelled_status("the agent this account holds", "the write", &reply);
    assert_eq!(
        reply.status, 200,
        "the write of an agent this account holds must be served, got HTTP {} with body {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.string("/aiAgent/visibilityStatus"),
        VISIBILITY_PUBLISHED,
        "the served write must report the value it carried, body {}",
        reply.body
    );
}
