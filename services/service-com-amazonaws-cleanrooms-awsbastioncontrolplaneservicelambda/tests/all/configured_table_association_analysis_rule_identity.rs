// SPDX-License-Identifier: CC0-1.0

//! How a configured table association is IDENTIFIED in the analysis-rule plane, and the
//! one ARN layout this service renders one in.
//!
//! The rules under test:
//!
//! * a configured-table-association analysis rule reports the association by the
//!   identifiers the request ADDRESSED it with: `membershipIdentifier` and
//!   `configuredTableAssociationId` are the two identifier `@httpLabel`s of the path, and
//!   `configuredTableAssociationArn` is the membership-qualified ARN of that pair. The
//!   create persists them, so every later read of the rule reports the association the way
//!   the create addressed it;
//! * every configured-table-association ARN this service renders nests the association in
//!   its membership —
//!   `arn:aws:cleanrooms:<region>:<account>:membership/<membershipId>/configuredtableassociation/<configuredTableAssociationId>`
//!   — the layout the model spells out for this very resource in the
//!   `AccessBudgetResourceArn` pattern of `ListPrivacyBudgets$accessBudgetResourceArn`
//!   (`^arn:aws:[\w]+:[\w]{2}-[\w]{4,9}-[\d]:[\d]{12}:membership/[\d\w-]+/(configuredtableassociation|intermediatetable)/[\d\w-]+$`)
//!   and repeats in `SchemaResourceArn`;
//! * the `(membership, configured table association, analysis rule type)` triple is what
//!   an analysis-rule operation addresses, so the delete of a triple drops exactly that
//!   rule — the type leaves the association's `analysisRuleTypes` with it — and a triple
//!   that holds no rule is the modelled 404 `ResourceNotFoundException`, named by the
//!   identifier the request carried and the internal name of the rule class.
//!
//! The tables vary what the rules generalise over: every
//! `ConfiguredTableAssociationAnalysisRuleType` the model declares, both forms a
//! `configuredTableAssociationIdentifier` takes, how many associations the membership
//! holds, how many rule types one association holds, which of them is addressed, and the
//! operation reporting the ARN (the association create, its read, the list, and the
//! analysis-rule create and read). Every request goes over the wire through the same
//! `MultiServiceRouter` the production router uses.

use serde_json::{json, Value};

use super::cleanrooms_collaboration_lifecycle::{
    error_code, error_message, is_model_uuid, Emulator,
};
use super::configured_table_association_analysis_rules::{
    association_policy, internal_rule_type_name, ALL_RULE_TYPES,
};
use super::configured_table_association_identifier_forms::{
    association_arn, fixture, identifier_forms,
};
use super::empty_output_response_statuses::assert_no_content;

/// An account id that is not the caller's, twelve digits as `AccountId` declares.
const OTHER_RESULT_RECEIVER: &str = "210987654321";

/// An identifier in the form the model's `MembershipIdentifier` /
/// `ConfiguredTableAssociationIdentifier` pattern declares that no create here mints.
const ABSENT_UUID: &str = "00000000-0000-0000-0000-000000000000";

impl Emulator {
    /// `DeleteConfiguredTableAssociationAnalysisRule`, with the `x-amzn-errortype` header
    /// restJson1 names a refusal's shape in.
    async fn delete_association_analysis_rule(
        &self,
        membership_id: &str,
        association_identifier: &str,
        rule_type: &str,
    ) -> (u16, String, Value) {
        self.call_with_error_type(
            "DELETE",
            &format!(
                "/memberships/{membership_id}/configuredTableAssociations/{association_identifier}/analysisRule/{rule_type}"
            ),
            None,
        )
        .await
    }

    /// `GetConfiguredTableAssociationAnalysisRule`, with the `x-amzn-errortype` header
    /// restJson1 names a refusal's shape in.
    async fn get_association_analysis_rule_with_error_type(
        &self,
        membership_id: &str,
        association_identifier: &str,
        rule_type: &str,
    ) -> (u16, String, Value) {
        self.call_with_error_type(
            "GET",
            &format!(
                "/memberships/{membership_id}/configuredTableAssociations/{association_identifier}/analysisRule/{rule_type}"
            ),
            None,
        )
        .await
    }

