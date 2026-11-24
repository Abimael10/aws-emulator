// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the tag entry a PUBLISHED Amazon Q in Connect AI agent version
//! reports for the agent it was published from, driven over HTTP through the same router the
//! emulator serves with.
//!
//! The rule under test, asserted over a TABLE of inputs rather than one recorded request:
//! every projection of a published version reports, under `tags`, the tag set its AI agent
//! carried at the instant it was published TOGETHER WITH one entry keyed
//! [`VERSION_RESOURCE_TAG_KEY`] and valued with the UNQUALIFIED ARN of that agent — while the
//! working copy, and the tagging surface that reads the tags the agent itself holds, report
//! only what a caller applied. The entry belongs to the version record, so it is reported
//! whether or not the agent carries any tag of its own, it names the agent rather than the
//! version (no `:<versionNumber>` qualifier), and it is as immutable as the rest of the
//! snapshot.
//!
//! The rows differ along every axis the rule generalises over:
//!
//! * how many tags the agent carries when it is published — none at all, one, several, and a
//!   key and a value at the published length bounds of `TagKey`/`TagValue`;
//! * where those tags came from: the `tags` member of the create, a later `TagResource`
//!   merge, or both;
//! * whether the agent itself carries a tag under the same key, with a value of its own;
//! * which `AIAgentType` the agent is, and so which configuration union member it carries;
//! * which projection reports the version: the publish's own answer, the listing's summary,
//!   or the version-addressed read;
//! * how many versions the agent has published, and whether its tags changed in between.
//!
//! Every assertion goes over the wire — only the HTTP status and the JSON body are inspected
//! — so a response that is not computed from persisted state cannot pass. The harness (the
//! router, the reply reader, the assistant and agent creates, the publish, the listing and
//! the tagging calls) is the one the rest of this crate's suites already drive this service
//! with.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::ai_agent::{
    assistant_id, create_agent, expected_agent_arn, full_configuration, get_agent,
    QUALIFIER_LATEST, VISIBILITY_SAVED,
};
use super::ai_agent_versions::{list_versions, publish};
use super::resource_tags::{Emulator, Reply};
use super::tag_resource::{stored_tags, tag_resource};

/// The key of the entry a published version reports for the agent it was published from.
///
/// Written out here rather than imported from the implementation, so the assertions pin the
/// map a CLIENT sees: the recorded wire answers of this endpoint carry this entry on a
/// published version of an agent the client never tagged at all, and carry no such entry on
/// the read of that agent's working copy.
pub(crate) const VERSION_RESOURCE_TAG_KEY: &str = "resourceArn";

/// The published bounds of the modelled tag map's key and value
/// (`smithy_query(query="shape",shape="Tags")`: `TagKey` len 1..128, `TagValue` len 1..256).
const TAG_KEY_MAX: usize = 128;
const TAG_VALUE_MAX: usize = 256;

/// A string of `len` ASCII letters, for the rows that sit on a published length bound.
fn filled(len: usize, fill: char) -> String {
    std::iter::repeat_n(fill, len).collect()
}

/// The map a published version must report: the agent's own tag set at publish time, plus
/// the entry naming the agent it was published from.
pub(crate) fn expected_version_tags(
    on_the_agent: &BTreeMap<String, String>,
    agent_arn: &str,
) -> BTreeMap<String, String> {
    let mut expected = on_the_agent.clone();
    expected.insert(VERSION_RESOURCE_TAG_KEY.to_string(), agent_arn.to_string());
    expected
}

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

/// One row: the case, the `AIAgentType`, the tags the create carries, and the tags a later
/// `TagResource` merges in before the publish.
type Row = (
    &'static str,
    &'static str,
    Vec<(String, String)>,
    Vec<(String, String)>,
);

