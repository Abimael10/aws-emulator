// SPDX-License-Identifier: CC0-1.0

//! The `name` floor and the `priority` form are ONE rule each, held over every
//! operation that writes the member.
//!
//! The rule under test: a value a request stores into a rule's `name` or its
//! `priority` is judged by the same requirement whichever operation carries it —
//! `CreateAutomationRule`, which requires the name and may carry a priority, and
//! `UpdateAutomationRule`, which REPLACES either — so the two accept the same
//! values, refuse the same values, and word the refusal with the same report:
//!
//! * `name` — "Must be 1-128 characters long"
//!   (`smithy_query(query="doc",shape="UpdateAutomationRuleRequest",member="name")`),
//!   of which the model's `RuleName` carries only the ceiling (`@length(min: 0,
//!   max: 128)`, `smithy_query(query="shape",shape="RuleName")`), so the floor is
//!   the service's own while the ceiling is the protocol boundary's;
//! * `priority` — "A string representation of a decimal number between 0 and 1
//!   (having up to 30 digits after the decimal point)"
//!   (`smithy_query(query="doc",shape="CreateAutomationRuleRequest",member="priority")`),
//!   on a member the model types as a plain `String`, so the whole form is the
//!   service's own.
//!
//! Each is asserted over a TABLE spanning the class rather than over one value:
//! the name cases walk each published bound and one past it, and the priority
//! cases walk the ends of the documented range, the fractional width at and past
//! its documented 30 digits, and the spellings that are not a decimal at all.
//! Every row is driven through BOTH write paths and their answers compared, so a
//! requirement that moved on one path and not on the other fails here.
//!
//! The same holds of a request that breaks MORE than one of the members a rule
//! is made of: the two write paths judge those members in one and the same
//! ORDER, so such a request is refused with one and the same report whichever of
//! them received it — see
//! [`one_request_breaking_several_members_is_refused_the_same_way_on_both_write_paths`].
//!
//! Every case drives the service over the wire through the shared awsJson1_0
//! harness in [`crate::automation_rule_lifecycle`].

use serde_json::{json, Value};
use service_com_amazonaws_computeoptimizerautomation_computeoptimizerautomationservice::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::automation_rule_lifecycle::{
    assert_refused, call, call_ok, emulator, member, minimal_rule, read_rule, string_member,
};

/// The report a value outside the `name` floor is refused with, on either write
/// path.
const NAME_FLOOR_REPORT: &str = "1 validation error detected: Value at 'name' failed to satisfy \
                                 constraint: Member must have length greater than or equal to 1";

/// The report a name past the modelled ceiling is refused with — the protocol
/// boundary's, which is the half of the same sentence the model DOES carry.
const NAME_CEILING_REPORT: &str = "1 validation error detected: Value at 'name' failed to satisfy \
                                   constraint: Member must have length less than or equal to 128";

/// The report a value outside the documented `priority` form is refused with, on
/// either write path.
const PRIORITY_FORM_REPORT: &str =
    "1 validation error detected: Value at 'priority' failed to satisfy constraint: Member must \
     be a decimal number between 0 and 1 with up to 30 digits after the decimal point";

/// What the service must answer for the value under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Stored as sent, and reported back by a later read.
    Accepted,
    /// Refused `InvalidParameterValueException` (HTTP 400) carrying this report,
    /// with nothing written.
    Refused(&'static str),
}

/// The rule every update case is applied to: its ARN, the revision it stands at,
/// and the whole `GetAutomationRule` projection a refused update must leave
/// untouched.
struct Subject {
    arn: String,
    revision: i64,
    snapshot: Value,
}

impl Subject {
    async fn create(router: &MultiServiceRouter, name: &str) -> Self {
        let created = call_ok(router, "CreateAutomationRule", &minimal_rule(name), name).await;
        let arn = string_member(&created, "ruleArn");
        let revision = member(&created, "ruleRevision")
            .as_i64()
            .expect("the created rule carries a revision");
        let snapshot = read_rule(router, &arn, name).await;
        Self {
            arn,
            revision,
            snapshot,
        }
    }
}

