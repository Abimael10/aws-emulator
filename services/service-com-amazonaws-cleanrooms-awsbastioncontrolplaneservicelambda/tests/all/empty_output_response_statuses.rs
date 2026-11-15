// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the status and body of a successful call to an operation of this
//! service whose OUTPUT SHAPE MODELS NO MEMBERS.
//!
//! The rule under test: such a call has nothing to serialize, so it answers HTTP 204 No
//! Content and carries no member for a client to read, while an operation whose output
//! shape does model members answers 200 with that body. The rows span what the rule
//! generalises over — the operation (`TagResource`, `UntagResource` and every implemented
//! `Delete*` operation of this service), the kind of resource addressed, and the size and
//! shape of the tag set a mutation carries (the empty map, one key, several keys, keys
//! that need percent-encoding, a key already carried, a key not carried) — and each
//! tagging row also reads the resource back, so a no-content answer is never mistaken for
//! a call that did nothing.
//!
//! The request helpers, and the tag read that checks a mutation took effect, are the
//! sibling `resource_tags` suite's: both suites are modules of one test target, so the
//! untag query encoding and the tag read have one definition.
//!
//! Every request goes over the wire through the same `MultiServiceRouter` the production
//! router uses.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::cleanrooms_collaboration_lifecycle::{
    account_id, minimal_collaboration, Emulator, ALL_LOG_STATUSES, ALL_MEMBER_ABILITIES,
};
use super::cleanrooms_configured_table::{
    create_body, ALL_ANALYSIS_METHODS, ALL_TABLE_REFERENCE_TYPES,
};
use super::collaboration_change_requests::{declared_member, PARTNER_ACCOUNT};
use super::resource_tags::{as_map, listed_tags};

/// The status an operation of this service whose output shape models no members answers
/// on success: there is no entity to send, which is what the no-content status states.
///
/// Every operation of this service with an empty output shape whose `@http` code the model
/// states pins this code (the `Delete*` operations); the tagging operations' reported code
/// is restJson1's default rather than a stated one, and the served plane answers the same
/// no-content status.
pub(crate) const EMPTY_OUTPUT_SUCCESS_STATUS: u16 = 204;

/// The status an operation whose output shape DOES model members answers, for the setup
/// and contrast rows: the rule is keyed to the output shape being empty, not to the
/// operation's path.
const POPULATED_OUTPUT_SUCCESS_STATUS: u16 = 200;

/// Whether a response body carries no member for a client to read: either no body at all,
/// or the empty object restJson1 writes for an output structure with no members.
fn carries_no_member(body: &Value) -> bool {
    match body {
        Value::Null => true,
        Value::Object(members) => members.is_empty(),
        _ => false,
    }
}

/// Assert one answer is an empty-output success: the no-content status, and nothing to
/// read in the body.
pub(crate) fn assert_no_content(case: &str, status: u16, body: &Value) {
    assert_eq!(
        status, EMPTY_OUTPUT_SUCCESS_STATUS,
        "case {case:?}: an empty output shape must answer {EMPTY_OUTPUT_SUCCESS_STATUS}, \
         got {status} with {body}"
    );
    assert!(
        carries_no_member(body),
        "case {case:?}: a no-content answer must carry no member to read, got {body}"
    );
}

/// One write this table drives a resource through.
#[derive(Debug)]
enum Mutation {
    /// `TagResource` with this map.
    Tag(Value),
    /// `UntagResource` with these keys.
    Untag(Vec<String>),
}