    /// `ListConfiguredTableAssociations` of one membership.
    async fn list_associations(&self, membership_id: &str) -> (u16, Value) {
        self.call(
            "GET",
            &format!("/memberships/{membership_id}/configuredTableAssociations"),
            None,
        )
        .await
    }
}

/// The message the association plane answers when the triple a request addresses holds no
/// analysis rule: the identifier the REQUEST carried, and the rule type by the internal
/// name of its class.
fn no_such_rule_message(association_identifier: &str, rule_type: &str) -> String {
    format!(
        "No such analysis rule with configuredTableAssociationId: {association_identifier} and type {}",
        internal_rule_type_name(rule_type)
    )
}

/// Asserts `body` is the 404 a triple holding no rule answers with.
fn assert_no_such_rule(
    case: &str,
    operation: &str,
    status: u16,
    error_type: &str,
    body: &Value,
    association_identifier: &str,
    rule_type: &str,
) {
    assert_eq!(
        status, 404,
        "case {case:?}: expected {operation} 404, got {status} with {body}"
    );
    assert!(
        error_type.ends_with("ResourceNotFoundException"),
        "case {case:?}: expected {operation} to name ResourceNotFoundException in \
         x-amzn-errortype, got {error_type:?}"
    );
    assert_eq!(
        error_code(body),
        "ResourceNotFoundException",
        "case {case:?}: expected {operation} to answer ResourceNotFoundException, got {body}"
    );
    let expected = no_such_rule_message(association_identifier, rule_type);
    assert_eq!(
        error_message(body),
        expected,
        "case {case:?}: expected {operation} message {expected:?}, got {:?}",
        error_message(body)
    );
    assert_eq!(
        body["resourceId"].as_str(),
        Some(association_identifier),
        "case {case:?}: expected {operation} to name the association {association_identifier:?}, \
         got {body}"
    );
    assert_eq!(
        body["resourceType"].as_str(),
        Some("CONFIGURED_TABLE_ASSOCIATION"),
        "case {case:?}: expected {operation} resourceType CONFIGURED_TABLE_ASSOCIATION, got {body}"
    );
}

