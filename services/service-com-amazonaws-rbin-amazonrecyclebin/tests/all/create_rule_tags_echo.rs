// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for ONE rule: `CreateRule` echoes the tag assignments the create
//! made, and reports NO `Tags` member when it assigned none.
//!
//! `CreateRuleRequest$Tags` is "information about the tags to assign to the retention rule"
//! and `CreateRuleResponse$Tags` is "information about the tags assigned to the retention
//! rule" — the same member on both sides of the one call
//! (smithy_query(query="operation", operation="CreateRule")), so the create answers the
//! assignments it made and a create that assigned nothing has no assignment to report.
//! The rule's own tag COLLECTION is a different question, answered by the reader
//! (`ListTagsForResource`), which reports it as a present list however empty it is —
//! `tests/all/reported_tags_member.rs` owns that half.
//!
//! The table spans what the rule generalises over: how the create states the member (absent,
//! explicitly empty, one tag, several, the published quota), the tag values it carries (an
//! empty value, non-ASCII text) and the rule shape it creates (Region-level, tag-level,
//! exclusion-tagged), with each row read back through the tag reader so the echo and the
//! stored collection can only agree.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::resource_tags::{list_tags, pairs};
use crate::retention_rule_lifecycle::{create_rule, retention, router, RESOURCE_TYPES};
use crate::rule_status_transitions::tags_of;
use service_com_amazonaws_rbin_amazonrecyclebin::state::MAX_TAGS_PER_RULE;

/// One row: the case, the `Tags` member the create states (`None` = no member at all), the
/// other members the create carries, and the tag set the rule ends up holding.
struct Row {
    case: &'static str,
    resource_type: &'static str,
    assigned: Option<Vec<(String, String)>>,
    selection: Value,
    expected: BTreeMap<String, String>,
}

/// The tag set a row expects, as a map: a `TagList` holds one value per key.
fn map_of(tags: &[(String, String)]) -> BTreeMap<String, String> {
    tags.iter().cloned().collect()
}

/// Tag pairs filling the published per-rule quota
/// (`service_com_amazonaws_rbin_amazonrecyclebin::state::MAX_TAGS_PER_RULE`).
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
    let non_ascii = pairs(&[("équipe", "plateforme-européenne")]);
    let quota = quota_pairs();
    vec![
        Row {
            case: "a Region-level rule assigning no tag at all",
            resource_type: RESOURCE_TYPES[0],
            assigned: None,
            selection: json!({}),
            expected: BTreeMap::new(),
        },
        Row {
            case: "a Region-level rule assigning an explicitly empty list",
            resource_type: RESOURCE_TYPES[1],
            assigned: Some(Vec::new()),
            selection: json!({}),
            expected: BTreeMap::new(),
        },
        Row {
            case: "a tag-level rule assigning no tag at all",
            resource_type: RESOURCE_TYPES[2],
            assigned: None,
            selection: json!({
                "ResourceTags": [{ "ResourceTagKey": "retain", "ResourceTagValue": "yes" }],
            }),
            expected: BTreeMap::new(),
        },
        Row {
            case: "a Region-level rule with exclusion tags assigning no tag at all",
            resource_type: RESOURCE_TYPES[0],
            assigned: None,
            selection: json!({
                "ExcludeResourceTags": [{ "ResourceTagKey": "scratch", "ResourceTagValue": "1" }],
            }),
            expected: BTreeMap::new(),
        },
        Row {
            case: "a rule assigning one tag",
            resource_type: RESOURCE_TYPES[0],
            assigned: Some(one.clone()),
            selection: json!({}),
            expected: map_of(&one),
        },
        Row {
            case: "a rule assigning several tags",
            resource_type: RESOURCE_TYPES[1],
            assigned: Some(several.clone()),
            selection: json!({}),
            expected: map_of(&several),
        },
        Row {
            case: "a rule assigning a tag whose value is empty",
            resource_type: RESOURCE_TYPES[2],
            assigned: Some(empty_value.clone()),
            selection: json!({}),
            expected: map_of(&empty_value),
        },
        Row {
            case: "a rule assigning a non-ASCII key and value",
            resource_type: RESOURCE_TYPES[0],
            assigned: Some(non_ascii.clone()),
            selection: json!({}),
            expected: map_of(&non_ascii),
        },
        Row {
            case: "a tag-level rule assigning the quota of tags",
            resource_type: RESOURCE_TYPES[1],
            assigned: Some(quota.clone()),
            selection: json!({
                "ResourceTags": [{ "ResourceTagKey": "retain", "ResourceTagValue": "quota" }],
            }),
            expected: map_of(&quota),
        },
    ]
}

/// The create body a row asks for: the smallest rule `CreateRule` accepts, the row's own
/// selection members, and the `Tags` member only where the row states one.
fn create_body(row: &Row) -> Value {
    let mut body = json!({
        "ResourceType": row.resource_type,
        "RetentionPeriod": retention(7),
    });
    if let Value::Object(members) = row.selection.clone() {
        for (member, value) in members {
            body[member] = value;
        }
    }
    if let Some(assigned) = &row.assigned {
        body["Tags"] = Value::Array(
            assigned
                .iter()
                .map(|(key, value)| json!({ "Key": key, "Value": value }))
                .collect(),
        );
    }
    body
}

#[tokio::test]
async fn create_rule_echoes_the_tags_it_assigned_and_omits_the_member_when_it_assigned_none() {
    let router = router();
    for row in rows() {
        let case = row.case;
        let (status, created) = create_rule(&router, create_body(&row)).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201 from CreateRule, got {status} / {created:?}"
        );

        if row.expected.is_empty() {
            assert_eq!(
                created.get("Tags"),
                None,
                "case {case:?}: a create that assigned no tag reports no Tags member, got {:?}",
                created.get("Tags")
            );
        } else {
            assert!(
                created.get("Tags").is_some_and(Value::is_array),
                "case {case:?}: expected CreateRule to echo Tags as a list, got {:?}",
                created.get("Tags")
            );
            assert_eq!(
                tags_of(&created),
                Some(row.expected.clone()),
                "case {case:?}: expected CreateRule to echo Tags {:?}, got {:?}",
                row.expected,
                created.get("Tags")
            );
        }

        // The resource SELECTION members are read off the stored record rather than echoed,
        // so they are reported whether or not the rule names any.
        for member in ["ResourceTags", "ExcludeResourceTags"] {
            assert!(
                created.get(member).is_some_and(Value::is_array),
                "case {case:?}: expected CreateRule to report {member} as a list, got {:?}",
                created.get(member)
            );
        }

        // ROUND TRIP: the tag reader reports the stored collection of the same rule, which
        // holds exactly what the create assigned — as the present list a collection is.
        let rule_arn = created["RuleArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateRule reported no RuleArn"))
            .to_string();
        let (status, listed) = list_tags(&router, &rule_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListTagsForResource, got {status} / {listed:?}"
        );
        assert!(
            listed.get("Tags").is_some_and(Value::is_array),
            "case {case:?}: expected ListTagsForResource to report Tags as a present list, \
             got {:?}",
            listed.get("Tags")
        );
        assert_eq!(
            tags_of(&listed),
            Some(row.expected.clone()),
            "case {case:?}: expected ListTagsForResource to report {:?}, got {:?}",
            row.expected,
            listed.get("Tags")
        );
    }
}
