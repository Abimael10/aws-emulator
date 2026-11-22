// SPDX-License-Identifier: CC0-1.0

//! Which RESOURCE the IAM refusal of an operation on the custom-detection-rule
//! ASSOCIATION collection names, over the restJson1 wire.
//!
//! The rule under test: an operation whose URI binding carries no label for the
//! association it acts on is judged — and refused — against the association ARN of the
//! rule the request names in whichever binding carries it (the `ruleId` body member of
//! the create, the `ruleId` query parameter of the listing), with the any-association
//! wildcard standing in for the association id the request does not carry; only a request
//! that names no rule at all is judged against every resource of this service in the
//! account and Region addressed. The resource is what IAM both DECIDES on and names in
//! its sentence, so a grant written about one rule's associations serves calls about that
//! rule and refuses calls about another.
//!
//! The rows differ along what the rule generalises over: the BINDING the rule id arrives
//! in (a JSON body member, a query parameter), the FORM of the rule id (one segment,
//! several hyphenated segments, a long lower-case hex id, the id the catalogue actually
//! publishes), whether the request names a rule at all, and what the caller's policy
//! grants (nothing, this rule's associations, another rule's associations).
//!
//! The principals, their policies and the one shared harness are the
//! `identity_authorization` suite's, reused rather than rebuilt, so the decision here is
//! the real one IAM's own operations wrote.

use serde_json::json;
use service_com_amazonaws_guardduty_guarddutyapiservice::service::authorization::{
    ACTION_PREFIX, ANY_RESOURCE,
};

use crate::identity_authorization::{
    allow, default_deny_message, Emulator, Reply, ACCESS_DENIED, ACCESS_DENIED_STATUS, ACCOUNT_KEY,
    REGION,
};
use crate::resource_tag_removal::arn_of;

/// The operations of the association collection this suite drives, as `Operation::NAME`
/// spells each one: the create, whose `ruleId` arrives in its JSON body, and the listing,
/// whose `ruleId` arrives as a query parameter
/// (`smithy_query(operation="CreateCustomDetectionRuleAssociation,ListCustomDetectionRuleAssociations")`).
const CREATE: &str = "CreateCustomDetectionRuleAssociation";
const LIST: &str = "ListCustomDetectionRuleAssociations";

/// The URI every operation of the association collection is bound to
/// (`smithy_query(operation="CreateCustomDetectionRuleAssociation")`).
const ASSOCIATION_COLLECTION_PATH: &str = "/custom-detection-rule/association";

/// The resource path of one association of a custom detection rule, as this service's own
/// reads report it for a concrete association — `detection-rule/custom/<RuleId>` (the
/// resource path `DetectionRuleArn` carries, `smithy_query(shape="DetectionRuleArn")`)
/// followed by `association/<AssociationId>`. A request that carries no association id is
/// judged against every association of the rule, which is this path with the wildcard in
/// the id's place; the assertion below holds the two spellings to one another.
fn associations_of(rule_id: &str, association_id: &str) -> String {
    arn_of(
        REGION,
        &format!("detection-rule/custom/{rule_id}/association/{association_id}"),
    )
}

/// One request about the associations of a rule: the call itself and the action IAM judges
/// it by.
struct Request {
    case: &'static str,
    operation: &'static str,
    method: &'static str,
    path: String,
    body: Option<serde_json::Value>,
}

/// The create and the listing, each naming `rule_id` in the binding its own model gives
/// the member.
fn requests_naming(rule_id: &str) -> Vec<Request> {
    vec![
        Request {
            case: "a create, which names the rule in its body",
            operation: CREATE,
            method: "POST",
            path: ASSOCIATION_COLLECTION_PATH.to_string(),
            body: Some(json!({"ruleId": rule_id, "mode": "DRY_RUN"})),
        },
        Request {
            case: "a listing, which names the rule in its query string",
            operation: LIST,
            method: "GET",
            path: format!("{ASSOCIATION_COLLECTION_PATH}?ruleId={rule_id}"),
            body: None,
        },
    ]
}

/// Assert `reply` is the modelled refusal naming `expected` as its message.
fn assert_refused(case: &str, reply: &Reply, expected: &str) {
    assert_eq!(
        (reply.status, reply.error_code.as_str()),
        (ACCESS_DENIED_STATUS, ACCESS_DENIED),
        "case {case:?}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} {:?} with {:?}",
        reply.status,
        reply.error_code,
        reply.body
    );
    assert_eq!(
        reply.message(),
        expected,
        "case {case:?}: expected the refusal to name the rule's associations, got {:?}",
        reply.message()
    );
}

