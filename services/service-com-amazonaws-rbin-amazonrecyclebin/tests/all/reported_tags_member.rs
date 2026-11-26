// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for ONE rendering rule: the `Tags` member that reports "the
//! tags assigned to the retention rule".
//!
//! `Tags` is a member of every shape that reports it (`CreateRuleResponse`,
//! `ListTagsForResourceResponse`), read off the one stored tag set of one rule, and its
//! target `TagList` is bounded `0..200`.
//! Source: smithy_query(query="operation", operation="CreateRule,ListTagsForResource")
//! and smithy_query(query="doc", shape="ListTagsForResourceResponse", member="Tags").
//! So a rule carrying tags reports them, and a rule carrying NONE is a tag collection of
//! length zero: the READER (`ListTagsForResource`), whose whole payload is that collection,
//! reports it as `[]` rather than omitting it, so a reader of the collection must find it
//! whatever the rule holds. The CREATE's own member is its echo of the assignments it made,
//! and is absent for a create that assigned none
//! (`tests/all/rule_status_transitions.rs` owns that rule).
//!
//! What this suite pins, which no other suite does (`tests/all/rule_status_transitions.rs`
//! owns the `CreateRule` side of the same member, and this suite reads the member through
//! that suite's reader rather than a second one):
//!
//! * the PRESENCE of the member, as `[]`, for a rule with no tags, on
//!   `ListTagsForResource` — the tagging suite reads the member through a helper that
//!   collapses an absent member and `[]` into the same empty map, so it cannot see the
//!   difference;
//! * that both renderers of the member report the same tag SET for the same rule, whatever
//!   tag set it holds, so the two cannot drift apart again;
//! * that the rendering follows the rule's CURRENT tag set however the rule reached it —
//!   created untagged, created tagged, tagged afterwards by `TagResource`, or emptied
//!   again by `UntagResource` — and not the shape of the request that created it;
//! * that reading the tags twice reports the same body, a read changing nothing.
//!
//! The table spans the class the rule generalises over: the tag count (none, one,
//! several, the published quota), how the set was reached (create, `TagResource`,
//! `UntagResource`), an empty tag value, and non-ASCII keys and values.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::resource_tags::{keys, list_tags, pairs, tag_resource, untag_resource};
use crate::retention_rule_lifecycle::{create_rule, retention, router, RESOURCE_TYPES};
// The one reader in this test target that tells an ABSENT `Tags` member (`None`) from a
// present empty list (`Some` of an empty map), shared with the suite that pins the same
// distinction on `CreateRule`.
use crate::rule_status_transitions::tags_of;
use service_com_amazonaws_rbin_amazonrecyclebin::state::MAX_TAGS_PER_RULE;

/// A rule's tags as the assertions compare them: one value per key, since the ORDER of a
/// `TagList` is not something the service documents.
type Tags = BTreeMap<String, String>;

/// The tag set the rule must be reported as holding, as a map.
fn map_of(tags: &[(String, String)]) -> Tags {
    tags.iter().cloned().collect()
}

/// What a case does to the rule after creating it, on its way to the tag set under test.
enum Step {
    /// `TagResource` with these `(key, value)` pairs.
    Assign(Vec<(String, String)>),
    /// `UntagResource` with these keys.
    Unassign(Vec<String>),
}

/// One row: the case name, the tags the create requests (`None` = no `Tags` member at
/// all), the steps that follow, and the tag set the rule ends up holding.
struct Row {
    case: &'static str,
    created_with: Option<Vec<(String, String)>>,
    steps: Vec<Step>,
    expected: Tags,
}

/// Tag pairs `key0..keyN` filling the published per-rule quota.
fn quota_pairs() -> Vec<(String, String)> {
    (0..MAX_TAGS_PER_RULE)
        .map(|nth| (format!("key{nth}"), format!("value{nth}")))
        .collect()
}

