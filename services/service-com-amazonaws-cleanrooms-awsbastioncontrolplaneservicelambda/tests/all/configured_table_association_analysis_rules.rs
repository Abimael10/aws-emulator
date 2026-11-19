// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Clean Rooms configured-table-association analysis-rule slice
//! (`CreateConfiguredTableAssociationAnalysisRule`,
//! `UpdateConfiguredTableAssociationAnalysisRule` +
//! `GetConfiguredTableAssociationAnalysisRule`).
//!
//! The rules under test:
//!
//! * a create PERSISTS the rule and reports the association by the identifiers the
//!   request ADDRESSED it with — `membershipIdentifier` and
//!   `configuredTableAssociationId` are the path's `@httpLabel` values and
//!   `configuredTableAssociationArn` the membership-qualified ARN of that pair (the
//!   identity rule is pinned in `configured_table_association_analysis_rule_identity`) —
//!   and both timestamps are the creation instant, so the read after it reports exactly the
//!   record the create persisted, policy member for policy member, and the association
//!   itself reports the rule's type in `analysisRuleTypes`;
//! * each member of the `ConfiguredTableAssociationAnalysisRulePolicyV1` union is the
//!   policy of exactly one `ConfiguredTableAssociationAnalysisRuleType`, so a request
//!   whose `analysisRuleType` names another type is refused with the modelled 400
//!   `ValidationException` and persists nothing;
//! * a rule is identified by the `(configured table association, analysis rule type)`
//!   pair the request path carries, so a second create under a type the association
//!   already holds a rule for is a 409 `ConflictException` and leaves the stored rule
//!   as it was;
//! * an association is a sub-resource of ONE membership, so an identifier no create
//!   minted — the membership or the association — and an association of ANOTHER
//!   membership are the modelled 404 `ResourceNotFoundException`, as is a pair the store
//!   holds no rule for;
//! * an update REPLACES the policy of the rule the request's triple addresses and moves
//!   only that rule's `updateTime`, keeping its `createTime` and the identifiers it
//!   reports, so a later read reports the new policy; it keeps the same pairing of
//!   `analysisRuleType` and policy member the create keeps, and a triple that holds no
//!   rule is the modelled 404 rather than a rule this operation mints.
//!
//! The tables vary what those rules generalise over: every rule type in the model's
//! catalog, every (declared type, policy type) pair, and for the policies the optional
//! members one may carry (absent, present, how many entries, a wildcard entry). Every
//! request goes over the wire through the same `MultiServiceRouter` the production router
//! uses.

use serde_json::{json, Value};

use super::cleanrooms_collaboration_lifecycle::{
    account_id, error_code, error_message, is_model_uuid, minimal_collaboration, Emulator, REGION,
};
use super::cleanrooms_configured_table::{create_body, COLUMN_NAMES};

/// Every `ConfiguredTableAssociationAnalysisRuleType` the model declares (shape
/// `ConfiguredTableAssociationAnalysisRuleType`: AGGREGATION | LIST | CUSTOM).
pub(crate) const ALL_RULE_TYPES: [&str; 3] = ["AGGREGATION", "LIST", "CUSTOM"];

/// An identifier in the form the model's `MembershipIdentifier` /
/// `ConfiguredTableAssociationIdentifier` pattern declares that no create here mints.
const ABSENT_UUID: &str = "00000000-0000-0000-0000-000000000000";

/// An account id that is not the caller's, twelve digits as `AccountId` declares.
const OTHER_RESULT_RECEIVER: &str = "210987654321";

/// The INTERNAL name the service reports a rule type by when it refuses a read: the
/// catalog value as one CamelCase word, except LIST, whose rule class is the
/// intersection-list rule and is named for it. Catalog source: model shape
/// `ConfiguredTableAssociationAnalysisRuleType` (AGGREGATION | LIST | CUSTOM).
pub(crate) fn internal_rule_type_name(rule_type: &str) -> &'static str {
    match rule_type {
        "AGGREGATION" => "Aggregation",
        "LIST" => "IntersectionList",
        "CUSTOM" => "Custom",
        other => panic!("no internal name for analysis rule type {other:?}"),
    }
}

/// The union member of `ConfiguredTableAssociationAnalysisRulePolicyV1` that carries the
/// policy of `rule_type`.
fn policy_member(rule_type: &str) -> &'static str {
    match rule_type {
        "AGGREGATION" => "aggregation",
        "LIST" => "list",
        "CUSTOM" => "custom",
        other => panic!("no policy member for analysis rule type {other:?}"),
    }
}