/// The id of a rule the catalogue publishes, read through this service's own listing.
async fn published_rule_id(emulator: &Emulator) -> String {
    let rules = emulator
        .call(
            ACCOUNT_KEY,
            "POST",
            "/custom-detection-rule/rule",
            Some(&json!({})),
        )
        .await;
    assert_eq!(
        rules.status, 200,
        "case \"fixture\": expected ListCustomDetectionRules 200, got {rules:?}"
    );
    rules.body["rules"]
        .as_array()
        .and_then(|rules| rules.first())
        .and_then(|rule| rule["ruleId"].as_str())
        .unwrap_or_else(|| panic!("the catalogue publishes at least one rule, got {rules:?}"))
        .to_string()
}

/// RULE: the refusal of an operation on the association collection names the ARN of the
/// associations of the rule the REQUEST names, whichever binding carries the id, with the
/// any-association wildcard where the request names no association id.
#[tokio::test]
async fn the_refusal_names_the_associations_of_the_rule_the_request_names() {
    let emulator = Emulator::new();
    let principal = emulator.user("association_zero_policy", None).await;
    let published = published_rule_id(&emulator).await;

    // Rule ids of the forms `RuleId` admits (`@pattern ^[a-z0-9]+(-[a-z0-9]+)*$`,
    // `@length` 1..100 — `smithy_query(operation="CreateCustomDetectionRuleAssociation")`):
    // one segment, several hyphenated segments, a long lower-case hex id, a single
    // character, and the id the catalogue publishes — which the suite did not choose.
    let rule_ids = [
        "suspiciouslogin".to_string(),
        "credential-exfiltration-probe".to_string(),
        "0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_string(),
        "a".to_string(),
        published.clone(),
    ];

    for rule_id in &rule_ids {
        for request in requests_naming(rule_id) {
            let reply = emulator
                .call(
                    &principal.access_key_id,
                    request.method,
                    &request.path,
                    request.body.as_ref(),
                )
                .await;
            let case = format!("{rule_id:?}: {}", request.case);
            let expected = default_deny_message(
                &principal.arn,
                request.operation,
                &associations_of(rule_id, ANY_RESOURCE),
            );
            assert_refused(&case, &reply, &expected);
        }
    }
}

/// RULE: the id is read from the binding the OPERATION's model gives `RuleId`, so a value
/// in a binding that operation does not use cannot decide the resource: the create takes
/// its rule from its body even when a query parameter of the same name travels with it.
#[tokio::test]
async fn the_binding_the_operation_models_decides_which_rule_is_read() {
    let emulator = Emulator::new();
    let principal = emulator.user("association_binding", None).await;
    let in_the_body = "body-named-rule";
    let in_the_query = "query-named-rule";

    let reply = emulator
        .call(
            &principal.access_key_id,
            "POST",
            &format!("{ASSOCIATION_COLLECTION_PATH}?ruleId={in_the_query}"),
            Some(&json!({"ruleId": in_the_body, "mode": "DRY_RUN"})),
        )
        .await;
    let expected = default_deny_message(
        &principal.arn,
        CREATE,
        &associations_of(in_the_body, ANY_RESOURCE),
    );
    assert_refused(
        "a create carrying a rule id in both bindings",
        &reply,
        &expected,
    );
}

/// RULE: the wildcard stands only for the id the request does not carry — the path it
/// stands in is the resource path a read of a concrete association reports, and a request
/// that names the association in its URI is judged against that association alone.
#[tokio::test]
async fn the_judged_path_is_the_one_a_read_of_the_association_reports() {
    let emulator = Emulator::new();
    let rule_id = published_rule_id(&emulator).await;
    let created = emulator
        .call(
            ACCOUNT_KEY,
            "POST",
            ASSOCIATION_COLLECTION_PATH,
            Some(&json!({"ruleId": rule_id, "mode": "LIVE"})),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case \"the association is made\": expected 200, got {created:?}"
    );
    let association = &created.body["ruleAssociation"];
    let association_id = association["associationId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        association["arn"].as_str().unwrap_or_default(),
        associations_of(&rule_id, &association_id),
        "case \"the association's own ARN\": expected the association resource path, got {association:?}"
    );

    // A refusal of the call that names the association in its URI names that association,
    // not the wildcard: the wildcard is only what the collection's operations, which carry
    // no association id, are judged against.
    let principal = emulator.user("association_path_zero", None).await;
    let reply = emulator
        .call(
            &principal.access_key_id,
            "GET",
            &format!("/custom-detection-rule/rule/{rule_id}/association/{association_id}"),
            None,
        )
        .await;
    let expected = default_deny_message(
        &principal.arn,
        "GetCustomDetectionRuleAssociation",
        &associations_of(&rule_id, &association_id),
    );
    assert_refused("a read of one association", &reply, &expected);
}

