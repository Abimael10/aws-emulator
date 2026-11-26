// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for ONE rendering rule, over every response of this service
//! that reports a collection it READS off a rule's record: a modelled LIST member such a
//! response reports is PRESENT on the wire, as `[]` when the resource holds nothing, never
//! omitted.
//!
//! Every list member this service reports is bounded with a minimum of ZERO — `TagList`
//! `0..200`, `ResourceTags` `0..50`, `ExcludeResourceTags` `0..5`, and `ListRules`'
//! `Rules` page — so "holds nothing" is a collection of length zero, a value of the
//! member, not an absence of it.
//! Source: smithy_query(query="operation", operation="CreateRule,GetRule,UpdateRule,
//! LockRule,UnlockRule,ListRules,ListTagsForResource").
//!
//! `CreateRule`'s own `Tags` is NOT such a member: it is the member the create assigns the
//! rule's tags with and echoes back, so a create that assigned none reports no `Tags` at
//! all (`tests/all/rule_status_transitions.rs` owns that rule). Every other collection a
//! create reports is read off the stored record and is present either way, which this
//! suite pins here.
//!
//! The table spans the class the rule generalises over: which operation renders the
//! member (the create, the two single-rule readers, the two lock transitions, the list
//! operation and the tag reader), which member it is (a rule's own tags, both resource
//! selections, a page of rules), and whether the collection is empty or not — a rule
//! created with nothing, a rule whose collection was emptied again, and one carrying
//! entries. `tests/all/reported_tags_member.rs` owns the tag sets of the `Tags` member
//! itself; this suite owns the presence of every collection member a response reads off a
//! record, so no reader can start omitting one.

use serde_json::{json, Value};

use crate::resource_tags::{keys, list_tags, pairs, tag_resource, untag_resource};
use crate::retention_rule_lifecycle::{
    create_rule, get_rule, list_rules, retention, router, supported_retention_max, update_rule,
    RESOURCE_TYPES,
};
use crate::rule_lock_transitions::{lock_rule, unlock_rule};

/// The list members `CreateRule` READS off the stored rule, which it reports present
/// whether or not they hold anything. Its `Tags` member is the create's own echo of the
/// assignments it made and is not one of them.
const CREATE_LIST_MEMBERS: [&str; 2] = ["ResourceTags", "ExcludeResourceTags"];

/// The list members the single-rule readers and the lock transitions report.
const RULE_RECORD_LIST_MEMBERS: [&str; 2] = ["ResourceTags", "ExcludeResourceTags"];

/// Assert `body` carries `member` as a present JSON array holding `expected`.
fn assert_list(case: &str, operation: &str, body: &Value, member: &str, expected: &Value) {
    let reported = body.get(member);
    assert!(
        reported.is_some_and(Value::is_array),
        "case {case:?}: expected {operation} to report {member} as a present list, got {reported:?}"
    );
    assert_eq!(
        reported,
        Some(expected),
        "case {case:?}: expected {operation} to report {member} as {expected:?}, got {reported:?}"
    );
}

/// One row: the case name, the `CreateRule` body, and the contents each list member of
/// the created rule's record must be reported with.
struct Row {
    case: &'static str,
    body: Value,
    tags: Value,
    resource_tags: Value,
    exclude_resource_tags: Value,
}

fn rows() -> Vec<Row> {
    let resource_tags = json!([{ "ResourceTagKey": "retain", "ResourceTagValue": "yes" }]);
    let exclusion_tags = json!([{ "ResourceTagKey": "scratch", "ResourceTagValue": "1" }]);
    let rule_tags = json!([{ "Key": "env", "Value": "prod" }, { "Key": "team", "Value": "storage" }]);
    vec![
        Row {
            case: "a Region-level rule naming no collection at all",
            body: json!({
                "ResourceType": RESOURCE_TYPES[0],
                "RetentionPeriod": retention(7),
            }),
            tags: json!([]),
            resource_tags: json!([]),
            exclude_resource_tags: json!([]),
        },
        Row {
            case: "a Region-level rule sending every collection empty",
            body: json!({
                "ResourceType": RESOURCE_TYPES[1],
                "RetentionPeriod": retention(1),
                "Tags": [],
                "ResourceTags": [],
                "ExcludeResourceTags": [],
            }),
            tags: json!([]),
            resource_tags: json!([]),
            exclude_resource_tags: json!([]),
        },
        Row {
            case: "a tag-level rule carrying resource tags and rule tags",
            body: json!({
                "ResourceType": RESOURCE_TYPES[2],
                "RetentionPeriod": retention(45),
                "ResourceTags": resource_tags.clone(),
                "Tags": rule_tags.clone(),
            }),
            tags: rule_tags,
            resource_tags,
            exclude_resource_tags: json!([]),
        },
        Row {
            case: "a Region-level rule carrying exclusion tags only",
            body: json!({
                "ResourceType": RESOURCE_TYPES[0],
                "RetentionPeriod": retention(supported_retention_max(RESOURCE_TYPES[0])),
                "ExcludeResourceTags": exclusion_tags.clone(),
            }),
            tags: json!([]),
            resource_tags: json!([]),
            exclude_resource_tags: exclusion_tags,
        },
    ]
}