/// An `AdditionalAnalysesResourceArn` of this membership, in the form the model's pattern
/// declares
/// (`^arn:aws:cleanrooms:[\w]{2}-[\w]{4,9}-[\d]:([\d]{12}|\*):membership\/[\*\d\w-]+\/configuredaudiencemodelassociation\/[\*\d\w-]+$`).
fn additional_analysis_arn(membership_id: &str, association: &str) -> String {
    format!(
        "arn:aws:cleanrooms:{REGION}:{}:membership/{membership_id}/configuredaudiencemodelassociation/{association}",
        account_id()
    )
}

/// A `ConfiguredTableAssociationAnalysisRulePolicy` of `rule_type`. Every member of the
/// three policy shapes is optional (`allowedResultReceivers`, `allowedAdditionalAnalyses`),
/// so each is present only when given.
pub(crate) fn association_policy(
    rule_type: &str,
    allowed_result_receivers: Option<&[&str]>,
    allowed_additional_analyses: Option<&[String]>,
) -> Value {
    let mut policy = json!({});
    if let Some(receivers) = allowed_result_receivers {
        policy["allowedResultReceivers"] = json!(receivers.to_vec());
    }
    if let Some(analyses) = allowed_additional_analyses {
        policy["allowedAdditionalAnalyses"] = json!(analyses.to_vec());
    }
    json!({ "v1": { policy_member(rule_type): policy } })
}

impl Emulator {
    pub(crate) async fn create_association_analysis_rule(
        &self,
        membership_id: &str,
        association_id: &str,
        rule_type: &str,
        policy: Value,
    ) -> (u16, Value) {
        self.call(
            "POST",
            &format!(
                "/memberships/{membership_id}/configuredTableAssociations/{association_id}/analysisRule"
            ),
            Some(json!({ "analysisRuleType": rule_type, "analysisRulePolicy": policy })),
        )
        .await
    }

    pub(crate) async fn get_association_analysis_rule(
        &self,
        membership_id: &str,
        association_id: &str,
        rule_type: &str,
    ) -> (u16, Value) {
        self.call(
            "GET",
            &format!(
                "/memberships/{membership_id}/configuredTableAssociations/{association_id}/analysisRule/{rule_type}"
            ),
            None,
        )
        .await
    }

    pub(crate) async fn update_association_analysis_rule(
        &self,
        membership_id: &str,
        association_id: &str,
        rule_type: &str,
        policy: Value,
    ) -> (u16, Value) {
        self.call(
            "PATCH",
            &format!(
                "/memberships/{membership_id}/configuredTableAssociations/{association_id}/analysisRule/{rule_type}"
            ),
            Some(json!({ "analysisRulePolicy": policy })),
        )
        .await
    }

    pub(crate) async fn read_association(&self, membership_id: &str, association_id: &str) -> (u16, Value) {
        self.call(
            "GET",
            &format!("/memberships/{membership_id}/configuredTableAssociations/{association_id}"),
            None,
        )
        .await
    }
}

/// One stored configured table association: the membership that holds it, its id and its
/// ARN.
struct Association {
    membership_id: String,
    id: String,
    arn: String,
}

