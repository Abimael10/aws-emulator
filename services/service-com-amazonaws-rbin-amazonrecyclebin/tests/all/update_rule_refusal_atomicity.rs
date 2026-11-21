// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the one rule every refused `UpdateRule` is held to: a
//! modification the service could never hold is refused BEFORE anything is written, so
//! the stored rule the request addressed is left exactly as it was — every member it
//! carried AND the status it had reached.
//!
//! The refusal class this suite leads with is the coupling between what a rule retains
//! and how long it may retain it. `RetentionPeriod$RetentionPeriodValue` publishes a
//! per-type period ("The supported retention periods are: EBS volumes: 1 - 7 days / EBS
//! snapshots and EBS-backed AMIs: 1 - 365 days"), while the member's own `@range` is the
//! looser union {min: 1, max: 3650}; `UpdateRuleRequest$ResourceType` is "currently not
//! supported. You can't update a retention rule's resource type after creation", so the
//! type the bound is read against is the STORED one.
//! Source: smithy_query(query="doc", shape="RetentionPeriod",
//! member="RetentionPeriodValue") and smithy_query(query="doc", shape="UpdateRuleRequest",
//! member="ResourceType").
//!
//! `tests/all/retention_period_resource_type_bounds.rs` owns which periods each type
//! admits, at both doors onto a rule's period. What this suite adds is what a refusal
//! must leave behind: the stored record is untouched, including the status, so a request
//! the service refuses never knocks an `available` rule back to `pending` and never
//! applies the members that travelled beside the refused one. A second refusal class —
//! a merged resource selection naming both tag sets — is driven through the same
//! assertions, so the rule under test is the general one about a refused `UpdateRule`
//! rather than one validation check's own doing. (The resource-tag-pair quota refusal is
//! held to the same invariant by `tests/all/resource_tag_pair_quota.rs`, and the
//! lock-protected refusal by `tests/all/update_rule_lock_invariant.rs`.)
//!
//! The same rule holds for the OTHER kind of `UpdateRule` that applies nothing: one the
//! service ACCEPTS but that changes no configuration, because every optional member is
//! absent or re-sends the value the rule already holds. `UpdateRuleRequest` declares
//! `Identifier` as its one required member, so both are requests the model accepts; only
//! a request that really changes the rule takes it out of the retaining state, `Status`
//! being "The state of the retention rule. Only retention rules that are in the available
//! state retain resources" (smithy_query(query="operation", operation="UpdateRule")). The
//! two tables at the end of this suite are that rule and its complement.
//!
//! Every case drives the service through the real router, exactly as a signed SDK request
//! would, and every row states its own case so a failure names the input.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::error_response_bodies::{error_code, message, one_validation_error};
use crate::retention_rule_lifecycle::{
    create_rule, delete_rule, get_rule, list_of, retention, router, supported_retention_max,
    update_rule, RESOURCE_TYPES, RETENTION_VALUE_MAX, RETENTION_VALUE_MIN,
};
use service_com_amazonaws_rbin_amazonrecyclebin::state::{
    RULE_STATUS_AVAILABLE, RULE_STATUS_PENDING, STATUS_TRANSITION,
};
use smithy_server_runtime::MultiServiceRouter;

/// The member path a refused `ExcludeResourceTags` is named by.
/// Source: smithy_query(query="operation", operation="UpdateRule") — input member
/// `ExcludeResourceTags`.
const EXCLUDE_RESOURCE_TAGS_MEMBER: &str = "excludeResourceTags";

/// The requirement an exclusion tag fails on a rule that selects by resource tags:
/// exclusion tags are "[Region-level retention rules only] ... You can't specify exclusion
/// tags for tag-level retention rules" (smithy_query(query="doc",
/// shape="UpdateRuleRequest", member="ExcludeResourceTags")).
const TAG_LEVEL_REQUIREMENT: &str = "Member must not be specified for a tag-level retention rule";

/// The fault class a refused parameter value is reported under.
/// Source: smithy_query(query="shape", shape="ValidationExceptionReason").
const INVALID_PARAMETER_VALUE: &str = "INVALID_PARAMETER_VALUE";

/// The longest period each resource type is retained for, in the WORDS the service states
/// that bound in when it refuses a longer one: the SPAN its documentation publishes, not a
/// count of days.
/// Source: smithy_query(query="doc", shape="RetentionPeriod", member="RetentionPeriodValue") —
/// "EBS volumes: 1 - 7 days / EBS snapshots and EBS-backed AMIs: 1 - 365 days", the latter
/// being one year.
const SUPPORTED_MAXIMUM_PHRASE: [(&str, &str); 3] = [
    ("EBS_VOLUME", "7 days"),
    ("EBS_SNAPSHOT", "1 year"),
    ("EC2_IMAGE", "1 year"),
];