#[tokio::test]
async fn every_reported_list_member_is_present_whether_or_not_it_holds_anything() {
    let router = router();
    for row in rows() {
        let case = row.case;
        let resource_type = row.body["ResourceType"].as_str().expect("a resource type");
        let expected = |member: &str| match member {
            "ResourceTags" => row.resource_tags.clone(),
            _ => row.exclude_resource_tags.clone(),
        };

        let (status, created) = create_rule(&router, row.body.clone()).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201 from CreateRule, got {status} / {created:?}"
        );
        for member in CREATE_LIST_MEMBERS {
            assert_list(case, "CreateRule", &created, member, &expected(member));
        }
        let identifier = created["Identifier"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateRule reported no Identifier"))
            .to_string();
        let rule_arn = created["RuleArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateRule reported no RuleArn"))
            .to_string();

        let (status, listed) = list_tags(&router, &rule_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListTagsForResource, got {status} / {listed:?}"
        );
        assert_list(case, "ListTagsForResource", &listed, "Tags", &row.tags);

        let (status, fetched) = get_rule(&router, &identifier).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetRule, got {status} / {fetched:?}"
        );
        for member in RULE_RECORD_LIST_MEMBERS {
            assert_list(case, "GetRule", &fetched, member, &expected(member));
        }

        let (status, updated) = update_rule(
            &router,
            &identifier,
            json!({ "Description": "a description this rule did not have" }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateRule, got {status} / {updated:?}"
        );
        for member in RULE_RECORD_LIST_MEMBERS {
            assert_list(case, "UpdateRule", &updated, member, &expected(member));
        }

        let (status, page) = list_rules(&router, json!({ "ResourceType": resource_type })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListRules, got {status} / {page:?}"
        );
        let reported = page.get("Rules");
        assert!(
            reported.is_some_and(Value::is_array),
            "case {case:?}: expected ListRules to report Rules as a present list, got {reported:?}"
        );
        assert!(
            reported
                .and_then(Value::as_array)
                .expect("Rules checked to be a list just above")
                .iter()
                .any(|rule| rule["Identifier"].as_str() == Some(identifier.as_str())),
            "case {case:?}: expected the created rule in the page, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_collection_emptied_again_is_still_reported_as_a_present_list() {
    let router = router();
    // The tag set is the one collection a caller can empty again after the create, so it
    // is the one case where "holds nothing" is reached by a later call rather than by the
    // create: the member is reported exactly as it is for a rule that never held a tag.
    let assigned = pairs(&[("env", "dev"), ("team", "platform")]);
    let (status, created) = create_rule(
        &router,
        json!({
            "ResourceType": RESOURCE_TYPES[1],
            "RetentionPeriod": retention(supported_retention_max(RESOURCE_TYPES[1])),
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "fixture CreateRule returned {status} / {created:?}"
    );
    let rule_arn = created["RuleArn"].as_str().expect("a minted RuleArn").to_string();

    let (status, body) = tag_resource(&router, &rule_arn, &assigned).await;
    assert_eq!(
        status, 201,
        "fixture TagResource returned {status} / {body:?}"
    );
    let (status, body) = untag_resource(&router, &rule_arn, &keys(&["env", "team"])).await;
    assert_eq!(
        status, 204,
        "fixture UntagResource returned {status} / {body:?}"
    );

    let (status, listed) = list_tags(&router, &rule_arn).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from ListTagsForResource, got {status} / {listed:?}"
    );
    assert_list(
        "a tag set emptied by UntagResource",
        "ListTagsForResource",
        &listed,
        "Tags",
        &json!([]),
    );
}

#[tokio::test]
async fn the_lock_transitions_report_their_list_members_as_present_lists() {
    let router = router();
    // Only a Region-level rule with no exclusion tags can be locked, so the lockable
    // shape is the one whose collections are all empty — exactly the shape the rule is
    // about.
    let (status, created) = create_rule(
        &router,
        json!({
            "ResourceType": RESOURCE_TYPES[0],
            "RetentionPeriod": retention(7),
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "fixture CreateRule returned {status} / {created:?}"
    );
    let identifier = created["Identifier"]
        .as_str()
        .expect("a minted Identifier")
        .to_string();

    let (status, locked) = lock_rule(
        &router,
        &identifier,
        json!({ "LockConfiguration": { "UnlockDelay": { "UnlockDelayValue": 7, "UnlockDelayUnit": "DAYS" } } }),
    )
    .await;
    assert_eq!(status, 200, "LockRule returned {status} / {locked:?}");
    for member in RULE_RECORD_LIST_MEMBERS {
        assert_list("a locked Region-level rule", "LockRule", &locked, member, &json!([]));
    }

    let (status, unlocked) = unlock_rule(&router, &identifier).await;
    assert_eq!(status, 200, "UnlockRule returned {status} / {unlocked:?}");
    for member in RULE_RECORD_LIST_MEMBERS {
        assert_list(
            "an unlocked Region-level rule",
            "UnlockRule",
            &unlocked,
            member,
            &json!([]),
        );
    }
}

#[tokio::test]
async fn a_region_holding_no_rules_reports_an_empty_page() {
    let router = router();
    // The lower bound of the one page member: a Region with nothing in it reports the
    // page as a present empty list, not as a response without a page.
    for resource_type in RESOURCE_TYPES {
        let (status, page) = list_rules(&router, json!({ "ResourceType": resource_type })).await;
        assert_eq!(
            status, 200,
            "case {resource_type:?}: expected HTTP 200 from ListRules, got {status} / {page:?}"
        );
        assert_list(resource_type, "ListRules", &page, "Rules", &json!([]));
    }
}