/// A collaboration, the caller's membership in it, a configured table and an association
/// of that table under that membership.
async fn association(emulator: &Emulator, name: &str) -> Association {
    let (status, created) = emulator
        .create_collaboration(minimal_collaboration(name))
        .await;
    assert_eq!(
        status, 200,
        "setup {name:?}: expected CreateCollaboration 200, got {status} with {created}"
    );
    let collaboration_id = created["collaboration"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup {name:?}: no collaboration.id in {created}"))
        .to_string();

    let (status, joined) = emulator
        .create_membership(json!({
            "collaborationIdentifier": collaboration_id,
            "queryLogStatus": "ENABLED",
        }))
        .await;
    assert_eq!(
        status, 200,
        "setup {name:?}: expected CreateMembership 200, got {status} with {joined}"
    );
    let membership_id = joined["membership"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup {name:?}: no membership.id in {joined}"))
        .to_string();

    let (status, table) = emulator
        .create_configured_table(create_body(
            &format!("{name}_table"),
            "GLUE",
            "DIRECT_QUERY",
            None,
            COLUMN_NAMES.len(),
        ))
        .await;
    assert_eq!(
        status, 200,
        "setup {name:?}: expected CreateConfiguredTable 200, got {status} with {table}"
    );
    let table_id = table["configuredTable"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup {name:?}: no configuredTable.id in {table}"))
        .to_string();

    let (status, associated) = emulator
        .call(
            "POST",
            &format!("/memberships/{membership_id}/configuredTableAssociations"),
            Some(json!({
                "name": "sales",
                "configuredTableIdentifier": table_id,
                "roleArn": format!("arn:aws:iam::{}:role/clean-rooms-table-role", account_id()),
            })),
        )
        .await;
    assert_eq!(
        status, 200,
        "setup {name:?}: expected CreateConfiguredTableAssociation 200, got {status} with \
         {associated}"
    );
    let association = &associated["configuredTableAssociation"];
    Association {
        membership_id,
        id: association["id"]
            .as_str()
            .unwrap_or_else(|| panic!("setup {name:?}: no id in {associated}"))
            .to_string(),
        arn: association["arn"]
            .as_str()
            .unwrap_or_else(|| panic!("setup {name:?}: no arn in {associated}"))
            .to_string(),
    }
}

/// One row of the create/read round-trip table: a rule type and the optional members its
/// policy carries.
struct Case {
    label: String,
    rule_type: &'static str,
    /// `None` leaves `allowedResultReceivers` out of the policy entirely.
    receivers: Option<Vec<&'static str>>,
    /// How many `allowedAdditionalAnalyses` entries the policy carries, and whether the
    /// last of them is the wildcard the published pattern admits. `None` leaves the
    /// member out.
    additional: Option<(usize, bool)>,
}

/// One shape of the optional members a policy may take, shared by every rule type.
struct PolicyShape {
    label: &'static str,
    /// `None` leaves `allowedResultReceivers` out of the policy entirely.
    receivers: Option<Vec<&'static str>>,
    /// How many `allowedAdditionalAnalyses` entries the policy carries, and whether the
    /// last of them is a wildcard. `None` leaves the member out.
    additional: Option<(usize, bool)>,
}

/// Every rule type crossed with the optional-member shapes a policy may take: both
/// members absent, one present and the other absent, both present, one entry and several,
/// and a wildcard entry.
fn round_trip_cases() -> Vec<Case> {
    let shapes = [
        PolicyShape {
            label: "no optional member",
            receivers: None,
            additional: None,
        },
        PolicyShape {
            label: "one result receiver",
            receivers: Some(vec![OTHER_RESULT_RECEIVER]),
            additional: None,
        },
        PolicyShape {
            label: "no receivers, one additional analysis",
            receivers: None,
            additional: Some((1, false)),
        },
        PolicyShape {
            label: "two result receivers and two additional analyses",
            receivers: Some(vec![OTHER_RESULT_RECEIVER, account_id()]),
            additional: Some((2, false)),
        },
        PolicyShape {
            label: "an empty receiver list and a wildcard additional analysis",
            receivers: Some(vec![]),
            additional: Some((1, true)),
        },
    ];
    ALL_RULE_TYPES
        .iter()
        .flat_map(|rule_type| {
            shapes.iter().map(move |shape| Case {
                label: format!("analysisRuleType={rule_type}, {}", shape.label),
                rule_type,
                receivers: shape.receivers.clone(),
                additional: shape.additional,
            })
        })
        .collect()
}

#[tokio::test]
async fn a_created_association_analysis_rule_is_reported_by_a_later_read() {
    for (row, case) in round_trip_cases().into_iter().enumerate() {
        let label = &case.label;
        let emulator = Emulator::new();
        let stored = association(&emulator, &format!("assocrule{row}")).await;

        let (status, before) = emulator
            .read_association(&stored.membership_id, &stored.id)
            .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected GetConfiguredTableAssociation 200, got {status} with {before}"
        );
        let association_created_at = before["configuredTableAssociation"]["createTime"].clone();
        assert_eq!(
            before["configuredTableAssociation"]["analysisRuleTypes"],
            json!([]),
            "case {label:?}: a fresh association holds no analysis rule, got {}",
            before["configuredTableAssociation"]["analysisRuleTypes"]
        );

        let analyses: Option<Vec<String>> = case.additional.map(|(count, wildcard)| {
            (0..count)
                .map(|index| {
                    let last = index + 1 == count;
                    additional_analysis_arn(
                        &stored.membership_id,
                        if last && wildcard {
                            "*".to_string()
                        } else {
                            format!("model-association-{index}")
                        }
                        .as_str(),
                    )
                })
                .collect()
        });
        let policy = association_policy(
            case.rule_type,
            case.receivers.as_deref(),
            analyses.as_deref(),
        );

        let (status, created) = emulator
            .create_association_analysis_rule(
                &stored.membership_id,
                &stored.id,
                case.rule_type,
                policy.clone(),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected CreateConfiguredTableAssociationAnalysisRule 200, got \
             {status} with {created}"
        );
        let rule = &created["analysisRule"];

        assert_eq!(
            rule["membershipIdentifier"].as_str(),
            Some(stored.membership_id.as_str()),
            "case {label:?}: expected membershipIdentifier {:?}, got {}",
            stored.membership_id,
            rule["membershipIdentifier"]
        );
        assert_eq!(
            rule["configuredTableAssociationId"].as_str(),
            Some(stored.id.as_str()),
            "case {label:?}: expected configuredTableAssociationId {:?}, got {}",
            stored.id,
            rule["configuredTableAssociationId"]
        );
        assert!(
            is_model_uuid(rule["configuredTableAssociationId"].as_str().unwrap_or_default()),
            "case {label:?}: the id must keep the modelled UUID form, got {}",
            rule["configuredTableAssociationId"]
        );
        assert_eq!(
            rule["configuredTableAssociationArn"].as_str(),
            Some(stored.arn.as_str()),
            "case {label:?}: expected the association's own arn {:?}, got {}",
            stored.arn,
            rule["configuredTableAssociationArn"]
        );
        assert_eq!(
            rule["type"].as_str(),
            Some(case.rule_type),
            "case {label:?}: expected type {:?}, got {}",
            case.rule_type,
            rule["type"]
        );
        assert_eq!(
            rule["policy"], policy,
            "case {label:?}: expected the policy that was sent {policy}, got {}",
            rule["policy"]
        );
        assert!(
            rule["createTime"].is_number() && rule["createTime"] == rule["updateTime"],
            "case {label:?}: a created rule carries the creation instant for both \
             timestamps, got createTime {} and updateTime {}",
            rule["createTime"],
            rule["updateTime"]
        );

        // The read reports exactly the persisted record.
        let (status, read) = emulator
            .get_association_analysis_rule(&stored.membership_id, &stored.id, case.rule_type)
            .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected GetConfiguredTableAssociationAnalysisRule 200, got \
             {status} with {read}"
        );
        assert_eq!(
            read["analysisRule"], *rule,
            "case {label:?}: the read must report the created rule {rule}, got {}",
            read["analysisRule"]
        );

        // The association now reports the rule's type, and nothing else about it moved
        // except the timestamp that records the change.
        let (status, after) = emulator
            .read_association(&stored.membership_id, &stored.id)
            .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected GetConfiguredTableAssociation 200, got {status} with {after}"
        );
        assert_eq!(
            after["configuredTableAssociation"]["analysisRuleTypes"],
            json!([case.rule_type]),
            "case {label:?}: expected analysisRuleTypes [{:?}], got {}",
            case.rule_type,
            after["configuredTableAssociation"]["analysisRuleTypes"]
        );
        assert_eq!(
            after["configuredTableAssociation"]["createTime"], association_created_at,
            "case {label:?}: the association's createTime must be untouched, got {}",
            after["configuredTableAssociation"]["createTime"]
        );
        assert_eq!(
            after["configuredTableAssociation"]["updateTime"], rule["updateTime"],
            "case {label:?}: the association's updateTime must move to the rule's instant \
             {}, got {}",
            rule["updateTime"],
            after["configuredTableAssociation"]["updateTime"]
        );
    }
}