/// RULE: a request that names no rule at all addresses every association of the account in
/// the Region it arrived at, so it is judged against the any-resource wildcard.
#[tokio::test]
async fn a_listing_naming_no_rule_is_judged_against_every_resource() {
    let emulator = Emulator::new();
    let principal = emulator.user("association_unfiltered", None).await;

    // Rows: no query string at all, and a query string carrying only the OTHER filter the
    // listing models (`mode` — `smithy_query(operation="ListCustomDetectionRuleAssociations")`).
    for (case, path) in [
        ("no query string", ASSOCIATION_COLLECTION_PATH.to_string()),
        (
            "a query string naming only the mode",
            format!("{ASSOCIATION_COLLECTION_PATH}?mode=LIVE"),
        ),
    ] {
        let reply = emulator
            .call(&principal.access_key_id, "GET", &path, None)
            .await;
        let expected = default_deny_message(&principal.arn, LIST, &arn_of(REGION, ANY_RESOURCE));
        assert_refused(case, &reply, &expected);
    }
}

/// RULE: the resource is what IAM DECIDES on, not only what the sentence quotes — a grant
/// of the action on one rule's associations serves the call about that rule and leaves the
/// same call about another rule refused, naming the resource it was judged against.
#[tokio::test]
async fn a_grant_on_one_rules_associations_serves_that_rule_alone() {
    let emulator = Emulator::new();
    let rule_id = published_rule_id(&emulator).await;
    let other_rule = "credential-exfiltration-probe";
    let granted = emulator
        .user(
            "association_rule_grant",
            Some(&allow(
                &format!("{ACTION_PREFIX}:{CREATE}"),
                &associations_of(&rule_id, ANY_RESOURCE),
            )),
        )
        .await;

    // The granted rule: the create is served, and the association it wrote is readable
    // back through this service's own read.
    let created = emulator
        .call(
            &granted.access_key_id,
            "POST",
            ASSOCIATION_COLLECTION_PATH,
            Some(&json!({"ruleId": rule_id, "mode": "DRY_RUN"})),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case \"the granted rule\": expected the create's own 200, got {created:?}"
    );
    let association_id = created.body["ruleAssociation"]["associationId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let read = emulator
        .call(
            ACCOUNT_KEY,
            "GET",
            &format!("/custom-detection-rule/rule/{rule_id}/association/{association_id}"),
            None,
        )
        .await;
    assert_eq!(
        (
            read.status,
            read.body["ruleAssociation"]["associationId"]
                .as_str()
                .unwrap_or_default()
        ),
        (200, association_id.as_str()),
        "case \"the granted rule\": expected the association to read back, got {read:?}"
    );

    // Another rule: the same grant covers nothing about it, so the create falls to IAM's
    // default deny naming THAT rule's associations.
    let refused = emulator
        .call(
            &granted.access_key_id,
            "POST",
            ASSOCIATION_COLLECTION_PATH,
            Some(&json!({"ruleId": other_rule, "mode": "DRY_RUN"})),
        )
        .await;
    let expected = default_deny_message(
        &granted.arn,
        CREATE,
        &associations_of(other_rule, ANY_RESOURCE),
    );
    assert_refused("another rule's associations", &refused, &expected);

    // The refused create wrote nothing: the account holds the one association the granted
    // call made and no other.
    let listed = emulator
        .call(ACCOUNT_KEY, "GET", ASSOCIATION_COLLECTION_PATH, None)
        .await;
    let ids: Vec<&str> = listed.body["ruleAssociations"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["associationId"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        ids,
        vec![association_id.as_str()],
        "case \"a refused create writes nothing\": expected only the granted association, \
         got {listed:?}"
    );
}
