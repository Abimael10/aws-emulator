// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the TAG MAP a published Amazon Q in Connect AI agent version
//! reports — the map `CreateAIAgentVersion` answers with, the one each
//! `ListAIAgentVersions` summary carries, and the one a version-addressed `GetAIAgent`
//! reads back.
//!
//! Every assertion goes over the wire, through the same [`Emulator`] harness the rest of
//! this crate's suites drive the production router with: only the HTTP status and the JSON
//! body are inspected, so a response that is not computed from persisted state cannot pass.
//!
//! The rule under test, asserted over a TABLE of inputs rather than one recorded request:
//! the `tags` a published version reports is the tag set the AI agent carried at the instant
//! it was published — EXACTLY that set, with nothing added to it and nothing dropped from it.
//! `Tags` is "The tags used to organize, track, or control access for this resource"
//! (`smithy_query(query="shape",shape="AIAgentData",member="tags")`), the key pattern
//! reserves the `aws:` prefix (`^(?!aws:)[a-zA-Z+-=._:/]+$`) and so leaves every other key to
//! the account, and nothing in this surface writes a tag of its own: a tag reaches a version
//! only by having been applied to the agent — through the create's own `tags` member or a
//! later `TagResource` merge — before the publish took its snapshot. The snapshot being
//! immutable, a later tag write to the working copy leaves an already-published version's map
//! as it was, while the next publish carries the set as it then stands.
//!
//! The rows differ along every axis the rule generalises over:
//!
//! * how many tags the agent carries when it is published — none, one, several, and a key
//!   and a value at the published length bounds of `TagKey`/`TagValue`;
//! * where those tags came from: the `tags` member of the create, a later `TagResource`
//!   merge, or both;
//! * the FORM of the key and the value, including a key whose name is one a reader might
//!   expect the service to write for itself (`resourceArn`) and a value that is an ARN;
//! * which `AIAgentType` the agent is, and so which configuration union member it carries;
//! * which projection reports the version: the publish's answer, the listing's summary, or
//!   the version-addressed read;
//! * how many versions the agent has published, and whether its tags changed in between.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::ai_agent::{
    assistant_id, create_agent, expected_agent_arn, full_configuration, get_agent,
    QUALIFIER_LATEST, VISIBILITY_SAVED,
};
use super::ai_agent_versions::{list_versions, publish};
use super::resource_tags::{label, Emulator, Reply};
use super::tag_resource::{stored_tags, tag_resource};

/// The published bounds of the modelled tag map's key and value
/// (`smithy_query(query="shape",shape="Tags")`: `TagKey` len 1..128, `TagValue` len
/// 1..256).
const TAG_KEY_MAX: usize = 128;
const TAG_VALUE_MAX: usize = 256;

/// The stand-in a row writes where the tag VALUE must be the agent's own ARN, which no row
/// can spell before the create mints it: [`agent_ready_to_publish`] substitutes it.
const AGENT_ARN_PLACEHOLDER: &str = "<the agent's own ARN>";

/// The tag map one reply reports under `pointer`, as a sorted map.
fn tags_at(case: &str, what: &str, pointer: &str, reply: &Reply) -> BTreeMap<String, String> {
    let member = reply.field(pointer).cloned().unwrap_or_else(|| {
        panic!(
            "case {case:?}: {what} must report a tags member at {pointer}, body {}",
            reply.body
        )
    });
    serde_json::from_value(member).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: {what} must report a tag object at {pointer}, got {error} in body {}",
            reply.body
        )
    })
}

/// A string of `len` ASCII letters, for the rows that sit on a published length bound.
fn filled(len: usize, fill: char) -> String {
    std::iter::repeat_n(fill, len).collect()
}

/// Create one agent, answering with its id. The body carries the row's own tags when it has
/// any, so an untagged row is created exactly as a client that names no tags creates one.
async fn created_agent(
    emulator: &Emulator,
    assistant: &str,
    case: &str,
    name: &str,
    agent_type: &str,
    tags: &BTreeMap<String, String>,
) -> String {
    let mut body = json!({
        "name": name,
        "type": agent_type,
        "visibilityStatus": VISIBILITY_SAVED,
        "configuration": full_configuration(agent_type),
    });
    if !tags.is_empty() {
        body["tags"] = json!(tags);
    }
    let reply = create_agent(emulator, assistant, body).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the agent this row publishes must be created, got HTTP {} with body {}",
        reply.status, reply.body
    );
    let id = reply.string("/aiAgent/aiAgentId");
    assert!(
        !id.is_empty(),
        "case {case:?}: the create answered no aiAgentId, body {}",
        reply.body
    );
    id
}

/// Merge `tags` onto one resource, as a client that tags an existing agent does.
async fn merge_tags(
    emulator: &Emulator,
    case: &str,
    resource_arn: &str,
    tags: &BTreeMap<String, String>,
) {
    if tags.is_empty() {
        return;
    }
    let tagged = tag_resource(emulator, resource_arn, tags).await;
    assert!(
        tagged.status < 300,
        "case {case:?}: the tag merge must be served, got HTTP {} with body {}",
        tagged.status,
        tagged.body
    );
}