#[tokio::test]
async fn an_analysis_rule_reports_the_identifiers_the_request_addressed() {
    // Every rule type crossed with both identifier forms, with the SECOND of two
    // associations addressed, so neither form can resolve by falling back to the only
    // association stored.
    for rule_type in ALL_RULE_TYPES {
        for form in 0..2 {
            let emulator = Emulator::new();
            let held = fixture(&emulator, &format!("ruleid-{rule_type}-{form}"), 2).await;
            let association = &held.associations[1];
            let (addressed_by, identifier) = identifier_forms(association)[form];
            let case = format!("analysisRuleType={rule_type}, addressed by {addressed_by}");
            let policy = association_policy(rule_type, Some(&[OTHER_RESULT_RECEIVER]), None);

            let (status, created) = emulator
                .create_association_analysis_rule(
                    &held.membership_id,
                    identifier,
                    rule_type,
                    policy.clone(),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected CreateConfiguredTableAssociationAnalysisRule 200, \
                 got {status} with {created}"
            );
            let rule = &created["analysisRule"];

            assert_eq!(
                rule["membershipIdentifier"].as_str(),
                Some(held.membership_id.as_str()),
                "case {case:?}: expected the membership the path carried {}, got {}",
                held.membership_id,
                rule["membershipIdentifier"]
            );
            assert_eq!(
                rule["configuredTableAssociationId"].as_str(),
                Some(identifier),
                "case {case:?}: expected the identifier the path carried {identifier:?}, got {}",
                rule["configuredTableAssociationId"]
            );
            assert!(
                is_model_uuid(rule["configuredTableAssociationId"].as_str().unwrap_or_default()),
                "case {case:?}: the reported id must keep the modelled UUID form, got {}",
                rule["configuredTableAssociationId"]
            );
            let expected_arn = association_arn(&held.membership_id, identifier);
            assert_eq!(
                rule["configuredTableAssociationArn"].as_str(),
                Some(expected_arn.as_str()),
                "case {case:?}: expected the membership-qualified ARN of the pair \
                 {expected_arn:?}, got {}",
                rule["configuredTableAssociationArn"]
            );
            assert_eq!(
                rule["type"].as_str(),
                Some(rule_type),
                "case {case:?}: expected the modelled catalog value {rule_type:?}, got {}",
                rule["type"]
            );
            assert_eq!(
                rule["policy"], policy,
                "case {case:?}: expected the policy that was sent {policy}, got {}",
                rule["policy"]
            );

            // The identifiers are PERSISTED with the rule, so a read reports the
            // association the way the create addressed it — whichever form the read uses.
            for (read_by, read_identifier) in identifier_forms(association) {
                let (status, read) = emulator
                    .get_association_analysis_rule(&held.membership_id, read_identifier, rule_type)
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected GetConfiguredTableAssociationAnalysisRule 200 \
                     read by {read_by}, got {status} with {read}"
                );
                assert_eq!(
                    read["analysisRule"], *rule,
                    "case {case:?}: the read by {read_by} must report the persisted rule \
                     {rule}, got {}",
                    read["analysisRule"]
                );
            }

            // The rule landed on the association the identifier named, and on no other.
            let (status, addressed) = emulator
                .read_association(&held.membership_id, &association.id)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected GetConfiguredTableAssociation 200, got {status} \
                 with {addressed}"
            );
            assert_eq!(
                addressed["configuredTableAssociation"]["analysisRuleTypes"],
                json!([rule_type]),
                "case {case:?}: expected the addressed association to hold [{rule_type}], got {}",
                addressed["configuredTableAssociation"]["analysisRuleTypes"]
            );
        }
    }
}

#[tokio::test]
async fn every_configured_table_association_arn_nests_it_in_its_membership() {
    let emulator = Emulator::new();
    // Rows differ in how many associations the membership holds and which of them is
    // addressed: one membership with three, and a second membership with one.
    let first = fixture(&emulator, "arnlayout-first", 3).await;
    let second = fixture(&emulator, "arnlayout-second", 1).await;

    for held in [&first, &second] {
        let (status, listed) = emulator.list_associations(&held.membership_id).await;
        assert_eq!(
            status, 200,
            "membership {}: expected ListConfiguredTableAssociations 200, got {status} with \
             {listed}",
            held.membership_id
        );

        for (row, association) in held.associations.iter().enumerate() {
            let case = format!(
                "membership {} association {row} ({})",
                held.membership_id, association.name
            );
            let expected = association_arn(&held.membership_id, &association.id);

            assert_eq!(
                association.arn, expected,
                "case {case:?}: expected CreateConfiguredTableAssociation to report \
                 {expected:?}, got {:?}",
                association.arn
            );

            let (status, read) = emulator
                .read_association(&held.membership_id, &association.id)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected GetConfiguredTableAssociation 200, got {status} with \
                 {read}"
            );
            assert_eq!(
                read["configuredTableAssociation"]["arn"].as_str(),
                Some(expected.as_str()),
                "case {case:?}: expected the read to report {expected:?}, got {}",
                read["configuredTableAssociation"]["arn"]
            );

            let summary = listed["configuredTableAssociationSummaries"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: no summaries in {listed}"))
                .iter()
                .find(|summary| summary["id"].as_str() == Some(association.id.as_str()))
                .unwrap_or_else(|| panic!("case {case:?}: {} not listed", association.id));
            assert_eq!(
                summary["arn"].as_str(),
                Some(expected.as_str()),
                "case {case:?}: expected the listed summary to report {expected:?}, got {summary}"
            );

            // The analysis-rule plane renders the ARN of the pair the request addressed,
            // which for the association's own id is the association's own ARN.
            let (status, created) = emulator
                .create_association_analysis_rule(
                    &held.membership_id,
                    &association.id,
                    "LIST",
                    association_policy("LIST", Some(&[OTHER_RESULT_RECEIVER]), None),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected CreateConfiguredTableAssociationAnalysisRule 200, \
                 got {status} with {created}"
            );
            assert_eq!(
                created["analysisRule"]["configuredTableAssociationArn"].as_str(),
                Some(expected.as_str()),
                "case {case:?}: expected the created rule to report {expected:?}, got {}",
                created["analysisRule"]["configuredTableAssociationArn"]
            );
            let (status, read) = emulator
                .get_association_analysis_rule(&held.membership_id, &association.id, "LIST")
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected GetConfiguredTableAssociationAnalysisRule 200, got \
                 {status} with {read}"
            );
            assert_eq!(
                read["analysisRule"]["configuredTableAssociationArn"].as_str(),
                Some(expected.as_str()),
                "case {case:?}: expected the read rule to report {expected:?}, got {}",
                read["analysisRule"]["configuredTableAssociationArn"]
            );
        }
    }
}