#[tokio::test]
async fn every_tag_mutation_answers_no_content_with_no_body_and_still_takes_effect() {
    let emulator = Emulator::new();

    // Three kinds of resource, one created WITH tags and two without, so the rule is not
    // read off a single resource kind.
    let mut body = minimal_collaboration("no-content-collaboration");
    body["tags"] = json!({ "env": "test" });
    let (status, created) = emulator.create_collaboration(body).await;
    assert_eq!(
        status, POPULATED_OUTPUT_SUCCESS_STATUS,
        "setup: expected CreateCollaboration {POPULATED_OUTPUT_SUCCESS_STATUS}, got {status} with {created}"
    );
    let collaboration_id = created["collaboration"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no collaboration.id in {created}"))
        .to_string();
    let collaboration_arn = created["collaboration"]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no collaboration.arn in {created}"))
        .to_string();

    let (status, joined) = emulator
        .create_membership(json!({
            "collaborationIdentifier": collaboration_id,
            "queryLogStatus": ALL_LOG_STATUSES[0],
        }))
        .await;
    assert_eq!(
        status, POPULATED_OUTPUT_SUCCESS_STATUS,
        "setup: expected CreateMembership {POPULATED_OUTPUT_SUCCESS_STATUS}, got {status} with {joined}"
    );
    let membership_arn = joined["membership"]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no membership.arn in {joined}"))
        .to_string();

    let (status, table) = emulator
        .create_configured_table(create_body(
            "no-content-table",
            ALL_TABLE_REFERENCE_TYPES[0],
            ALL_ANALYSIS_METHODS[0],
            None,
            1,
        ))
        .await;
    assert_eq!(
        status, POPULATED_OUTPUT_SUCCESS_STATUS,
        "setup: expected CreateConfiguredTable {POPULATED_OUTPUT_SUCCESS_STATUS}, got {status} with {table}"
    );
    let table_arn = table["configuredTable"]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no configuredTable.arn in {table}"))
        .to_string();

    // What each resource carries, tracked so every row can read its resource back.
    let mut carried: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    carried.insert(
        collaboration_arn.clone(),
        BTreeMap::from([("env".to_string(), "test".to_string())]),
    );
    carried.insert(membership_arn.clone(), BTreeMap::new());
    carried.insert(table_arn.clone(), BTreeMap::new());

    // Rows vary the operation, the resource and the tag set: the empty map, one key,
    // several keys including ones that need percent-encoding in the untag query, a key
    // already carried (a replacement), a key the resource does not carry, and the same
    // removal twice.
    let rows: Vec<(&str, &str, Mutation)> = vec![
        (
            "TagResource with one new key",
            &collaboration_arn,
            Mutation::Tag(json!({ "owner": "fixer" })),
        ),
        (
            "TagResource with the empty map",
            &collaboration_arn,
            Mutation::Tag(json!({})),
        ),
        (
            "TagResource replacing a carried key's value",
            &collaboration_arn,
            Mutation::Tag(json!({ "env": "staging" })),
        ),
        (
            "TagResource with three keys that need percent-encoding in the untag query",
            &membership_arn,
            Mutation::Tag(
                json!({ "cost:center": "cc-1", "app/name+1": "search", "two words": "yes" }),
            ),
        ),
        (
            "UntagResource removing one carried key",
            &collaboration_arn,
            Mutation::Untag(vec!["owner".to_string()]),
        ),
        (
            "UntagResource removing a key the resource does not carry",
            &collaboration_arn,
            Mutation::Untag(vec!["never-set".to_string()]),
        ),
        (
            "UntagResource removing three keys at once",
            &membership_arn,
            Mutation::Untag(vec![
                "cost:center".to_string(),
                "app/name+1".to_string(),
                "two words".to_string(),
            ]),
        ),
        (
            "UntagResource removing the same keys again",
            &membership_arn,
            Mutation::Untag(vec!["cost:center".to_string()]),
        ),
        (
            "TagResource on a resource created with no tags",
            &table_arn,
            Mutation::Tag(json!({ "tier": "bronze" })),
        ),
        (
            "UntagResource removing the only key a resource carries",
            &table_arn,
            Mutation::Untag(vec!["tier".to_string()]),
        ),
    ];

    for (case, arn, mutation) in rows {
        let (status, body) = match &mutation {
            Mutation::Tag(tags) => emulator.tag_resource(arn, tags.clone()).await,
            Mutation::Untag(keys) => emulator.untag_resource(arn, keys).await,
        };
        assert_no_content(case, status, &body);

        // The write still happened: the resource reads back with exactly the set the
        // mutation leaves, and no other resource moved.
        let expected = carried.get_mut(arn).expect("a tracked resource");
        match &mutation {
            Mutation::Tag(tags) => expected.extend(as_map(tags)),
            Mutation::Untag(keys) => {
                for key in keys {
                    expected.remove(key);
                }
            }
        }
        for (resource_arn, want) in &carried {
            let listed = listed_tags(&emulator, case, resource_arn).await;
            assert_eq!(
                &listed, want,
                "case {case:?}: {resource_arn} must carry {want:?}, got {listed:?}"
            );
        }
    }
}

#[tokio::test]
async fn every_implemented_delete_with_an_empty_output_shape_answers_no_content() {
    let emulator = Emulator::new();

    // A collaboration with one other party declared, so the member row has a grant to
    // remove that is not the creator's own.
    let mut body = minimal_collaboration("no-content-deletes");
    body["members"] = json!([declared_member(PARTNER_ACCOUNT, &[ALL_MEMBER_ABILITIES[0]])]);
    let (status, created) = emulator.create_collaboration(body).await;
    assert_eq!(
        status, POPULATED_OUTPUT_SUCCESS_STATUS,
        "setup: expected CreateCollaboration {POPULATED_OUTPUT_SUCCESS_STATUS}, got {status} with {created}"
    );
    let collaboration_id = created["collaboration"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no collaboration.id in {created}"))
        .to_string();

    let (status, joined) = emulator
        .create_membership(json!({
            "collaborationIdentifier": collaboration_id,
            "queryLogStatus": ALL_LOG_STATUSES[1],
        }))
        .await;
    assert_eq!(
        status, POPULATED_OUTPUT_SUCCESS_STATUS,
        "setup: expected CreateMembership {POPULATED_OUTPUT_SUCCESS_STATUS}, got {status} with {joined}"
    );
    let membership_id = joined["membership"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no membership.id in {joined}"))
        .to_string();

    let (status, analysis_template) = emulator
        .call(
            "POST",
            &format!("/memberships/{membership_id}/analysistemplates"),
            Some(json!({
                "name": "no-content",
                "format": "SQL",
                "source": { "text": "SELECT id FROM sales" },
            })),
        )
        .await;
    assert_eq!(
        status, POPULATED_OUTPUT_SUCCESS_STATUS,
        "setup: expected CreateAnalysisTemplate {POPULATED_OUTPUT_SUCCESS_STATUS}, got {status} with {analysis_template}"
    );
    let analysis_template_id = analysis_template["analysisTemplate"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no analysisTemplate.id in {analysis_template}"))
        .to_string();

    let (status, budget_template) = emulator
        .call(
            "POST",
            &format!("/memberships/{membership_id}/privacybudgettemplates"),
            Some(json!({
                "privacyBudgetType": "DIFFERENTIAL_PRIVACY",
                "parameters": { "differentialPrivacy": { "epsilon": 7, "usersNoisePerQuery": 42 } },
            })),
        )
        .await;
    assert_eq!(
        status, POPULATED_OUTPUT_SUCCESS_STATUS,
        "setup: expected CreatePrivacyBudgetTemplate {POPULATED_OUTPUT_SUCCESS_STATUS}, got {status} with {budget_template}"
    );
    let budget_template_id = budget_template["privacyBudgetTemplate"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no privacyBudgetTemplate.id in {budget_template}"))
        .to_string();

    let (status, table) = emulator
        .create_configured_table(create_body(
            "no-content-rule-table",
            ALL_TABLE_REFERENCE_TYPES[0],
            ALL_ANALYSIS_METHODS[0],
            None,
            3,
        ))
        .await;
    assert_eq!(
        status, POPULATED_OUTPUT_SUCCESS_STATUS,
        "setup: expected CreateConfiguredTable {POPULATED_OUTPUT_SUCCESS_STATUS}, got {status} with {table}"
    );
    let table_id = table["configuredTable"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no configuredTable.id in {table}"))
        .to_string();

    // The one analysis-rule type whose policy needs no column arithmetic to be valid
    // (`AnalysisRuleCustom`: allowedAnalyses is required, and the provider list with it
    // for ANY_QUERY).
    let rule_type = "CUSTOM";
    let (status, rule) = emulator
        .create_analysis_rule(
            &table_id,
            rule_type,
            json!({
                "v1": {
                    "custom": {
                        "allowedAnalyses": ["ANY_QUERY"],
                        "allowedAnalysisProviders": [account_id()],
                    }
                }
            }),
        )
        .await;
    assert_eq!(
        status, POPULATED_OUTPUT_SUCCESS_STATUS,
        "setup: expected CreateConfiguredTableAnalysisRule {POPULATED_OUTPUT_SUCCESS_STATUS}, got {status} with {rule}"
    );

    // An association of that configured table, and a collaboration analysis rule on it,
    // so the association plane's analysis-rule delete has a row below too.
    let (status, associated) = emulator
        .call(
            "POST",
            &format!("/memberships/{membership_id}/configuredTableAssociations"),
            Some(json!({
                "name": "no_content_association",
                "configuredTableIdentifier": table_id,
                "roleArn": format!("arn:aws:iam::{}:role/clean-rooms-table-role", account_id()),
            })),
        )
        .await;
    assert_eq!(
        status, POPULATED_OUTPUT_SUCCESS_STATUS,
        "setup: expected CreateConfiguredTableAssociation {POPULATED_OUTPUT_SUCCESS_STATUS}, got {status} with {associated}"
    );
    let association_id = associated["configuredTableAssociation"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no configuredTableAssociation.id in {associated}"))
        .to_string();
    let (status, association_rule) = emulator
        .call(
            "POST",
            &format!(
                "/memberships/{membership_id}/configuredTableAssociations/{association_id}/analysisRule"
            ),
            Some(json!({
                "analysisRuleType": rule_type,
                "analysisRulePolicy": { "v1": { "custom": {} } },
            })),
        )
        .await;
    assert_eq!(
        status, POPULATED_OUTPUT_SUCCESS_STATUS,
        "setup: expected CreateConfiguredTableAssociationAnalysisRule {POPULATED_OUTPUT_SUCCESS_STATUS}, got {status} with {association_rule}"
    );

    // One row per `Delete*` operation of this service that is implemented — the seven
    // others still carry the scaffold header and answer 501, so they serve no success
    // status at all. A child goes before its parent: the collaboration's creator cannot
    // leave while the collaboration is still there, and a membership survives it.
    let rows: [(&str, String); 7] = [
        (
            "DeleteAnalysisTemplate",
            format!("/memberships/{membership_id}/analysistemplates/{analysis_template_id}"),
        ),
        (
            "DeletePrivacyBudgetTemplate",
            format!("/memberships/{membership_id}/privacybudgettemplates/{budget_template_id}"),
        ),
        (
            "DeleteConfiguredTableAnalysisRule",
            format!("/configuredTables/{table_id}/analysisRule/{rule_type}"),
        ),
        (
            "DeleteConfiguredTableAssociationAnalysisRule",
            format!(
                "/memberships/{membership_id}/configuredTableAssociations/{association_id}/analysisRule/{rule_type}"
            ),
        ),
        (
            "DeleteMember",
            format!("/collaborations/{collaboration_id}/member/{PARTNER_ACCOUNT}"),
        ),
        (
            "DeleteCollaboration",
            format!("/collaborations/{collaboration_id}"),
        ),
        ("DeleteMembership", format!("/memberships/{membership_id}")),
    ];

    for (case, path) in rows {
        let (status, body) = emulator.call("DELETE", &path, None).await;
        assert_no_content(case, status, &body);
    }
}