/// Drive `member_name: value` through both write paths and assert they answer
/// the same way, which is `verdict`.
///
/// A refused row must leave the store exactly as it was on both paths: the
/// create stores no rule, and the update neither applies the member nor advances
/// the subject's revision. An accepted row must round-trip through
/// `GetAutomationRule` on both paths.
async fn assert_both_write_paths_agree(
    router: &MultiServiceRouter,
    subject: &mut Subject,
    member_name: &str,
    value: &Value,
    verdict: Verdict,
    case: &str,
) {
    // The create: the member under test replaces whatever the minimal rule
    // carries for it, every other member staying valid, so the answer is about
    // this value alone. The name the create would store is the value itself when
    // the member under test IS the name.
    let mut create_request = minimal_rule(&format!("subject-{case}"));
    create_request[member_name] = value.clone();
    let created_name = string_member(&create_request, "name");
    let (create_status, create_body) = call(router, "CreateAutomationRule", &create_request).await;

    // The update: the same member, on a rule that already exists, at the
    // revision it stands at.
    let mut update_request = json!({"ruleArn": &subject.arn, "ruleRevision": subject.revision});
    update_request[member_name] = value.clone();
    let (update_status, update_body) = call(router, "UpdateAutomationRule", &update_request).await;

    match verdict {
        Verdict::Accepted => {
            assert_eq!(
                create_status, 200,
                "case {case:?}: the create must accept this {member_name}, got {create_status} \
                 and {create_body}"
            );
            let fetched = read_rule(router, &string_member(&create_body, "ruleArn"), case).await;
            assert_eq!(
                member(&fetched, member_name),
                value,
                "case {case:?}: the created rule's {member_name} must read back as sent, got \
                 {fetched}"
            );

            assert_eq!(
                update_status, 200,
                "case {case:?}: the update must accept this {member_name}, got {update_status} \
                 and {update_body}"
            );
            subject.revision += 1;
            assert_eq!(
                member(&update_body, "ruleRevision").as_i64(),
                Some(subject.revision),
                "case {case:?}: an applied update bumps the revision by one, got {update_body}"
            );
            let fetched = read_rule(router, &subject.arn, case).await;
            assert_eq!(
                member(&fetched, member_name),
                value,
                "case {case:?}: the updated rule's {member_name} must read back as sent, got \
                 {fetched}"
            );
            subject.snapshot = fetched;
        }
        Verdict::Refused(report) => {
            assert_refused(
                (create_status, create_body.clone()),
                400,
                "InvalidParameterValueException",
                case,
            );
            assert_refused(
                (update_status, update_body.clone()),
                400,
                "InvalidParameterValueException",
                case,
            );
            // Read after the shape is pinned above, so a response that is not
            // this error fails with the case-naming assertion rather than on a
            // missing member.
            let from_create = string_member(&create_body, "message");
            let from_update = string_member(&update_body, "message");
            assert_eq!(
                from_create, report,
                "case {case:?}: expected the create to report {report:?}, got {from_create:?}"
            );
            assert_eq!(
                from_update, report,
                "case {case:?}: expected the update to report {report:?}, got {from_update:?}"
            );

            let listed = call_ok(router, "ListAutomationRules", &json!({}), case).await;
            let stored: Vec<String> = member(&listed, "automationRules")
                .as_array()
                .into_iter()
                .flatten()
                .map(|rule| string_member(rule, "name"))
                .collect();
            assert!(
                !stored.contains(&created_name),
                "case {case:?}: a refused create must store nothing, got {listed}"
            );
            let after = read_rule(router, &subject.arn, case).await;
            assert_eq!(
                after, subject.snapshot,
                "case {case:?}: a refused update must leave the rule, its revision and its \
                 update time alone, got {after}"
            );
        }
    }
}

/// A rule's `name` is held to the same floor by the create and by the update.
///
/// The rows walk the published bounds of "1-128 characters" and one past each:
/// the empty name the floor refuses, one character, 128 characters, and 129,
/// which the modelled `@length` ceiling refuses at the protocol boundary — the
/// same answer on both paths either way. The accepted spellings stay inside the
/// modelled pattern `^[a-zA-Z0-9_-]*$`.
#[tokio::test]
async fn the_name_floor_is_one_rule_over_the_create_and_the_update() {
    let router = emulator();
    let mut subject = Subject::create(&router, "name-floor-subject").await;

    let cases: Vec<(String, String, Verdict)> = vec![
        (
            "no-characters-at-all".to_string(),
            String::new(),
            Verdict::Refused(NAME_FLOOR_REPORT),
        ),
        (
            "one-character".to_string(),
            "a".to_string(),
            Verdict::Accepted,
        ),
        (
            "every-admitted-character-class".to_string(),
            "Rule_9-name".to_string(),
            Verdict::Accepted,
        ),
        (
            "just-inside-the-ceiling".to_string(),
            "n".repeat(127),
            Verdict::Accepted,
        ),
        (
            "at-the-ceiling".to_string(),
            "n".repeat(128),
            Verdict::Accepted,
        ),
        (
            "one-past-the-ceiling".to_string(),
            "n".repeat(129),
            Verdict::Refused(NAME_CEILING_REPORT),
        ),
    ];

    for (case, name, verdict) in &cases {
        assert_both_write_paths_agree(
            &router,
            &mut subject,
            "name",
            &json!(name),
            *verdict,
            case,
        )
        .await;
    }
}