#[tokio::test]
async fn an_association_analysis_rule_policy_must_carry_the_policy_of_the_declared_type() {
    let mut row = 0;
    for declared in ALL_RULE_TYPES {
        for policy_type in ALL_RULE_TYPES {
            let label = format!("analysisRuleType={declared}, policy={policy_type}");
            let emulator = Emulator::new();
            let stored = association(&emulator, &format!("mismatch{row}")).await;
            row += 1;

            let (status, response) = emulator
                .create_association_analysis_rule(
                    &stored.membership_id,
                    &stored.id,
                    declared,
                    association_policy(policy_type, Some(&[OTHER_RESULT_RECEIVER]), None),
                )
                .await;

            if declared == policy_type {
                assert_eq!(
                    status, 200,
                    "case {label:?}: a policy of the declared type must be accepted, got \
                     {status} with {response}"
                );
                continue;
            }

            assert_eq!(
                status, 400,
                "case {label:?}: a policy of another type must be refused with 400, got \
                 {status} with {response}"
            );
            assert_eq!(
                error_code(&response),
                "ValidationException",
                "case {label:?}: expected ValidationException, got {response}"
            );
            assert_eq!(
                response["fieldList"][0]["name"].as_str(),
                Some("analysisRulePolicy"),
                "case {label:?}: expected the fieldList to name analysisRulePolicy, got \
                 {response}"
            );
            assert!(
                response.get("analysisRule").is_none(),
                "case {label:?}: a refused create must report no analysis rule, got {response}"
            );

            // The refusal persisted nothing: neither the declared type nor the policy's
            // own type is readable, and the association still holds no rule.
            for rule_type in [declared, policy_type] {
                let (status, read) = emulator
                    .get_association_analysis_rule(&stored.membership_id, &stored.id, rule_type)
                    .await;
                assert_eq!(
                    status, 404,
                    "case {label:?}: a refused create persists nothing, so {rule_type} must \
                     be 404, got {status} with {read}"
                );
            }
            let (status, after) = emulator
                .read_association(&stored.membership_id, &stored.id)
                .await;
            assert_eq!(
                status, 200,
                "case {label:?}: expected GetConfiguredTableAssociation 200, got {status} \
                 with {after}"
            );
            assert_eq!(
                after["configuredTableAssociation"]["analysisRuleTypes"],
                json!([]),
                "case {label:?}: a refused create leaves analysisRuleTypes empty, got {}",
                after["configuredTableAssociation"]["analysisRuleTypes"]
            );
        }
    }
}