/// The table every test here runs.
fn rows() -> Vec<Row> {
    vec![
        // The case the recording shows: a client that applies no tag anywhere.
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
        // The agent carries a tag of its own under the very key a version reports for the
        // agent it was published from: the version names the agent all the same.
        (
            "the agent's own tag under the same key",
            "MANUAL_SEARCH",
            vec![(
                VERSION_RESOURCE_TAG_KEY.to_string(),
                "the caller's own value".to_string(),
            )],
            vec![],
        ),
        (
            "the agent's own tag under the same key, merged in later",
            "ANSWER_RECOMMENDATION",
            vec![("env".to_string(), "prod".to_string())],
            vec![(
                VERSION_RESOURCE_TAG_KEY.to_string(),
                "merged by the caller".to_string(),
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
    let create_map: BTreeMap<String, String> = create_tags.iter().cloned().collect();
    let mut body = json!({
        "name": name,
        "type": agent_type,
        "visibilityStatus": VISIBILITY_SAVED,
        "configuration": full_configuration(agent_type),
    });
    if !create_map.is_empty() {
        body["tags"] = json!(create_map);
    }
    let created = create_agent(emulator, assistant, body).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the agent this row publishes must be created, got HTTP {} with body {}",
        created.status, created.body
    );
    let agent = created.string("/aiAgent/aiAgentId");
    assert!(
        !agent.is_empty(),
        "case {case:?}: the create answered no aiAgentId, body {}",
        created.body
    );
    let agent_arn = expected_agent_arn(assistant, &agent);
    let merge_map: BTreeMap<String, String> = merged_tags.iter().cloned().collect();
    if !merge_map.is_empty() {
        let tagged = tag_resource(emulator, &agent_arn, &merge_map).await;
        assert!(
            tagged.status < 300,
            "case {case:?}: the tag merge must be served, got HTTP {} with body {}",
            tagged.status,
            tagged.body
        );
    }
    // Read back what the agent carries rather than assuming it, so every expectation below
    // is computed from persisted state — and pin that read against the tags the row APPLIED,
    // so a store that persisted none of them cannot satisfy the comparisons that follow.
    let on_the_agent = stored_tags(emulator, case, &agent_arn).await;
    let applied: BTreeMap<String, String> =
        create_map.into_iter().chain(merge_map).collect();
    assert_eq!(
        on_the_agent, applied,
        "case {case:?}: the agent must carry the tags this row applied to it, expected \
         {applied:?}, got {on_the_agent:?}"
    );
    (agent, agent_arn, on_the_agent)
}

#[tokio::test]
async fn every_projection_of_a_version_names_the_agent_it_was_published_from() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai agent version resource tag").await;

    for (index, (case, agent_type, create_tags, merged_tags)) in rows().into_iter().enumerate() {
        let (agent, agent_arn, on_the_agent) = agent_ready_to_publish(
            &emulator,
            &assistant,
            case,
            &format!("version-resource-tag-agent-{index}"),
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
        let expected = expected_version_tags(&on_the_agent, &agent_arn);

        // The publish's own answer, the listing's summary of that version and the
        // version-addressed read: one map, whichever projection reports it.
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
                actual, expected,
                "case {case:?}: {what} must report the tag set the agent carried when version \
                 {version_number} was published together with the entry naming that agent, \
                 expected {expected:?}, got {actual:?}"
            );
            // The entry names the AGENT, not the version: the version's own ARN carries the
            // qualifier, the value under this key does not.
            let named = actual.get(VERSION_RESOURCE_TAG_KEY).cloned();
            assert_eq!(
                named.as_deref(),
                Some(agent_arn.as_str()),
                "case {case:?}: {what} must name the agent's unqualified ARN {agent_arn:?} \
                 under {VERSION_RESOURCE_TAG_KEY:?}, got {named:?}"
            );
        }
        let version_arn = read.string("/aiAgent/aiAgentArn");
        assert_eq!(
            version_arn,
            format!("{agent_arn}:{version_number}"),
            "case {case:?}: the version-addressed read must report the version-qualified ARN, \
             got {version_arn:?}"
        );

        // The WORKING COPY is untouched by the publish, whichever way it is addressed, and so
        // is the tagging surface: the entry belongs to the version record alone.
        for (what, addressed_as) in [
            ("the working copy", agent.clone()),
            (
                "the read addressed through the floating alias",
                format!("{agent}:{QUALIFIER_LATEST}"),
            ),
        ] {
            let reply = get_agent(&emulator, &assistant, &addressed_as).await;
            let actual = tags_at(case, what, "/aiAgent/tags", &reply);
            assert_eq!(
                actual, on_the_agent,
                "case {case:?}: {what} must report the agent's own tags and nothing else, \
                 expected {on_the_agent:?}, got {actual:?}"
            );
        }
        let actual = stored_tags(&emulator, case, &agent_arn).await;
        assert_eq!(
            actual, on_the_agent,
            "case {case:?}: the publish must leave the agent's own tag set as it was, expected \
             {on_the_agent:?}, got {actual:?}"
        );
    }
}

#[tokio::test]
async fn a_versions_entry_is_as_immutable_as_the_rest_of_its_snapshot() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai agent version resource tag immutability").await;

    // One row per way the agent's tag set moves after a version was published: a key added,
    // a stored key re-valued, and the key the entry uses taken by the caller.
    let rows: [(&str, Vec<(String, String)>); 3] = [
        (
            "a key is added",
            vec![("added".to_string(), "later".to_string())],
        ),
        (
            "a stored key is re-valued",
            vec![("env".to_string(), "changed".to_string())],
        ),
        (
            "the caller takes the key the entry uses",
            vec![(
                VERSION_RESOURCE_TAG_KEY.to_string(),
                "the caller's own value".to_string(),
            )],
        ),
    ];

    for (index, (case, merged)) in rows.into_iter().enumerate() {
        let (agent, agent_arn, at_publish) = agent_ready_to_publish(
            &emulator,
            &assistant,
            case,
            &format!("version-resource-tag-immutability-agent-{index}"),
            "MANUAL_SEARCH",
            &[("env".to_string(), "prod".to_string())],
            &[],
        )
        .await;
        let published = publish(&emulator, &assistant, &agent, json!({})).await;
        assert_eq!(
            published.status, 200,
            "case {case:?}: the publish must be served, got HTTP {} with body {}",
            published.status, published.body
        );
        let expected_first = expected_version_tags(&at_publish, &agent_arn);

        let merge: BTreeMap<String, String> = merged.into_iter().collect();
        let tagged = tag_resource(&emulator, &agent_arn, &merge).await;
        assert!(
            tagged.status < 300,
            "case {case:?}: the tag merge must be served, got HTTP {} with body {}",
            tagged.status,
            tagged.body
        );
        let now_on_the_agent = stored_tags(&emulator, case, &agent_arn).await;
        assert_ne!(
            now_on_the_agent, at_publish,
            "case {case:?}: the row must have changed the agent's tag set, got \
             {now_on_the_agent:?}"
        );

        // The version published BEFORE the write keeps the map it was published with.
        let listed = list_versions(&emulator, &assistant, &agent, "").await;
        let actual = tags_at(
            case,
            "the listing's summary",
            "/aiAgentVersionSummaries/0/aiAgentSummary/tags",
            &listed,
        );
        assert_eq!(
            actual, expected_first,
            "case {case:?}: the first version must keep the tag map it was published with, \
             expected {expected_first:?}, got {actual:?}"
        );

        // A version published AFTER the write carries the set as it now stands, with the same
        // entry naming the same agent.
        let republished = publish(&emulator, &assistant, &agent, json!({})).await;
        assert_eq!(
            republished.status, 200,
            "case {case:?}: the second publish must be served, got HTTP {} with body {}",
            republished.status, republished.body
        );
        let expected_second = expected_version_tags(&now_on_the_agent, &agent_arn);
        let actual = tags_at(case, "the second publish", "/aiAgent/tags", &republished);
        assert_eq!(
            actual, expected_second,
            "case {case:?}: the version published after the write must report the agent's tags \
             as they now stand together with the entry naming it, expected {expected_second:?}, \
             got {actual:?}"
        );
    }
}