/// One row: the case, the `AIAgentType`, the tags the create carries, and the tags a later
/// `TagResource` merges in before the publish.
type Row = (
    &'static str,
    &'static str,
    Vec<(String, String)>,
    Vec<(String, String)>,
);

/// The rows every test here runs. A row may tag the agent with its own ARN by writing
/// [`AGENT_ARN_PLACEHOLDER`] as the value — a value a client may well choose, and one no rule
/// of this surface may write for itself.
fn rows() -> Vec<Row> {
    vec![
        ("no tags at all", "MANUAL_SEARCH", vec![], vec![]),
        (
            "one tag from the create",
            "ANSWER_RECOMMENDATION",
            vec![("env".to_string(), "prod".to_string())],
            vec![],
        ),
        (
            "several tags from the create",
            "EMAIL_OVERVIEW",
            vec![
                ("env".to_string(), "staging".to_string()),
                ("owner".to_string(), "support".to_string()),
                ("cost-centre".to_string(), "4711".to_string()),
            ],
            vec![],
        ),
        (
            "one tag merged in by TagResource",
            "CASE_SUMMARIZATION",
            vec![],
            vec![("team".to_string(), "escalations".to_string())],
        ),
        (
            "tags from the create and from a later merge",
            "MANUAL_SEARCH",
            vec![("env".to_string(), "prod".to_string())],
            vec![("team".to_string(), "escalations".to_string())],
        ),
        (
            "a key and a value at the published length bounds",
            "ANSWER_RECOMMENDATION",
            vec![(filled(TAG_KEY_MAX, 'k'), filled(TAG_VALUE_MAX, 'v'))],
            vec![],
        ),
        (
            "a key naming a resource ARN, merged in by TagResource",
            "MANUAL_SEARCH",
            vec![],
            vec![(
                "resourceArn".to_string(),
                AGENT_ARN_PLACEHOLDER.to_string(),
            )],
        ),
    ]
}

/// Create the row's agent, apply the row's tags, and answer with (its id, its ARN, the tag
/// set it carries at publish time read back off the service).
async fn agent_ready_to_publish(
    emulator: &Emulator,
    assistant: &str,
    case: &str,
    name: &str,
    agent_type: &str,
    create_tags: &[(String, String)],
    merged_tags: &[(String, String)],
) -> (String, String, BTreeMap<String, String>) {
    // The create's own `tags` member cannot name the agent's ARN, which does not exist yet,
    // so a row that tags the agent with its own ARN does it through the merge below.
    let create_map: BTreeMap<String, String> = create_tags.iter().cloned().collect();
    let agent = created_agent(emulator, assistant, case, name, agent_type, &create_map).await;
    let agent_arn = expected_agent_arn(assistant, &agent);
    let merge_map: BTreeMap<String, String> = merged_tags
        .iter()
        .map(|(key, value)| {
            let value = match value.as_str() {
                AGENT_ARN_PLACEHOLDER => agent_arn.clone(),
                written => written.to_string(),
            };
            (key.clone(), value)
        })
        .collect();
    merge_tags(emulator, case, &agent_arn, &merge_map).await;
    // Read back what the agent carries rather than assuming it, so every expectation below
    // is computed from persisted state — and pin that read against the tags the row APPLIED,
    // so a store that persisted none of them cannot satisfy the comparisons that follow by
    // answering empty on both sides.
    let on_the_agent = stored_tags(emulator, case, &agent_arn).await;
    let applied: BTreeMap<String, String> = create_map
        .into_iter()
        .chain(merge_map.clone())
        .collect();
    assert_eq!(
        on_the_agent, applied,
        "case {case:?}: the agent must carry the tags this row applied to it, expected \
         {applied:?}, got {on_the_agent:?}"
    );
    (agent, agent_arn, on_the_agent)
}