#[tokio::test]
async fn a_second_create_under_a_held_type_is_a_conflict() {
    for (row, rule_type) in ALL_RULE_TYPES.into_iter().enumerate() {
        let label = format!("analysisRuleType={rule_type}");
        let emulator = Emulator::new();
        let stored = association(&emulator, &format!("conflict{row}")).await;

        let (status, created) = emulator
            .create_association_analysis_rule(
                &stored.membership_id,
                &stored.id,
                rule_type,
                association_policy(rule_type, Some(&[OTHER_RESULT_RECEIVER]), None),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected the first create to be accepted, got {status} with \
             {created}"
        );
        let first = created["analysisRule"].clone();

        let (status, response) = emulator
            .create_association_analysis_rule(
                &stored.membership_id,
                &stored.id,
                rule_type,
                association_policy(rule_type, Some(&[account_id()]), None),
            )
            .await;
        assert_eq!(
            status, 409,
            "case {label:?}: a second create under a held type must be refused with 409, got \
             {status} with {response}"
        );
        assert_eq!(
            error_code(&response),
            "ConflictException",
            "case {label:?}: expected ConflictException, got {response}"
        );
        assert_eq!(
            response["reason"].as_str(),
            Some("ALREADY_EXISTS"),
            "case {label:?}: expected reason ALREADY_EXISTS, got {response}"
        );
        assert_eq!(
            response["resourceId"].as_str(),
            Some(stored.id.as_str()),
            "case {label:?}: expected the refusal to name the association {:?}, got {response}",
            stored.id
        );

        // The refused create left the stored rule as the first one persisted it.
        let (status, read) = emulator
            .get_association_analysis_rule(&stored.membership_id, &stored.id, rule_type)
            .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected GetConfiguredTableAssociationAnalysisRule 200, got \
             {status} with {read}"
        );
        assert_eq!(
            read["analysisRule"], first,
            "case {label:?}: a refused create must leave the stored rule {first}, got {}",
            read["analysisRule"]
        );
    }
}

#[tokio::test]
async fn an_identifier_the_store_does_not_hold_is_not_found() {
    let emulator = Emulator::new();
    let mine = association(&emulator, "notfound-mine").await;
    let other = association(&emulator, "notfound-other").await;

    // Each row addresses a pair the store does not hold: an unknown membership, an
    // unknown association, and an association of ANOTHER membership.
    let cases: [(&str, &str, &str); 3] = [
        ("an unknown membership", ABSENT_UUID, mine.id.as_str()),
        (
            "an unknown association",
            mine.membership_id.as_str(),
            ABSENT_UUID,
        ),
        (
            "an association of another membership",
            mine.membership_id.as_str(),
            other.id.as_str(),
        ),
    ];

    for (label, membership_id, association_id) in cases {
        let (status, response) = emulator
            .create_association_analysis_rule(
                membership_id,
                association_id,
                "LIST",
                association_policy("LIST", Some(&[OTHER_RESULT_RECEIVER]), None),
            )
            .await;
        assert_eq!(
            status, 404,
            "case {label:?}: expected 404 from the create, got {status} with {response}"
        );
        assert_eq!(
            error_code(&response),
            "ResourceNotFoundException",
            "case {label:?}: expected ResourceNotFoundException from the create, got {response}"
        );

        let (status, read) = emulator
            .get_association_analysis_rule(membership_id, association_id, "LIST")
            .await;
        assert_eq!(
            status, 404,
            "case {label:?}: expected 404 from the read, got {status} with {read}"
        );
        assert_eq!(
            error_code(&read),
            "ResourceNotFoundException",
            "case {label:?}: expected ResourceNotFoundException from the read, got {read}"
        );
    }
}