/// A rule's `priority` is held to the same documented form by the create and by
/// the update.
///
/// The rows walk both ends of the documented 0..1 range, the fractional width at
/// and past the documented 30 digits, values outside the range, and the
/// spellings that are not a plain decimal at all (an exponent, a sign, a
/// surrounding space, a half-written decimal, free text).
#[tokio::test]
async fn the_priority_form_is_one_rule_over_the_create_and_the_update() {
    let router = emulator();
    let mut subject = Subject::create(&router, "priority-form-subject").await;

    let refused = Verdict::Refused(PRIORITY_FORM_REPORT);
    let cases: Vec<(String, String, Verdict)> = vec![
        ("the-bottom-of-the-range".to_string(), "0".to_string(), Verdict::Accepted),
        ("the-top-of-the-range".to_string(), "1".to_string(), Verdict::Accepted),
        ("the-middle-of-the-range".to_string(), "0.5".to_string(), Verdict::Accepted),
        (
            "the-top-of-the-range-with-a-zero-fraction".to_string(),
            "1.000".to_string(),
            Verdict::Accepted,
        ),
        (
            "thirty-fractional-digits".to_string(),
            format!("0.{}", "1".repeat(30)),
            Verdict::Accepted,
        ),
        (
            "thirty-one-fractional-digits".to_string(),
            format!("0.{}", "1".repeat(31)),
            refused,
        ),
        ("just-past-the-top".to_string(), "1.0000000001".to_string(), refused),
        ("well-past-the-top".to_string(), "2".to_string(), refused),
        ("below-the-bottom".to_string(), "-0.5".to_string(), refused),
        ("an-explicit-plus-sign".to_string(), "+0.5".to_string(), refused),
        ("an-exponent".to_string(), "5e-1".to_string(), refused),
        ("a-half-written-decimal".to_string(), "0.".to_string(), refused),
        ("no-integral-part".to_string(), ".5".to_string(), refused),
        ("a-surrounding-space".to_string(), " 0.5".to_string(), refused),
        ("a-thousands-separator".to_string(), "0,5".to_string(), refused),
        ("no-characters-at-all".to_string(), String::new(), refused),
        ("free-text".to_string(), "high".to_string(), refused),
    ];

    for (case, priority, verdict) in &cases {
        assert_both_write_paths_agree(
            &router,
            &mut subject,
            "priority",
            &json!(priority),
            *verdict,
            case,
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// One request breaking several members at once
// ---------------------------------------------------------------------------

/// The members a rule a request writes is judged by, in the ONE order both write
/// paths judge them in: the rule's own form first — its `name`, its `priority`,
/// its `recommendedActionTypes`, its `schedule` and its `criteria` — and last the
/// `ruleType`/`organizationConfiguration` pair, whose judgement needs the merged
/// rule and so cannot be made before the stored one has been read.
///
/// Every row of [`one_request_breaking_several_members_is_refused_the_same_way_on_both_write_paths`]
/// takes its expected report from this list, so the order is stated once and each
/// row's expectation follows from it rather than being written out beside it.
const JUDGEMENT_ORDER: [&str; 6] = [
    "name",
    "priority",
    "recommendedActionTypes",
    "schedule",
    "criteria",
    "organizationConfiguration",
];

/// For one member of [`JUDGEMENT_ORDER`]: the request members that break its
/// requirement, and the report the service refuses that break with.
///
/// The reports are built through the service's own single home for the wording
/// ([`state::constraint_report`]), so a row names the member and the requirement
/// rather than a sentence copied beside the implementation. The break for the
/// `organizationConfiguration` pair is the rule type that REQUIRES a
/// configuration sent without one, which is a break of the pair whether the
/// request carries the other half or leaves it stored.
fn broken_member(member_name: &str) -> (Value, String) {
    match member_name {
        "name" => (json!({"name": ""}), NAME_FLOOR_REPORT.to_string()),
        "priority" => (json!({"priority": "7"}), PRIORITY_FORM_REPORT.to_string()),
        "recommendedActionTypes" => (
            json!({"recommendedActionTypes": []}),
            state::constraint_report(
                "recommendedActionTypes",
                "Member must have length greater than or equal to 1",
            ),
        ),
        "schedule" => (
            json!({"schedule": {"scheduleExpression": "* 12 * * ? *"}}),
            state::constraint_report(
                "schedule.scheduleExpression",
                "Member must use a single specific value from 0 to 59 for the minutes field, not \
                 a wildcard, list, range or step",
            ),
        ),
        "criteria" => (
            json!({"criteria": {"region": [{"values": ["us-east-1"]}]}}),
            state::constraint_report("criteria.region.member.comparison", "Member is required"),
        ),
        "organizationConfiguration" => (
            json!({"ruleType": "OrganizationRule"}),
            state::constraint_report(
                "organizationConfiguration",
                "Member is required for the OrganizationRule rule type",
            ),
        ),
        other => panic!("no break is written for the member {other:?}"),
    }
}

/// Splice every member of `overlay` (a JSON object) into `request`.
fn splice(request: &mut Value, overlay: &Value) {
    for (name, value) in overlay
        .as_object()
        .expect("a break is written as a JSON object of request members")
    {
        request[name] = value.clone();
    }
}

/// Drive a request breaking every member of `members` at once through both write
/// paths, and assert the two refuse it with one and the same report: the one
/// [`JUDGEMENT_ORDER`] picks.
///
/// A refused row must leave the store exactly as it was on both paths, as a
/// single-member refusal does: the create stores no rule and the update neither
/// applies a member nor advances the subject's revision.
async fn assert_both_write_paths_word_one_refusal(
    router: &MultiServiceRouter,
    subject: &Subject,
    members: &[&str],
    case: &str,
) {
    let expected = JUDGEMENT_ORDER
        .iter()
        .find(|member_name| members.contains(*member_name))
        .map(|member_name| broken_member(member_name).1)
        .unwrap_or_else(|| panic!("case {case:?}: the row breaks no judged member"));

    let mut create_request = minimal_rule(&format!("subject-{case}"));
    let mut update_request = json!({"ruleArn": &subject.arn, "ruleRevision": subject.revision});
    for member_name in members {
        let (overlay, _) = broken_member(member_name);
        splice(&mut create_request, &overlay);
        splice(&mut update_request, &overlay);
    }
    let created_name = string_member(&create_request, "name");

    let (create_status, create_body) = call(router, "CreateAutomationRule", &create_request).await;
    let (update_status, update_body) = call(router, "UpdateAutomationRule", &update_request).await;

    assert_refused(
        (create_status, create_body.clone()),
        400,
        "InvalidParameterValueException",
        case,
    );
    assert_refused(
        (update_status, update_body.clone()),
        400,
        "InvalidParameterValueException",
        case,
    );
    let from_create = string_member(&create_body, "message");
    let from_update = string_member(&update_body, "message");
    assert_eq!(
        from_create, expected,
        "case {case:?}: expected the create to report {expected:?}, got {from_create:?}"
    );
    assert_eq!(
        from_update, expected,
        "case {case:?}: expected the update to report {expected:?}, got {from_update:?}"
    );
    assert_eq!(
        from_create, from_update,
        "case {case:?}: both write paths must word one refusal alike, got {from_create:?} from \
         the create and {from_update:?} from the update"
    );

    let listed = call_ok(router, "ListAutomationRules", &json!({}), case).await;
    let stored: Vec<String> = member(&listed, "automationRules")
        .as_array()
        .into_iter()
        .flatten()
        .map(|rule| string_member(rule, "name"))
        .collect();
    assert!(
        !stored.contains(&created_name),
        "case {case:?}: a refused create must store nothing, got {listed}"
    );
    let after = read_rule(router, &subject.arn, case).await;
    assert_eq!(
        after, subject.snapshot,
        "case {case:?}: a refused update must leave the rule, its revision and its update time \
         alone, got {after}"
    );
}

/// A request breaking several of a rule's members at once is refused with one and
/// the same report whichever write path received it.
///
/// The rows span the pairs of members that can break together, each member of
/// [`JUDGEMENT_ORDER`] paired with one before and one after it, the pair
/// including the member judged last (the `ruleType`/`organizationConfiguration`
/// pair, whose judgement is the only one an update makes after reading the stored
/// rule), and the row that breaks every member of a rule's own form at once.
#[tokio::test]
async fn one_request_breaking_several_members_is_refused_the_same_way_on_both_write_paths() {
    let router = emulator();
    let subject = Subject::create(&router, "refusal-order-subject").await;

    let rows: Vec<Vec<&str>> = vec![
        vec!["name", "priority"],
        vec!["name", "recommendedActionTypes"],
        vec!["name", "criteria"],
        vec!["priority", "recommendedActionTypes"],
        vec!["priority", "schedule"],
        vec!["recommendedActionTypes", "schedule"],
        vec!["schedule", "criteria"],
        vec!["name", "organizationConfiguration"],
        vec!["priority", "organizationConfiguration"],
        vec!["criteria", "organizationConfiguration"],
        vec![
            "name",
            "priority",
            "recommendedActionTypes",
            "schedule",
            "criteria",
        ],
    ];

    for row in &rows {
        let case = row.join("-and-");
        assert_both_write_paths_word_one_refusal(&router, &subject, row, &case).await;
    }
}