#[tokio::test]
async fn deleting_an_analysis_rule_returns_its_triple_to_not_found() {
    // Every rule type, addressed by each identifier form: the delete drops the rule of
    // that triple and nothing else, and the triple is not found afterwards.
    for rule_type in ALL_RULE_TYPES {
        for form in 0..2 {
            let emulator = Emulator::new();
            let held = fixture(&emulator, &format!("ruledel-{rule_type}-{form}"), 2).await;
            let association = &held.associations[1];
            let sibling = &held.associations[0];
            let (addressed_by, identifier) = identifier_forms(association)[form];
            let case = format!("analysisRuleType={rule_type}, addressed by {addressed_by}");

            // Nothing is stored yet, so the triple is already a 404 — on the read and on
            // the delete alike.
            let (status, error_type, body) = emulator
                .delete_association_analysis_rule(&held.membership_id, identifier, rule_type)
                .await;
            assert_no_such_rule(
                &case,
                "DeleteConfiguredTableAssociationAnalysisRule before the create",
                status,
                &error_type,
                &body,
                identifier,
                rule_type,
            );

            let (status, created) = emulator
                .create_association_analysis_rule(
                    &held.membership_id,
                    identifier,
                    rule_type,
                    association_policy(rule_type, Some(&[OTHER_RESULT_RECEIVER]), None),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create to be accepted, got {status} with {created}"
            );
            // A rule of another type on the SIBLING association, which the delete below
            // must leave alone.
            let other_type = ALL_RULE_TYPES
                .iter()
                .find(|other| **other != rule_type)
                .copied()
                .expect("another rule type");
            let (status, sibling_rule) = emulator
                .create_association_analysis_rule(
                    &held.membership_id,
                    &sibling.id,
                    other_type,
                    association_policy(other_type, None, None),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the sibling's create to be accepted, got {status} \
                 with {sibling_rule}"
            );

            let (status, error_type, body) = emulator
                .delete_association_analysis_rule(&held.membership_id, identifier, rule_type)
                .await;
            assert_no_content(&case, status, &body);
            assert_eq!(
                error_type, "",
                "case {case:?}: a no-content answer names no error type, got {error_type:?}"
            );

            // The rule is gone: the read of its triple is the 404 it answered before the
            // create, and the association stops reporting the type.
            let (status, error_type, body) = emulator
                .get_association_analysis_rule_with_error_type(
                    &held.membership_id,
                    identifier,
                    rule_type,
                )
                .await;
            assert_no_such_rule(
                &case,
                "GetConfiguredTableAssociationAnalysisRule after the delete",
                status,
                &error_type,
                &body,
                identifier,
                rule_type,
            );
            let (status, error_type, body) = emulator
                .delete_association_analysis_rule(&held.membership_id, identifier, rule_type)
                .await;
            assert_no_such_rule(
                &case,
                "DeleteConfiguredTableAssociationAnalysisRule twice",
                status,
                &error_type,
                &body,
                identifier,
                rule_type,
            );
            let (status, after) = emulator
                .read_association(&held.membership_id, &association.id)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected GetConfiguredTableAssociation 200, got {status} with \
                 {after}"
            );
            assert_eq!(
                after["configuredTableAssociation"]["analysisRuleTypes"],
                json!([]),
                "case {case:?}: the deleted type must leave analysisRuleTypes, got {}",
                after["configuredTableAssociation"]["analysisRuleTypes"]
            );

            // The sibling's rule of another type is untouched: the delete addressed one
            // triple.
            let (status, kept) = emulator
                .get_association_analysis_rule(&held.membership_id, &sibling.id, other_type)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the sibling's {other_type} rule must survive, got {status} \
                 with {kept}"
            );
        }
    }
}