/// The sentence the service refuses a period past `resource_type`'s own bound with: the span a
/// period has to fall in, which is what a caller is told instead of the value it sent.
fn past_the_bound_message(resource_type: &str) -> String {
    let phrase = SUPPORTED_MAXIMUM_PHRASE
        .iter()
        .find(|(named, _)| *named == resource_type)
        .map(|(_, phrase)| *phrase)
        .unwrap_or_else(|| panic!("resource type {resource_type:?} has no published maximum"));
    format!("Retention period value must be between 1 day and {phrase} inclusive")
}

/// Long enough that the `pending` -> `available` transition the clock owes a rule
/// modified before the wait started is past due.
fn past_the_transition() -> Duration {
    STATUS_TRANSITION + Duration::from_millis(250)
}

/// One `UpdateRule` body the service must refuse, and the message the refusal must carry.
struct Refusal {
    case: String,
    patch: Value,
    expected_message: String,
}

/// Every refused patch this suite sends to a stored rule of `resource_type`: a period one
/// day past the bound its type publishes, a period at the endpoint of the model's own
/// looser range, the same unsupported period travelling beside members that would
/// otherwise be applied, and a merged selection no rule can hold.
fn refusals_for(resource_type: &str) -> Vec<Refusal> {
    let max = supported_retention_max(resource_type);
    let past_the_bound = past_the_bound_message(resource_type);
    vec![
        Refusal {
            case: format!("{resource_type}: one day past the period its stored type supports"),
            patch: json!({ "RetentionPeriod": retention(max + 1) }),
            expected_message: past_the_bound.clone(),
        },
        Refusal {
            case: format!("{resource_type}: at the endpoint of the model's own range"),
            patch: json!({ "RetentionPeriod": retention(RETENTION_VALUE_MAX) }),
            expected_message: past_the_bound.clone(),
        },
        Refusal {
            case: format!(
                "{resource_type}: an unsupported period beside members that would otherwise apply"
            ),
            patch: json!({
                "RetentionPeriod": retention(max + 1),
                "Description": "a description a refused request must not apply",
                "ResourceTags": [{ "ResourceTagKey": "retain", "ResourceTagValue": "later" }],
            }),
            expected_message: past_the_bound,
        },
        Refusal {
            case: format!(
                "{resource_type}: exclusion tags onto a rule that selects by resource tags"
            ),
            patch: json!({
                "ExcludeResourceTags": [{ "ResourceTagKey": "scratch", "ResourceTagValue": "1" }],
            }),
            expected_message: one_validation_error(
                EXCLUDE_RESOURCE_TAGS_MEMBER,
                TAG_LEVEL_REQUIREMENT,
            ),
        },
    ]
}

/// Assert one refusal: the modelled `ValidationException`, its fault class and the clause
/// naming the member that could not hold and the requirement it failed.
fn assert_refused(case: &str, status: u16, body: &Value, expected_message: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} / {body:?}"
    );
    assert_eq!(
        error_code(body),
        "ValidationException",
        "case {case:?}: expected the modelled ValidationException, got {body:?}"
    );
    assert_eq!(
        body["Reason"].as_str(),
        Some(INVALID_PARAMETER_VALUE),
        "case {case:?}: expected Reason={INVALID_PARAMETER_VALUE}, got {:?}",
        body["Reason"]
    );
    assert_eq!(
        message(body),
        expected_message,
        "case {case:?}: expected the message {expected_message:?}, got {:?}",
        message(body)
    );
}