#[tokio::test]
async fn a_pair_holding_no_rule_is_reported_as_the_association_that_would_own_it() {
    let emulator = Emulator::new();
    let stored = association(&emulator, "emptyrule").await;

    // One rule is stored, so the other two types of the catalog are pairs the store holds
    // no rule for: the read refuses each naming the pair that resolved to nothing.
    let (status, created) = emulator
        .create_association_analysis_rule(
            &stored.membership_id,
            &stored.id,
            "AGGREGATION",
            association_policy("AGGREGATION", None, None),
        )
        .await;
    assert_eq!(
        status, 200,
        "setup: expected CreateConfiguredTableAssociationAnalysisRule 200, got {status} with \
         {created}"
    );

    for rule_type in ["LIST", "CUSTOM"] {
        let label = format!("analysisRuleType={rule_type}");
        let (status, read) = emulator
            .get_association_analysis_rule(&stored.membership_id, &stored.id, rule_type)
            .await;
        assert_eq!(
            status, 404,
            "case {label:?}: expected 404 for a type the association holds no rule for, got \
             {status} with {read}"
        );
        assert_eq!(
            error_code(&read),
            "ResourceNotFoundException",
            "case {label:?}: expected ResourceNotFoundException, got {read}"
        );
        assert_eq!(
            error_message(&read),
            format!(
                "No such analysis rule with configuredTableAssociationId: {} and type {}",
                stored.id,
                internal_rule_type_name(rule_type)
            ),
            "case {label:?}: expected the refusal to name the pair, got {:?}",
            error_message(&read)
        );
        assert_eq!(
            read["resourceId"].as_str(),
            Some(stored.id.as_str()),
            "case {label:?}: expected the refusal to name the association {:?}, got {read}",
            stored.id
        );
        assert_eq!(
            read["resourceType"].as_str(),
            Some("CONFIGURED_TABLE_ASSOCIATION"),
            "case {label:?}: expected resourceType CONFIGURED_TABLE_ASSOCIATION, got {read}"
        );
    }
}

/// The `allowedAdditionalAnalyses` entries a policy shape asks for: `count` ARNs of this
/// membership, the last of them the wildcard the published pattern admits when `wildcard`.
fn additional_analyses(
    membership_id: &str,
    shape: Option<(usize, bool)>,
) -> Option<Vec<String>> {
    shape.map(|(count, wildcard)| {
        (0..count)
            .map(|index| {
                let last = index + 1 == count;
                additional_analysis_arn(
                    membership_id,
                    if last && wildcard {
                        "*".to_string()
                    } else {
                        format!("model-association-{index}")
                    }
                    .as_str(),
                )
            })
            .collect()
    })
}

/// One row of the update table: the policy the rule is created with and the policy the
/// update replaces it by, for one rule type.
struct UpdateCase {
    label: String,
    rule_type: &'static str,
    before: PolicyShape,
    after: PolicyShape,
}

/// Every rule type crossed with the transitions an update may make between the optional
/// members a policy carries: absent to present, present to absent, a longer list, a
/// wildcard entry, and a shorter list beside an emptied one.
fn update_cases() -> Vec<UpdateCase> {
    let transitions: [(PolicyShape, PolicyShape); 5] = [
        (
            PolicyShape {
                label: "no optional member",
                receivers: None,
                additional: None,
            },
            PolicyShape {
                label: "both members present",
                receivers: Some(vec![OTHER_RESULT_RECEIVER]),
                additional: Some((2, false)),
            },
        ),
        (
            PolicyShape {
                label: "both members present",
                receivers: Some(vec![OTHER_RESULT_RECEIVER]),
                additional: Some((1, false)),
            },
            PolicyShape {
                label: "no optional member",
                receivers: None,
                additional: None,
            },
        ),
        (
            PolicyShape {
                label: "one result receiver",
                receivers: Some(vec![OTHER_RESULT_RECEIVER]),
                additional: None,
            },
            PolicyShape {
                label: "two result receivers",
                receivers: Some(vec![OTHER_RESULT_RECEIVER, account_id()]),
                additional: None,
            },
        ),
        (
            PolicyShape {
                label: "one additional analysis",
                receivers: None,
                additional: Some((1, false)),
            },
            PolicyShape {
                label: "a wildcard additional analysis",
                receivers: None,
                additional: Some((1, true)),
            },
        ),
        (
            PolicyShape {
                label: "two additional analyses and a receiver",
                receivers: Some(vec![OTHER_RESULT_RECEIVER]),
                additional: Some((2, false)),
            },
            PolicyShape {
                label: "one additional analysis and an empty receiver list",
                receivers: Some(vec![]),
                additional: Some((1, false)),
            },
        ),
    ];
    ALL_RULE_TYPES
        .iter()
        .flat_map(|rule_type| {
            transitions.iter().map(move |(before, after)| UpdateCase {
                label: format!(
                    "analysisRuleType={rule_type}, {} -> {}",
                    before.label, after.label
                ),
                rule_type,
                before: PolicyShape {
                    label: before.label,
                    receivers: before.receivers.clone(),
                    additional: before.additional,
                },
                after: PolicyShape {
                    label: after.label,
                    receivers: after.receivers.clone(),
                    additional: after.additional,
                },
            })
        })
        .collect()
}