#[tokio::test]
async fn a_published_version_reports_the_tag_set_its_agent_carried_and_nothing_else() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai agent version tags").await;

    for (index, (case, agent_type, create_tags, merged_tags)) in rows().into_iter().enumerate() {
        let (agent, agent_arn, on_the_agent) = agent_ready_to_publish(
            &emulator,
            &assistant,
            case,
            &format!("version-tags-agent-{index}"),
            agent_type,
            &create_tags,
            &merged_tags,
        )
        .await;

        let published = publish(&emulator, &assistant, &agent, json!({})).await;
        assert_eq!(
            published.status, 200,
            "case {case:?}: the publish must be served, got HTTP {} with body {}",
            published.status, published.body
        );
        let version_number = published
            .field("/versionNumber")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the publish answered no versionNumber, body {}",
                    published.body
                )
            });

        // The publish's own answer, the listing's summary of that version and the
        // version-addressed read: one map, the agent's own, whichever projection reports it.
        let listed = list_versions(&emulator, &assistant, &agent, "").await;
        let read = get_agent(&emulator, &assistant, &format!("{agent}:{version_number}")).await;
        for (what, actual) in [
            (
                "the publish",
                tags_at(case, "the publish", "/aiAgent/tags", &published),
            ),
            (
                "the listing's summary",
                tags_at(
                    case,
                    "the listing's summary",
                    "/aiAgentVersionSummaries/0/aiAgentSummary/tags",
                    &listed,
                ),
            ),
            (
                "the version-addressed read",
                tags_at(case, "the version-addressed read", "/aiAgent/tags", &read),
            ),
        ] {
            assert_eq!(
                actual, on_the_agent,
                "case {case:?}: {what} must report the tag set the agent carried when \
                 version {version_number} was published, expected {on_the_agent:?}, got \
                 {actual:?}"
            );
        }

        // The WORKING COPY is untouched by the publish, whichever way it is addressed, and
        // so is the tagging surface: the publish writes no tag of its own anywhere.
        for (what, addressed_as) in [
            ("the working copy", agent.clone()),
            (
                "the read addressed through the floating alias",
                format!("{agent}:{QUALIFIER_LATEST}"),
            ),
        ] {
            let read = get_agent(&emulator, &assistant, &addressed_as).await;
            let actual = tags_at(case, what, "/aiAgent/tags", &read);
            assert_eq!(
                actual, on_the_agent,
                "case {case:?}: {what} must report the agent's own tags, expected \
                 {on_the_agent:?}, got {actual:?}"
            );
        }
        let actual = stored_tags(&emulator, case, &agent_arn).await;
        assert_eq!(
            actual, on_the_agent,
            "case {case:?}: the publish must leave the agent's own tag set as it was, \
             expected {on_the_agent:?}, got {actual:?}"
        );
    }
}

#[tokio::test]
async fn a_published_versions_tag_map_is_immutable_under_a_later_tag_write() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai agent version tag immutability").await;

    // One row per way the agent's tag set changes after a version was published: a key
    // added, a stored key re-valued, and a key removed.
    type ImmutabilityRow = (
        &'static str,
        Vec<(&'static str, &'static str)>,
        Vec<&'static str>,
    );
    let rows: [ImmutabilityRow; 3] = [
        ("a key is added", vec![("added", "later")], vec![]),
        ("a stored key is re-valued", vec![("env", "changed")], vec![]),
        ("a stored key is removed", vec![], vec!["env"]),
    ];

    for (index, (case, merged, removed)) in rows.into_iter().enumerate() {
        let create_tags = [("env".to_string(), "prod".to_string())];
        let (agent, agent_arn, at_publish) = agent_ready_to_publish(
            &emulator,
            &assistant,
            case,
            &format!("version-tag-immutability-agent-{index}"),
            "MANUAL_SEARCH",
            &create_tags,
            &[],
        )
        .await;

        let published = publish(&emulator, &assistant, &agent, json!({})).await;
        assert_eq!(
            published.status, 200,
            "case {case:?}: the publish must be served, got HTTP {} with body {}",
            published.status, published.body
        );
        let version_number = published
            .field("/versionNumber")
            .and_then(Value::as_i64)
            .unwrap_or_default();

        let merge: BTreeMap<String, String> = merged
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        merge_tags(&emulator, case, &agent_arn, &merge).await;
        for key in &removed {
            let untagged = emulator
                .send(
                    "DELETE",
                    &format!("/tags/{}?tagKeys={key}", label(&agent_arn)),
                    None,
                )
                .await;
            assert!(
                untagged.status < 300,
                "case {case:?}: the untag must be served, got HTTP {} with body {}",
                untagged.status,
                untagged.body
            );
        }

        // The agent's own tags moved; the version published before the write did not.
        let now_on_the_agent = stored_tags(&emulator, case, &agent_arn).await;
        assert_ne!(
            now_on_the_agent, at_publish,
            "case {case:?}: the row must have changed the agent's tag set, got \
             {now_on_the_agent:?}"
        );
        let listed = list_versions(&emulator, &assistant, &agent, "").await;
        let actual = tags_at(
            case,
            "the listing's summary",
            "/aiAgentVersionSummaries/0/aiAgentSummary/tags",
            &listed,
        );
        assert_eq!(
            actual, at_publish,
            "case {case:?}: version {version_number} must keep the tag set it was published \
             with, expected {at_publish:?}, got {actual:?}"
        );

        // A version published AFTER the write carries the set as it now stands.
        let republished = publish(&emulator, &assistant, &agent, json!({})).await;
        assert_eq!(
            republished.status, 200,
            "case {case:?}: the second publish must be served, got HTTP {} with body {}",
            republished.status, republished.body
        );
        let actual = tags_at(case, "the second publish", "/aiAgent/tags", &republished);
        assert_eq!(
            actual, now_on_the_agent,
            "case {case:?}: the version published after the write must report the agent's \
             tags as they now stand, expected {now_on_the_agent:?}, got {actual:?}"
        );
    }
}