fn rows() -> Vec<Row> {
    let one = pairs(&[("env", "dev")]);
    let several = pairs(&[("env", "prod"), ("team", "platform"), ("cost.centre", "42")]);
    let empty_value = pairs(&[("team", "")]);
    // `TagKey`/`TagValue` admit any Unicode letter: their pattern is
    // ^([\p{L}\p{Z}\p{N}_.:/=+\-@]*)$.
    // Source: smithy_query(query="shape", shape="Tag") — members `Key` and `Value`.
    let non_ascii = pairs(&[("équipe", "plateforme-européenne"), ("ラベル", "値")]);
    let quota = quota_pairs();
    vec![
        Row {
            case: "created with no Tags member at all",
            created_with: None,
            steps: Vec::new(),
            expected: Tags::new(),
        },
        Row {
            case: "created with an explicitly empty Tags list",
            created_with: Some(Vec::new()),
            steps: Vec::new(),
            expected: Tags::new(),
        },
        Row {
            case: "created with one tag",
            created_with: Some(one.clone()),
            steps: Vec::new(),
            expected: map_of(&one),
        },
        Row {
            case: "created with several tags",
            created_with: Some(several.clone()),
            steps: Vec::new(),
            expected: map_of(&several),
        },
        Row {
            case: "created with a tag whose value is empty",
            created_with: Some(empty_value.clone()),
            steps: Vec::new(),
            expected: map_of(&empty_value),
        },
        Row {
            case: "created with non-ASCII keys and values",
            created_with: Some(non_ascii.clone()),
            steps: Vec::new(),
            expected: map_of(&non_ascii),
        },
        Row {
            case: "created with the quota of tags",
            created_with: Some(quota.clone()),
            steps: Vec::new(),
            expected: map_of(&quota),
        },
        Row {
            case: "tagged after an untagged create",
            created_with: None,
            steps: vec![Step::Assign(several.clone())],
            expected: map_of(&several),
        },
        Row {
            case: "one of several tags unassigned",
            created_with: Some(several.clone()),
            steps: vec![Step::Unassign(keys(&["team"]))],
            expected: map_of(&pairs(&[("env", "prod"), ("cost.centre", "42")])),
        },
        Row {
            case: "every tag unassigned again",
            created_with: Some(several.clone()),
            steps: vec![Step::Unassign(keys(&["env", "team", "cost.centre"]))],
            expected: Tags::new(),
        },
        Row {
            case: "tagged and then emptied again",
            created_with: None,
            steps: vec![
                Step::Assign(one.clone()),
                Step::Unassign(keys(&["env"])),
            ],
            expected: Tags::new(),
        },
        Row {
            case: "the quota of tags unassigned again",
            created_with: Some(quota.clone()),
            steps: vec![Step::Unassign(
                quota.iter().map(|(key, _)| key.clone()).collect(),
            )],
            expected: Tags::new(),
        },
    ]
}

/// The create body a row asks for: the smallest rule `CreateRule` accepts, plus the
/// `Tags` member only when the row sends one.
fn create_body(row: &Row) -> Value {
    let mut body = json!({
        "ResourceType": RESOURCE_TYPES[0],
        "RetentionPeriod": retention(7),
    });
    if let Some(tags) = &row.created_with {
        body["Tags"] = Value::Array(
            tags.iter()
                .map(|(key, value)| json!({ "Key": key, "Value": value }))
                .collect(),
        );
    }
    body
}

#[tokio::test]
async fn the_tags_member_is_reported_as_a_present_list_for_every_tag_set() {
    let router = router();
    for row in rows() {
        let case = row.case;
        let (status, created) = create_rule(&router, create_body(&row)).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201 from CreateRule, got {status} / {created:?}"
        );
        let rule_arn = created["RuleArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateRule reported no RuleArn"))
            .to_string();

        // The two renderers of the member report the same tag SET for the rule as created:
        // one stored tag set, one set of assignments. The create's member is absent where it
        // assigned nothing, which the reader reports as the empty collection it is, so the
        // sets are compared and not the member's presence.
        let (status, listed) = list_tags(&router, &rule_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListTagsForResource, got {status} / {listed:?}"
        );
        assert_eq!(
            tags_of(&listed),
            Some(tags_of(&created).unwrap_or_default()),
            "case {case:?}: CreateRule reported Tags {:?} while ListTagsForResource \
             reported {:?} for the same rule",
            created.get("Tags"),
            listed.get("Tags")
        );

        for step in &row.steps {
            match step {
                Step::Assign(tags) => {
                    let (status, body) = tag_resource(&router, &rule_arn, tags).await;
                    assert_eq!(
                        status, 201,
                        "case {case:?}: expected HTTP 201 from TagResource, \
                         got {status} / {body:?}"
                    );
                }
                Step::Unassign(tag_keys) => {
                    let (status, body) = untag_resource(&router, &rule_arn, tag_keys).await;
                    assert_eq!(
                        status, 204,
                        "case {case:?}: expected HTTP 204 from UntagResource, \
                         got {status} / {body:?}"
                    );
                }
            }
        }

        let (status, listed) = list_tags(&router, &rule_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListTagsForResource, got {status} / {listed:?}"
        );
        // The member is a present list for every tag set, the empty one included, so a
        // client indexing it never misses it.
        assert!(
            listed.get("Tags").is_some_and(Value::is_array),
            "case {case:?}: expected ListTagsForResource to report Tags as a present list, \
             got {:?}",
            listed.get("Tags")
        );
        assert_eq!(
            tags_of(&listed).as_ref(),
            Some(&row.expected),
            "case {case:?}: expected ListTagsForResource to report Tags {:?}, got {:?}",
            row.expected,
            listed.get("Tags")
        );

        // Reporting only: reading the tags changes nothing a later caller sees, so the
        // second read is byte-for-byte the first.
        let (status, again) = list_tags(&router, &rule_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from a repeated ListTagsForResource, \
             got {status} / {again:?}"
        );
        assert_eq!(
            again, listed,
            "case {case:?}: a second ListTagsForResource reported {again:?}, \
             not the {listed:?} the first one did"
        );
    }
}