#[tokio::test]
async fn an_updated_association_analysis_rule_policy_is_reported_by_a_later_read() {
    for (row, case) in update_cases().into_iter().enumerate() {
        let label = &case.label;
        let emulator = Emulator::new();
        let stored = association(&emulator, &format!("assocupd{row}")).await;

        let before_policy = association_policy(
            case.rule_type,
            case.before.receivers.as_deref(),
            additional_analyses(&stored.membership_id, case.before.additional).as_deref(),
        );
        let (status, created) = emulator
            .create_association_analysis_rule(
                &stored.membership_id,
                &stored.id,
                case.rule_type,
                before_policy.clone(),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected CreateConfiguredTableAssociationAnalysisRule 200, got \
             {status} with {created}"
        );
        let created_rule = created["analysisRule"].clone();
        let (status, association_after_create) = emulator
            .read_association(&stored.membership_id, &stored.id)
            .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected GetConfiguredTableAssociation 200, got {status} with \
             {association_after_create}"
        );
        let association_before_update = association_after_create["configuredTableAssociation"].clone();

        let after_policy = association_policy(
            case.rule_type,
            case.after.receivers.as_deref(),
            additional_analyses(&stored.membership_id, case.after.additional).as_deref(),
        );
        let (status, updated) = emulator
            .update_association_analysis_rule(
                &stored.membership_id,
                &stored.id,
                case.rule_type,
                after_policy.clone(),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected UpdateConfiguredTableAssociationAnalysisRule 200, got \
             {status} with {updated}"
        );
        let rule = &updated["analysisRule"];

        assert_eq!(
            rule["policy"], after_policy,
            "case {label:?}: expected the policy that was sent {after_policy}, got {}",
            rule["policy"]
        );
        assert_eq!(
            rule["type"].as_str(),
            Some(case.rule_type),
            "case {label:?}: expected type {:?}, got {}",
            case.rule_type,
            rule["type"]
        );
        for member in [
            "membershipIdentifier",
            "configuredTableAssociationId",
            "configuredTableAssociationArn",
            "createTime",
        ] {
            assert_eq!(
                rule[member], created_rule[member],
                "case {label:?}: the update must leave {member} as the create persisted it \
                 {}, got {}",
                created_rule[member], rule[member]
            );
        }
        let created_at = created_rule["createTime"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {label:?}: no numeric createTime in {created_rule}"));
        let updated_at = rule["updateTime"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {label:?}: no numeric updateTime in {rule}"));
        assert!(
            updated_at >= created_at,
            "case {label:?}: the update's updateTime must be at or after the creation \
             instant {created_at}, got {updated_at}"
        );

        // The read reports exactly the persisted record, so the old policy is gone.
        let (status, read) = emulator
            .get_association_analysis_rule(&stored.membership_id, &stored.id, case.rule_type)
            .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected GetConfiguredTableAssociationAnalysisRule 200, got \
             {status} with {read}"
        );
        assert_eq!(
            read["analysisRule"], *rule,
            "case {label:?}: the read must report the updated rule {rule}, got {}",
            read["analysisRule"]
        );
        if after_policy != before_policy {
            assert_ne!(
                read["analysisRule"]["policy"], before_policy,
                "case {label:?}: the read must not report the replaced policy, got {}",
                read["analysisRule"]["policy"]
            );
        }

        // The set of types the association holds a rule for has not changed, so nothing
        // about the association itself moved.
        let (status, after) = emulator
            .read_association(&stored.membership_id, &stored.id)
            .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected GetConfiguredTableAssociation 200, got {status} with {after}"
        );
        assert_eq!(
            after["configuredTableAssociation"], association_before_update,
            "case {label:?}: a policy update leaves the association as it was {association_before_update}, got {}",
            after["configuredTableAssociation"]
        );
    }
}