#[tokio::test]
async fn an_analysis_rule_delete_outside_the_membership_is_not_found() {
    let emulator = Emulator::new();
    let mine = fixture(&emulator, "ruledel-mine", 1).await;
    let theirs = fixture(&emulator, "ruledel-theirs", 1).await;
    let (status, created) = emulator
        .create_association_analysis_rule(
            &mine.membership_id,
            &mine.associations[0].id,
            "LIST",
            association_policy("LIST", Some(&[OTHER_RESULT_RECEIVER]), None),
        )
        .await;
    assert_eq!(
        status, 200,
        "setup: expected the create to be accepted, got {status} with {created}"
    );

    // Each row addresses a triple this membership does not hold a rule under: an unknown
    // membership, an unknown association, another membership's association, and a type the
    // association holds no rule of.
    let cases: [(&str, &str, &str, &str); 4] = [
        (
            "an unknown membership",
            ABSENT_UUID,
            mine.associations[0].id.as_str(),
            "LIST",
        ),
        (
            "an unknown association",
            mine.membership_id.as_str(),
            ABSENT_UUID,
            "LIST",
        ),
        (
            "another membership's association",
            mine.membership_id.as_str(),
            theirs.associations[0].id.as_str(),
            "LIST",
        ),
        (
            "a type the association holds no rule of",
            mine.membership_id.as_str(),
            mine.associations[0].id.as_str(),
            "CUSTOM",
        ),
    ];

    for (case, membership_id, identifier, rule_type) in cases {
        let (status, error_type, body) = emulator
            .delete_association_analysis_rule(membership_id, identifier, rule_type)
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected DeleteConfiguredTableAssociationAnalysisRule 404, got \
             {status} with {body}"
        );
        assert!(
            error_type.ends_with("ResourceNotFoundException"),
            "case {case:?}: expected x-amzn-errortype to name ResourceNotFoundException, got \
             {error_type:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body}"
        );
    }

    // The rule the setup created is still there: a refused delete persists nothing.
    let (status, kept) = emulator
        .get_association_analysis_rule(&mine.membership_id, &mine.associations[0].id, "LIST")
        .await;
    assert_eq!(
        status, 200,
        "expected the stored rule to survive every refused delete, got {status} with {kept}"
    );
}