#[tokio::test]
async fn a_refused_update_leaves_the_stored_rule_exactly_as_it_was() {
    let router = router();
    for resource_type in RESOURCE_TYPES {
        let max = supported_retention_max(resource_type);
        // The rule under test starts at the longest period its type supports — the bound
        // itself is accepted — and carries every member a refusal could disturb.
        let (status, created) = create_rule(
            &router,
            json!({
                "ResourceType": resource_type,
                "RetentionPeriod": retention(max),
                "Description": "the rule a refused update must leave alone",
                "ResourceTags": [{ "ResourceTagKey": "retain", "ResourceTagValue": "yes" }],
            }),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {resource_type:?}: the rule to patch must be created, got {status} / {created:?}"
        );
        let identifier = created["Identifier"]
            .as_str()
            .unwrap_or_else(|| panic!("case {resource_type:?}: CreateRule reported no Identifier"))
            .to_string();

        // Let the create settle, so a refused update that wrongly re-entered `pending`
        // would show in the status a later reader sees.
        tokio::time::sleep(past_the_transition()).await;
        let (status, baseline) = get_rule(&router, &identifier).await;
        assert_eq!(
            status, 200,
            "case {resource_type:?}: expected HTTP 200 from GetRule, got {status} / {baseline:?}"
        );
        assert_eq!(
            baseline["Status"].as_str(),
            Some(RULE_STATUS_AVAILABLE),
            "case {resource_type:?}: the rule to patch must have settled to \
             {RULE_STATUS_AVAILABLE}, got {:?}",
            baseline["Status"]
        );

        for Refusal {
            case,
            patch,
            expected_message,
        } in refusals_for(resource_type)
        {
            let (status, body) = update_rule(&router, &identifier, patch.clone()).await;
            assert_refused(&case, status, &body, &expected_message);

            // The whole stored record is what it was before the refused request: the
            // period, the description, both selections and the status the clock had
            // already reached.
            let (status, fetched) = get_rule(&router, &identifier).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from GetRule, got {status} / {fetched:?}"
            );
            assert_eq!(
                fetched, baseline,
                "case {case:?}: a refused UpdateRule must leave the stored rule {baseline:?}, \
                 got {fetched:?}"
            );
        }

        // The complement: an update the stored type DOES support is applied, so the
        // invariant above is the refusal's doing and not an `UpdateRule` that writes
        // nothing at all.
        let (status, updated) = update_rule(
            &router,
            &identifier,
            json!({
                "RetentionPeriod": retention(RETENTION_VALUE_MIN),
                "Description": "an accepted update",
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {resource_type:?}: an update inside the stored type's bound must be \
             accepted, got {status} / {updated:?}"
        );
        assert_eq!(
            updated["Status"].as_str(),
            Some(RULE_STATUS_PENDING),
            "case {resource_type:?}: a just-modified rule must report \
             Status={RULE_STATUS_PENDING}, got {:?}",
            updated["Status"]
        );
        // ROUND TRIP: the accepted members are the ones a later read reports.
        let (status, fetched) = get_rule(&router, &identifier).await;
        assert_eq!(
            status, 200,
            "case {resource_type:?}: expected HTTP 200 from GetRule, got {status} / {fetched:?}"
        );
        assert_eq!(
            fetched["RetentionPeriod"]["RetentionPeriodValue"].as_i64(),
            Some(RETENTION_VALUE_MIN),
            "case {resource_type:?}: expected the accepted period {RETENTION_VALUE_MIN}, got {:?}",
            fetched["RetentionPeriod"]
        );
        assert_eq!(
            fetched["Description"].as_str(),
            Some("an accepted update"),
            "case {resource_type:?}: expected the accepted description, got {:?}",
            fetched["Description"]
        );
    }
}

// ---------------------------------------------------------------------------
// An ACCEPTED `UpdateRule` that changes no configuration, held to the same rule as a
// refused one: the stored record is left exactly as it was, the status included.
// ---------------------------------------------------------------------------

/// Every member of a rule's record that both `UpdateRule` and `GetRule` report, so a
/// response can be held to the stored rule member by member.
/// Source: smithy_query(query="operation", operation="UpdateRule,GetRule") — the
/// intersection of their output members.
const REPORTED_MEMBERS: [&str; 9] = [
    "Identifier",
    "RuleArn",
    "ResourceType",
    "RetentionPeriod",
    "Description",
    "ResourceTags",
    "ExcludeResourceTags",
    "Status",
    "LockState",
];

/// The description the tag-level fixture rule below carries.
const STORED_DESCRIPTION: &str = "the rule an unchanging update must leave alone";

/// The retention period, in days, the tag-level fixture rule below carries — one every
/// modelled resource type supports.
const STORED_RETENTION: i64 = 7;

/// A tag-level rule: a description, a period and a resource selection whose second pair
/// carries NO value (`ResourceTagValue` is optional).
fn tag_level_rule() -> Value {
    json!({
        "ResourceType": RESOURCE_TYPES[0],
        "RetentionPeriod": retention(STORED_RETENTION),
        "Description": STORED_DESCRIPTION,
        "ResourceTags": [
            { "ResourceTagKey": "retain", "ResourceTagValue": "yes" },
            { "ResourceTagKey": "tier" },
        ],
    })
}

/// The resource selection [`tag_level_rule`] stores.
fn stored_resource_tags() -> Value {
    tag_level_rule()["ResourceTags"].clone()
}

/// A Region-level rule carrying exclusion tags, at the longest period its resource type
/// supports.
fn region_level_rule() -> Value {
    json!({
        "ResourceType": RESOURCE_TYPES[1],
        "RetentionPeriod": retention(supported_retention_max(RESOURCE_TYPES[1])),
        "ExcludeResourceTags": [{ "ResourceTagKey": "scratch", "ResourceTagValue": "1" }],
    })
}

/// The exclusion selection [`region_level_rule`] stores.
fn stored_exclude_resource_tags() -> Value {
    region_level_rule()["ExcludeResourceTags"].clone()
}

/// A rule carrying NO description, so an absent member is covered on a rule whose stored
/// value is absent as well as on one whose value is present.
fn undescribed_rule() -> Value {
    json!({
        "ResourceType": RESOURCE_TYPES[2],
        "RetentionPeriod": retention(45),
    })
}

/// One row: the rule to create, the patch to send it, and whether that patch changes the
/// rule's configuration.
struct Patch {
    case: &'static str,
    create: Value,
    patch: Value,
    modifies: bool,
}

/// Patches that change NOTHING about the rule they address, varied along what the rule
/// generalises over: which members the request carries and how many, the values it
/// re-sends, which of the two selections it names, the spelling of a valueless pair,
/// whether the stored value is present or absent, and the published retention bound.
fn unchanging_patches() -> Vec<Patch> {
    vec![
        Patch {
            case: "no optional member at all, on a tag-level rule",
            create: tag_level_rule(),
            patch: json!({}),
            modifies: false,
        },
        Patch {
            case: "no optional member at all, on a Region-level rule with exclusion tags",
            create: region_level_rule(),
            patch: json!({}),
            modifies: false,
        },
        Patch {
            case: "no optional member at all, on a rule carrying no description",
            create: undescribed_rule(),
            patch: json!({}),
            modifies: false,
        },
        Patch {
            case: "the stored retention period",
            create: tag_level_rule(),
            patch: json!({ "RetentionPeriod": retention(STORED_RETENTION) }),
            modifies: false,
        },
        Patch {
            case: "the stored retention period at the maximum its type supports",
            create: region_level_rule(),
            patch: json!({
                "RetentionPeriod": retention(supported_retention_max(RESOURCE_TYPES[1])),
            }),
            modifies: false,
        },
        Patch {
            case: "the stored description",
            create: tag_level_rule(),
            patch: json!({ "Description": STORED_DESCRIPTION }),
            modifies: false,
        },
        Patch {
            case: "the stored resource tags",
            create: tag_level_rule(),
            patch: json!({ "ResourceTags": stored_resource_tags() }),
            modifies: false,
        },
        Patch {
            // A pair carrying no value and the same pair carrying the empty value select
            // the same resources, so re-sending one as the other selects exactly what the
            // rule already selects.
            case: "the stored resource tags, the valueless pair written as an empty value",
            create: tag_level_rule(),
            patch: json!({
                "ResourceTags": [
                    { "ResourceTagKey": "retain", "ResourceTagValue": "yes" },
                    { "ResourceTagKey": "tier", "ResourceTagValue": "" },
                ],
            }),
            modifies: false,
        },
        Patch {
            case: "the stored exclusion tags",
            create: region_level_rule(),
            patch: json!({ "ExcludeResourceTags": stored_exclude_resource_tags() }),
            modifies: false,
        },
        Patch {
            case: "every stored member at once",
            create: tag_level_rule(),
            patch: json!({
                "RetentionPeriod": retention(STORED_RETENTION),
                "Description": STORED_DESCRIPTION,
                "ResourceTags": stored_resource_tags(),
            }),
            modifies: false,
        },
        Patch {
            // "This parameter is currently not supported. You can't update a retention
            // rule's resource type after creation", so a patch naming only it carries no
            // change the service applies.
            case: "a resource type alone, which cannot be updated",
            create: tag_level_rule(),
            patch: json!({ "ResourceType": RESOURCE_TYPES[2] }),
            modifies: false,
        },
    ]
}

/// Patches that DO change the rule they address — the complement, so the rule above is
/// what an unchanging request does and not an `UpdateRule` that stopped writing.
fn changing_patches() -> Vec<Patch> {
    vec![
        Patch {
            case: "a different retention period",
            create: tag_level_rule(),
            patch: json!({ "RetentionPeriod": retention(STORED_RETENTION + 23) }),
            modifies: true,
        },
        Patch {
            case: "the minimum retention period",
            create: tag_level_rule(),
            patch: json!({ "RetentionPeriod": retention(RETENTION_VALUE_MIN) }),
            modifies: true,
        },
        Patch {
            case: "a different description",
            create: tag_level_rule(),
            patch: json!({ "Description": "renamed" }),
            modifies: true,
        },
        Patch {
            case: "a description on a rule that carried none",
            create: undescribed_rule(),
            patch: json!({ "Description": "named at last" }),
            modifies: true,
        },
        Patch {
            case: "an empty description on a rule that carried none",
            create: undescribed_rule(),
            patch: json!({ "Description": "" }),
            modifies: true,
        },
        Patch {
            case: "one more resource tag",
            create: tag_level_rule(),
            patch: json!({
                "ResourceTags": [
                    { "ResourceTagKey": "retain", "ResourceTagValue": "yes" },
                    { "ResourceTagKey": "tier" },
                    { "ResourceTagKey": "extra", "ResourceTagValue": "1" },
                ],
            }),
            modifies: true,
        },
        Patch {
            case: "another value for a stored pair",
            create: tag_level_rule(),
            patch: json!({
                "ResourceTags": [
                    { "ResourceTagKey": "retain", "ResourceTagValue": "no" },
                    { "ResourceTagKey": "tier" },
                ],
            }),
            modifies: true,
        },
        Patch {
            // The stored order is the order every reader reports the selection in, so a
            // selection listing the same pairs differently is not the stored one.
            case: "the stored pairs in another order",
            create: tag_level_rule(),
            patch: json!({
                "ResourceTags": [
                    { "ResourceTagKey": "tier" },
                    { "ResourceTagKey": "retain", "ResourceTagValue": "yes" },
                ],
            }),
            modifies: true,
        },
        Patch {
            case: "an empty resource tag list, clearing the selection",
            create: tag_level_rule(),
            patch: json!({ "ResourceTags": [] }),
            modifies: true,
        },
        Patch {
            case: "one more exclusion tag",
            create: region_level_rule(),
            patch: json!({
                "ExcludeResourceTags": [
                    { "ResourceTagKey": "scratch", "ResourceTagValue": "1" },
                    { "ResourceTagKey": "temp", "ResourceTagValue": "yes" },
                ],
            }),
            modifies: true,
        },
    ]
}

/// Read the rule until it reports `available` — the transition the CLOCK owes it, which
/// no call brings forward — and answer with that record.
///
/// The wait is for the CONDITION: it ends when the rule has settled, and its deadline is
/// generous enough to bound only a rule that never settles at all, so nothing here
/// depends on how fast the host runs.
async fn settled_rule(router: &MultiServiceRouter, case: &str, identifier: &str) -> Value {
    const DEADLINE: Duration = Duration::from_secs(30);
    const BETWEEN_READS: Duration = Duration::from_millis(20);
    let started = Instant::now();
    loop {
        let (status, fetched) = get_rule(router, identifier).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetRule, got {status} / {fetched:?}"
        );
        if fetched["Status"].as_str() == Some(RULE_STATUS_AVAILABLE) {
            return fetched;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "case {case:?}: the rule never reached Status={RULE_STATUS_AVAILABLE}, \
             last read {fetched:?}"
        );
        tokio::time::sleep(BETWEEN_READS).await;
    }
}