#[tokio::test]
async fn an_updated_policy_must_carry_the_policy_of_the_addressed_type() {
    let mut row = 0;
    for addressed in ALL_RULE_TYPES {
        for policy_type in ALL_RULE_TYPES {
            let label = format!("analysisRuleType={addressed}, policy={policy_type}");
            let emulator = Emulator::new();
            let stored = association(&emulator, &format!("updmismatch{row}")).await;
            row += 1;

            let (status, created) = emulator
                .create_association_analysis_rule(
                    &stored.membership_id,
                    &stored.id,
                    addressed,
                    association_policy(addressed, Some(&[OTHER_RESULT_RECEIVER]), None),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {label:?}: expected the create to be accepted, got {status} with {created}"
            );
            let stored_rule = created["analysisRule"].clone();

            let (status, response) = emulator
                .update_association_analysis_rule(
                    &stored.membership_id,
                    &stored.id,
                    addressed,
                    association_policy(policy_type, Some(&[account_id()]), None),
                )
                .await;

            if addressed == policy_type {
                assert_eq!(
                    status, 200,
                    "case {label:?}: a policy of the addressed type must be accepted, got \
                     {status} with {response}"
                );
                continue;
            }

            assert_eq!(
                status, 400,
                "case {label:?}: a policy of another type must be refused with 400, got \
                 {status} with {response}"
            );
            assert_eq!(
                error_code(&response),
                "ValidationException",
                "case {label:?}: expected ValidationException, got {response}"
            );
            assert_eq!(
                response["fieldList"][0]["name"].as_str(),
                Some("analysisRulePolicy"),
                "case {label:?}: expected the fieldList to name analysisRulePolicy, got \
                 {response}"
            );
            assert!(
                response.get("analysisRule").is_none(),
                "case {label:?}: a refused update must report no analysis rule, got {response}"
            );

            // The refusal persisted nothing: the stored rule keeps the policy the create
            // gave it, and the policy's own type still holds no rule.
            let (status, read) = emulator
                .get_association_analysis_rule(&stored.membership_id, &stored.id, addressed)
                .await;
            assert_eq!(
                status, 200,
                "case {label:?}: expected GetConfiguredTableAssociationAnalysisRule 200, got \
                 {status} with {read}"
            );
            assert_eq!(
                read["analysisRule"], stored_rule,
                "case {label:?}: a refused update must leave the stored rule {stored_rule}, \
                 got {}",
                read["analysisRule"]
            );
            let (status, read) = emulator
                .get_association_analysis_rule(&stored.membership_id, &stored.id, policy_type)
                .await;
            assert_eq!(
                status, 404,
                "case {label:?}: a refused update mints no rule of the policy's type, so \
                 {policy_type} must be 404, got {status} with {read}"
            );
        }
    }
}

#[tokio::test]
async fn an_update_of_a_triple_holding_no_rule_is_not_found() {
    let emulator = Emulator::new();
    let mine = association(&emulator, "updnotfound-mine").await;
    let other = association(&emulator, "updnotfound-other").await;

    // `mine` holds one rule, so every row below addresses a triple that resolves to no
    // stored rule: an unknown membership, an unknown association, an association of
    // ANOTHER membership, and a type this association holds no rule for.
    let (status, created) = emulator
        .create_association_analysis_rule(
            &mine.membership_id,
            &mine.id,
            "AGGREGATION",
            association_policy("AGGREGATION", None, None),
        )
        .await;
    assert_eq!(
        status, 200,
        "setup: expected CreateConfiguredTableAssociationAnalysisRule 200, got {status} with \
         {created}"
    );

    let cases: [(&str, &str, &str, &str); 4] = [
        (
            "an unknown membership",
            ABSENT_UUID,
            mine.id.as_str(),
            "AGGREGATION",
        ),
        (
            "an unknown association",
            mine.membership_id.as_str(),
            ABSENT_UUID,
            "AGGREGATION",
        ),
        (
            "an association of another membership",
            mine.membership_id.as_str(),
            other.id.as_str(),
            "AGGREGATION",
        ),
        (
            "a type the association holds no rule for",
            mine.membership_id.as_str(),
            mine.id.as_str(),
            "LIST",
        ),
    ];

    for (label, membership_id, association_id, rule_type) in cases {
        let (status, response) = emulator
            .update_association_analysis_rule(
                membership_id,
                association_id,
                rule_type,
                association_policy(rule_type, Some(&[OTHER_RESULT_RECEIVER]), None),
            )
            .await;
        assert_eq!(
            status, 404,
            "case {label:?}: expected 404 from the update, got {status} with {response}"
        );
        assert_eq!(
            error_code(&response),
            "ResourceNotFoundException",
            "case {label:?}: expected ResourceNotFoundException from the update, got {response}"
        );

        // The refused update minted nothing the read can see.
        let (status, read) = emulator
            .get_association_analysis_rule(membership_id, association_id, rule_type)
            .await;
        assert_eq!(
            status, 404,
            "case {label:?}: a refused update persists nothing, so the read must be 404, got \
             {status} with {read}"
        );
    }

    // A triple whose membership and association resolve names the pair that holds no rule.
    let (status, response) = emulator
        .update_association_analysis_rule(
            &mine.membership_id,
            &mine.id,
            "CUSTOM",
            association_policy("CUSTOM", None, None),
        )
        .await;
    assert_eq!(
        status, 404,
        "expected 404 for a type the association holds no rule for, got {status} with {response}"
    );
    assert_eq!(
        error_message(&response),
        format!(
            "No such analysis rule with configuredTableAssociationId: {} and type {}",
            mine.id,
            internal_rule_type_name("CUSTOM")
        ),
        "expected the refusal to name the pair, got {:?}",
        error_message(&response)
    );
    assert_eq!(
        response["resourceType"].as_str(),
        Some("CONFIGURED_TABLE_ASSOCIATION"),
        "expected resourceType CONFIGURED_TABLE_ASSOCIATION, got {response}"
    );
}