#[tokio::test]
async fn a_delete_drops_one_type_of_an_association_holding_several() {
    // One association holds a rule of EVERY type the model's catalog declares, so the
    // delete has siblings on the very resource it addresses to leave alone. The rows vary
    // which type the delete addresses and which form of the identifier addresses it.
    for deleted in ALL_RULE_TYPES {
        for form in 0..2 {
            let emulator = Emulator::new();
            let held = fixture(&emulator, &format!("ruleset-{deleted}-{form}"), 2).await;
            let association = &held.associations[1];
            let (addressed_by, identifier) = identifier_forms(association)[form];
            let case = format!("deleting {deleted}, addressed by {addressed_by}");

            // Every type, created in catalog order, each with its own policy shape so a
            // surviving rule cannot be mistaken for another.
            let mut stored: Vec<(&str, Value)> = Vec::new();
            for rule_type in ALL_RULE_TYPES {
                let (status, created) = emulator
                    .create_association_analysis_rule(
                        &held.membership_id,
                        identifier,
                        rule_type,
                        association_policy(rule_type, Some(&[OTHER_RESULT_RECEIVER]), None),
                    )
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the {rule_type} create to be accepted, got \
                     {status} with {created}"
                );
                stored.push((rule_type, created["analysisRule"].clone()));
            }
            let (status, before) = emulator
                .read_association(&held.membership_id, &association.id)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected GetConfiguredTableAssociation 200, got {status} with \
                 {before}"
            );
            assert_eq!(
                before["configuredTableAssociation"]["analysisRuleTypes"],
                json!(ALL_RULE_TYPES),
                "case {case:?}: expected the association to hold every created type, got {}",
                before["configuredTableAssociation"]["analysisRuleTypes"]
            );

            let (status, error_type, body) = emulator
                .delete_association_analysis_rule(&held.membership_id, identifier, deleted)
                .await;
            assert_no_content(&case, status, &body);
            assert_eq!(
                error_type, "",
                "case {case:?}: a no-content answer names no error type, got {error_type:?}"
            );

            // Exactly the addressed triple is gone: its read is the 404 of a triple holding
            // no rule, and every sibling type still reports the record its create persisted.
            let (status, error_type, body) = emulator
                .get_association_analysis_rule_with_error_type(
                    &held.membership_id,
                    identifier,
                    deleted,
                )
                .await;
            assert_no_such_rule(
                &case,
                "GetConfiguredTableAssociationAnalysisRule after the delete",
                status,
                &error_type,
                &body,
                identifier,
                deleted,
            );
            for (rule_type, rule) in stored.iter().filter(|(rule_type, _)| *rule_type != deleted) {
                let (status, read) = emulator
                    .get_association_analysis_rule(&held.membership_id, identifier, rule_type)
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the {rule_type} rule must survive, got {status} with {read}"
                );
                assert_eq!(
                    read["analysisRule"], *rule,
                    "case {case:?}: expected the {rule_type} rule to stay {rule}, got {}",
                    read["analysisRule"]
                );
            }

            // The deleted type — and only it — leaves `analysisRuleTypes`; the rest keep
            // the order they were created in.
            let remaining: Vec<&str> = ALL_RULE_TYPES
                .iter()
                .copied()
                .filter(|rule_type| *rule_type != deleted)
                .collect();
            let (status, after) = emulator
                .read_association(&held.membership_id, &association.id)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected GetConfiguredTableAssociation 200, got {status} with \
                 {after}"
            );
            assert_eq!(
                after["configuredTableAssociation"]["analysisRuleTypes"],
                json!(remaining),
                "case {case:?}: expected the surviving types {remaining:?}, got {}",
                after["configuredTableAssociation"]["analysisRuleTypes"]
            );

            // The delete freed the triple rather than marking it taken: a create of the
            // same type is accepted again — no 409 — and the read reports the NEW policy,
            // which the dropped record did not carry.
            let replacement = association_policy(deleted, None, None);
            let (status, recreated) = emulator
                .create_association_analysis_rule(
                    &held.membership_id,
                    identifier,
                    deleted,
                    replacement.clone(),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create after the delete to be accepted, got \
                 {status} with {recreated}"
            );
            let (status, read) = emulator
                .get_association_analysis_rule(&held.membership_id, identifier, deleted)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the recreated rule to be readable, got {status} with \
                 {read}"
            );
            assert_eq!(
                read["analysisRule"]["policy"], replacement,
                "case {case:?}: expected the recreated rule to carry {replacement}, got {}",
                read["analysisRule"]["policy"]
            );
        }
    }
}