/// Create the row's rule and hold it until the clock has settled it to `available`,
/// answering with its identifier and the settled record the assertions compare against.
async fn settled_fixture(
    router: &MultiServiceRouter,
    case: &str,
    create: &Value,
) -> (String, Value) {
    let (status, created) = create_rule(router, create.clone()).await;
    assert_eq!(
        status, 201,
        "case {case:?}: setup CreateRule expected HTTP 201, got {status} / {created:?}"
    );
    let identifier = created["Identifier"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateRule reported no Identifier"))
        .to_string();
    let baseline = settled_rule(router, case, &identifier).await;
    (identifier, baseline)
}

#[tokio::test]
async fn an_accepted_update_that_changes_nothing_leaves_the_stored_rule_exactly_as_it_was() {
    let router = router();
    for Patch {
        case,
        create,
        patch,
        modifies,
    } in unchanging_patches()
    {
        assert!(
            !modifies,
            "case {case:?}: this table holds unchanging patches only"
        );
        let (identifier, baseline) = settled_fixture(&router, case, &create).await;

        let (status, updated) = update_rule(&router, &identifier, patch.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateRule, got {status} / {updated:?}"
        );
        // The response is the stored rule, member by member — the status included, so a
        // rule that was retaining resources is still reported as retaining them.
        assert_eq!(
            updated["Status"].as_str(),
            Some(RULE_STATUS_AVAILABLE),
            "case {case:?}: an update that changes nothing must keep \
             Status={RULE_STATUS_AVAILABLE}, got {:?}",
            updated["Status"]
        );
        for member in REPORTED_MEMBERS {
            assert_eq!(
                updated.get(member),
                baseline.get(member),
                "case {case:?}: UpdateRule must report the stored {member} {:?}, got {:?}",
                baseline.get(member),
                updated.get(member)
            );
        }

        // ROUND TRIP: nothing was written, so a later caller reads the same record —
        // the instant the transition fell due included, which a re-entered `pending`
        // would have pushed into the future.
        let (status, fetched) = get_rule(&router, &identifier).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetRule, got {status} / {fetched:?}"
        );
        assert_eq!(
            fetched, baseline,
            "case {case:?}: after an update that changes nothing GetRule must report \
             {baseline:?}, got {fetched:?}"
        );

        let (status, _) = delete_rule(&router, &identifier).await;
        assert_eq!(
            status, 204,
            "case {case:?}: teardown DeleteRule expected HTTP 204, got {status}"
        );
    }
}

#[tokio::test]
async fn an_update_that_changes_the_rule_returns_it_to_pending_and_persists_the_change() {
    let router = router();
    for Patch {
        case,
        create,
        patch,
        modifies,
    } in changing_patches()
    {
        assert!(
            modifies,
            "case {case:?}: this table holds changing patches only"
        );
        let (identifier, baseline) = settled_fixture(&router, case, &create).await;

        let (status, updated) = update_rule(&router, &identifier, patch.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateRule, got {status} / {updated:?}"
        );
        assert_eq!(
            updated["Status"].as_str(),
            Some(RULE_STATUS_PENDING),
            "case {case:?}: a rule that has just been modified must report \
             Status={RULE_STATUS_PENDING}, got {:?}",
            updated["Status"]
        );
        assert_ne!(
            updated["Status"], baseline["Status"],
            "case {case:?}: the modified rule must leave the settled status {:?}",
            baseline["Status"]
        );

        // ROUND TRIP: every member the patch carried is the stored one afterwards, so
        // the status above went with a change that really landed.
        for member in ["RetentionPeriod", "Description"] {
            if let Some(requested) = patch.get(member) {
                assert_eq!(
                    updated.get(member),
                    Some(requested),
                    "case {case:?}: UpdateRule must report the patched {member} \
                     {requested:?}, got {:?}",
                    updated.get(member)
                );
            }
        }
        for member in ["ResourceTags", "ExcludeResourceTags"] {
            if patch.get(member).is_some() {
                assert_eq!(
                    list_of(&updated, member),
                    list_of(&patch, member),
                    "case {case:?}: UpdateRule must report the patched {member} {:?}, got {:?}",
                    list_of(&patch, member),
                    updated.get(member)
                );
            }
        }
        let (status, fetched) = get_rule(&router, &identifier).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetRule, got {status} / {fetched:?}"
        );
        for member in REPORTED_MEMBERS {
            // `Status` alone is the member the CLOCK moves, so it is not compared here:
            // the settle below is what reads it.
            if member == "Status" {
                continue;
            }
            assert_eq!(
                fetched.get(member),
                updated.get(member),
                "case {case:?}: GetRule must report the stored {member} UpdateRule \
                 returned ({:?}), got {:?}",
                updated.get(member),
                fetched.get(member)
            );
        }

        // The clock, and nothing else, brings the modified rule back to `available`.
        let resettled = settled_rule(&router, case, &identifier).await;
        assert_eq!(
            resettled["Status"].as_str(),
            Some(RULE_STATUS_AVAILABLE),
            "case {case:?}: a modified rule must settle back to \
             {RULE_STATUS_AVAILABLE}, got {:?}",
            resettled["Status"]
        );

        let (status, _) = delete_rule(&router, &identifier).await;
        assert_eq!(
            status, 204,
            "case {case:?}: teardown DeleteRule expected HTTP 204, got {status}"
        );
    }
}
