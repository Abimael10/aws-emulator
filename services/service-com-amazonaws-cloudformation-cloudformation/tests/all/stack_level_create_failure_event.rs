// SPDX-License-Identifier: CC0-1.0

//! WHETHER a failed stack operation records the stack's OWN failure event, how many times, and
//! WHERE in the stream it stands — end to end over the awsQuery wire.
//!
//! RULE: a stack operation CloudFormation rolls back records the stack's own `*_FAILED` event
//! EXACTLY ONCE where a PRE-DEPLOYMENT VALIDATION stage is what reached the failure — that
//! stage runs before anything is asked of any owning service, so no resource of the stack has
//! an event that could carry the failure — and that event stands BEFORE the event of the
//! rollback the failure owes and carries `DetailedStatus` `VALIDATION_FAILED`. Every other
//! failure such an operation reaches is reported by the rollback's own event, which begins at
//! the same instant, so the stack records NO failure event of its own. A request that asked for
//! the failed state to be KEPT (`OnFailure=DO_NOTHING`, `DisableRollback=true`) gets a stack
//! that RESTS in the failed status, and then its own event reports that status whatever stage
//! reached it.
//!
//! Why it matters: deployment tooling that waits for, or branches on, a stack-level
//! `CREATE_FAILED` event rather than polling `StackStatus` sees the stack-level failure only if
//! that event exists; one that counts the events sees the operation fail twice if it exists
//! more than once; and one that reads the stream in order to decide whether the rollback had
//! already started sees a failure recorded after its own rollback if the order slips.
//!
//! The rows differ along what the rule generalises over: WHICH stage ended the operation (the
//! property validation, the existence check, the stage that refuses a transform directive it
//! cannot apply, an owning service's own refusal), WHAT the request asked for on failure (the
//! documented default rollback, `OnFailure=DO_NOTHING`, `DisableRollback=true`) and WHICH
//! operation failed (a create, an update). Two create rows share one template and one collision
//! and differ only in whether the validation stage ran (`DisableValidation`), which is the pair
//! that pins the discrimination: the same collision, recorded on the stack's own event where a
//! validation caught it and on the rollback's where the owning service did. The control row
//! deploys.
//!
//! The templates and the collision are this target's one fixture set for the stages a stack
//! operation can fail at, shared with `event_detailed_status.rs` — which owns WHICH events
//! carry a detailed status, as `superseded_failure_events.rs` owns which logical ids carry a
//! failed status and `one_stack_event_per_failure.rs` how many events repeat one failure's
//! reason. What this suite adds is the COUNT of the stack's own failure event and its PLACE in
//! the stream.

use serde_json::{json, Map, Value};

use crate::event_detailed_status::{
    colliding_group_template, hold_the_held_group, DEPLOYABLE_TEMPLATE, REFUSED_TOPIC_TEMPLATE,
    UNPROCESSABLE_TEMPLATE, UNSUPPORTED_PROPERTY_TEMPLATE, VALIDATION_FAILED,
};
use crate::harness::{
    call, create_stack, member, router, settled_stack_status, stack_own_events, StackOwnEvent,
    REGION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{
    early_validation, failed_action, resource_failure_summary, status,
};

/// The logical id [`REFUSED_TOPIC_TEMPLATE`] declares its refused topic under, which the
/// summary a stack resting in the failed status reports names.
const REFUSED_LOGICAL_ID: &str = "Notifications";

/// The `Resources` section every template above declares its resources in.
const RESOURCES: &str = "Resources";

/// One template declaring everything `base` declares and everything `extra` declares — how the
/// update rows ADD a declaration to the template their create deployed, so neither declaration
/// is spelled a second time here.
fn merged(base: &str, extra: &str) -> String {
    let mut resources = Map::new();
    for body in [base, extra] {
        let parsed: Value = serde_json::from_str(body).expect("a well-formed template");
        let declared = parsed
            .get(RESOURCES)
            .and_then(Value::as_object)
            .expect("a template declaring resources");
        for (logical_id, declaration) in declared {
            resources.insert(logical_id.clone(), declaration.clone());
        }
    }
    json!({RESOURCES: resources}).to_string()
}

/// What one row expects of the stack's own failure event: the detailed status it carries and the
/// sentence it reports. `None` where the operation leaves the stack no failure event at all.
struct ExpectedFailure {
    detailed_status: &'static str,
    reason: String,
}

/// Assert the stack's own events against what a row expects, and the three properties the rule
/// states about them.
///
/// `sequence` is the whole sequence of statuses, oldest first; `failed_status` the operation's
/// own failed status and `rollback_status` the status of the rollback that failure owes, so the
/// ORDER between them can be asserted wherever both stand.
fn assert_the_stacks_own_failure(
    case: &str,
    recorded: &[StackOwnEvent],
    sequence: &[&str],
    failed_status: &str,
    rollback_status: &str,
    expected: &Option<ExpectedFailure>,
) {
    let statuses: Vec<&str> = recorded
        .iter()
        .map(|event| event.resource_status.as_str())
        .collect();
    assert_eq!(
        statuses, sequence,
        "case {case:?}: expected the stack's own events to be {sequence:?}, oldest first, got \
         {recorded:?}"
    );

    // HOW MANY events report the operation's failed status: the count a client selecting the
    // stack-scoped failure reads, which is one exactly where the row says the stack records
    // that failure itself.
    let failures: Vec<&StackOwnEvent> = recorded
        .iter()
        .filter(|event| event.resource_status == failed_status)
        .collect();
    let owed = usize::from(expected.is_some());
    assert_eq!(
        failures.len(),
        owed,
        "case {case:?}: expected exactly {owed} stack-level {failed_status} event(s), got \
         {failures:?}"
    );
    let (Some(failure), Some(expected)) = (failures.first(), expected.as_ref()) else {
        return;
    };

    // WHAT it reports: the sentence the stage that failed the operation words, and the
    // classification of that failure as a validation's exactly where a validation reached it.
    assert_eq!(
        failure.reason, expected.reason,
        "case {case:?}: expected the stack-level {failed_status} event to report \
         {:?}, got {failure:?}",
        expected.reason
    );
    assert_eq!(
        failure.detailed_status, expected.detailed_status,
        "case {case:?}: expected the stack-level {failed_status} event to report \
         DetailedStatus {:?}, got {failure:?}",
        expected.detailed_status
    );

    // WHERE it stands: before the rollback that failure owes, where the stack rolled back at
    // all — the stream is what the stack DID, in the order it did it.
    if let Some(rolled_back_at) = statuses.iter().position(|status| *status == rollback_status) {
        let failed_at = statuses
            .iter()
            .position(|status| *status == failed_status)
            .unwrap_or(usize::MAX);
        assert!(
            failed_at < rolled_back_at,
            "case {case:?}: expected the stack-level {failed_status} event before its \
             {rollback_status} event, got {statuses:?}"
        );
    }
}

/// One create row: what it submits, and the stack's own events it must leave behind.
struct CreateRow {
    case: &'static str,
    stack: &'static str,
    /// Whether CloudWatch Logs is given the held group before the create, so the name the
    /// colliding declaration states is already taken outside CloudFormation.
    holds_the_group: bool,
    template: String,
    /// What the request states beyond its template.
    extra: &'static [(&'static str, &'static str)],
    /// Every event the stack records of ITS OWN status, oldest first. The last of them is the
    /// status the stack settles in.
    stack_statuses: &'static [&'static str],
    /// The stack's own `CREATE_FAILED` event, where the create leaves one.
    expected_failure: Option<ExpectedFailure>,
}

/// The create rows, in the order the test drives them.
fn create_rows() -> Vec<CreateRow> {
    // Both validation stages report the same summary on the stack: which stage failed is read
    // off the validation result `DescribeEvents` carries, not off the stack's own sentence.
    let one_validation_error = || ExpectedFailure {
        detailed_status: VALIDATION_FAILED,
        reason: early_validation::validation_failure_summary(1),
    };
    vec![
        CreateRow {
            case: "a declaration carrying a member its type has no property for",
            stack: "stack-failure-event-property",
            holds_the_group: false,
            template: UNSUPPORTED_PROPERTY_TEMPLATE.to_string(),
            extra: &[],
            stack_statuses: &[
                status::CREATE_IN_PROGRESS,
                status::CREATE_FAILED,
                status::ROLLBACK_IN_PROGRESS,
                status::ROLLBACK_COMPLETE,
            ],
            expected_failure: Some(one_validation_error()),
        },
        CreateRow {
            case: "the same declaration on a create that keeps its failed state",
            stack: "stack-failure-event-property-kept",
            holds_the_group: false,
            template: UNSUPPORTED_PROPERTY_TEMPLATE.to_string(),
            extra: &[("DisableRollback", "true")],
            stack_statuses: &[status::CREATE_IN_PROGRESS, status::CREATE_FAILED],
            expected_failure: Some(one_validation_error()),
        },
        CreateRow {
            case: "a log group declared at a name the owning service already holds",
            stack: "stack-failure-event-conflict",
            holds_the_group: true,
            template: colliding_group_template(),
            extra: &[],
            stack_statuses: &[
                status::CREATE_IN_PROGRESS,
                status::CREATE_FAILED,
                status::ROLLBACK_IN_PROGRESS,
                status::ROLLBACK_COMPLETE,
            ],
            expected_failure: Some(one_validation_error()),
        },
        CreateRow {
            // The discriminating twin of the row above: the same collision, left to the owning
            // service to refuse, so the stack records no failure event of its own.
            case: "the same collision on a create that turned the validations off",
            stack: "stack-failure-event-deferred",
            holds_the_group: true,
            template: colliding_group_template(),
            extra: &[("DisableValidation", "true")],
            stack_statuses: &[
                status::CREATE_IN_PROGRESS,
                status::ROLLBACK_IN_PROGRESS,
                status::ROLLBACK_COMPLETE,
            ],
            expected_failure: None,
        },
        CreateRow {
            case: "a template whose declared transform cannot be applied to it",
            stack: "stack-failure-event-transform",
            holds_the_group: false,
            template: UNPROCESSABLE_TEMPLATE.to_string(),
            // The transform the template declares has to be acknowledged for the create to be
            // accepted at all, which is what lets it reach the stage that refuses the
            // directive.
            extra: &[("Capabilities.member.1", "CAPABILITY_AUTO_EXPAND")],
            stack_statuses: &[
                status::CREATE_IN_PROGRESS,
                status::ROLLBACK_IN_PROGRESS,
                status::ROLLBACK_COMPLETE,
            ],
            expected_failure: None,
        },
        CreateRow {
            case: "a topic the owning service refuses to create",
            stack: "stack-failure-event-refused",
            holds_the_group: false,
            template: REFUSED_TOPIC_TEMPLATE.to_string(),
            extra: &[],
            stack_statuses: &[
                status::CREATE_IN_PROGRESS,
                status::ROLLBACK_IN_PROGRESS,
                status::ROLLBACK_COMPLETE,
            ],
            expected_failure: None,
        },
        CreateRow {
            case: "the same refusal on a create that keeps its failed state",
            stack: "stack-failure-event-refused-kept",
            holds_the_group: false,
            template: REFUSED_TOPIC_TEMPLATE.to_string(),
            extra: &[("OnFailure", "DO_NOTHING")],
            stack_statuses: &[status::CREATE_IN_PROGRESS, status::CREATE_FAILED],
            // A failure an owning service answered is summarised over the resources the
            // operation could not carry through, and is no validation's verdict.
            expected_failure: Some(ExpectedFailure {
                detailed_status: "",
                reason: resource_failure_summary(failed_action::CREATE, [REFUSED_LOGICAL_ID]),
            }),
        },
        CreateRow {
            case: "a template that deploys",
            stack: "stack-failure-event-control",
            holds_the_group: false,
            template: DEPLOYABLE_TEMPLATE.to_string(),
            extra: &[],
            stack_statuses: &[status::CREATE_IN_PROGRESS, status::CREATE_COMPLETE],
            expected_failure: None,
        },
    ]
}

/// Every create row: the stack's own events are exactly the sequence the row states, so the
/// stack-level `CREATE_FAILED` exists exactly where CloudFormation reached the failure itself,
/// exactly once, and before the rollback it owes.
#[tokio::test]
async fn a_failed_create_records_the_stacks_own_failure_where_cloudformation_reached_it() {
    for row in create_rows() {
        let case = row.case;
        // A router of its own per row: what each case is about is its own operation, not what
        // another row left behind.
        let router = router();
        if row.holds_the_group {
            hold_the_held_group(&router, case).await;
        }

        let extra: Vec<(String, String)> = row
            .extra
            .iter()
            .map(|(key, value)| member(key, value))
            .collect();
        let (code, created) = create_stack(&router, row.stack, &row.template, &extra).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must be accepted — the stack reports the outcome — got \
             {code} {created}"
        );

        let settles_in = *row
            .stack_statuses
            .last()
            .expect("every row states the statuses the stack moves through");
        let settled = settled_stack_status(case, &router, REGION, row.stack).await;
        assert_eq!(
            settled, settles_in,
            "case {case:?}: expected the stack to settle {settles_in}, got {settled:?}"
        );

        let recorded = stack_own_events(&router, case, row.stack).await;
        assert_the_stacks_own_failure(
            case,
            &recorded,
            row.stack_statuses,
            status::CREATE_FAILED,
            status::ROLLBACK_IN_PROGRESS,
            &row.expected_failure,
        );
    }
}

/// One update row: the template the update submits, and the stack's own events it must leave
/// behind once the whole walk — create, update, rollback — has run.
struct UpdateRow {
    case: &'static str,
    stack: &'static str,
    holds_the_group: bool,
    /// The template the update submits, over the one the create deployed.
    updated: String,
    /// The statuses the stack's own events must END in, oldest first — the update's own
    /// transitions, which follow the create's `CREATE_IN_PROGRESS` / `CREATE_COMPLETE`.
    update_statuses: &'static [&'static str],
    /// The stack's own `UPDATE_FAILED` event, where the update leaves one.
    expected_failure: Option<ExpectedFailure>,
}

/// The update rows: the same discrimination on the other operation that deploys a template.
fn update_rows() -> Vec<UpdateRow> {
    vec![
        UpdateRow {
            case: "an update adding a log group at a name the owning service already holds",
            stack: "stack-failure-event-update-conflict",
            holds_the_group: true,
            updated: merged(DEPLOYABLE_TEMPLATE, &colliding_group_template()),
            update_statuses: &[
                status::UPDATE_IN_PROGRESS,
                status::UPDATE_FAILED,
                status::UPDATE_ROLLBACK_IN_PROGRESS,
                status::UPDATE_ROLLBACK_COMPLETE_CLEANUP_IN_PROGRESS,
                status::UPDATE_ROLLBACK_COMPLETE,
            ],
            expected_failure: Some(ExpectedFailure {
                detailed_status: VALIDATION_FAILED,
                reason: early_validation::validation_failure_summary(1),
            }),
        },
        UpdateRow {
            case: "an update adding a topic the owning service refuses to create",
            stack: "stack-failure-event-update-refused",
            holds_the_group: false,
            updated: merged(DEPLOYABLE_TEMPLATE, REFUSED_TOPIC_TEMPLATE),
            update_statuses: &[
                status::UPDATE_IN_PROGRESS,
                status::UPDATE_ROLLBACK_IN_PROGRESS,
                status::UPDATE_ROLLBACK_COMPLETE_CLEANUP_IN_PROGRESS,
                status::UPDATE_ROLLBACK_COMPLETE,
            ],
            expected_failure: None,
        },
    ]
}

/// The same rule on an UPDATE: the stack's own `UPDATE_FAILED` event exists exactly where
/// CloudFormation reached the failure itself, and before the rollback that failure owes.
#[tokio::test]
async fn a_failed_update_records_the_stacks_own_failure_where_cloudformation_reached_it() {
    for row in update_rows() {
        let case = row.case;
        let router = router();
        if row.holds_the_group {
            hold_the_held_group(&router, case).await;
        }

        let (code, created) =
            create_stack(&router, row.stack, DEPLOYABLE_TEMPLATE, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the setup CreateStack must answer 200, got {code} {created}"
        );
        let settled = settled_stack_status(case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::CREATE_COMPLETE,
            "case {case:?}: the create must complete before the update, got {settled:?}"
        );

        let (code, updated) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", row.stack),
                member("TemplateBody", &row.updated),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: UpdateStack must be accepted — the stack reports the outcome — got \
             {code} {updated}"
        );

        let settles_in = *row
            .update_statuses
            .last()
            .expect("every row states the statuses the update moves through");
        let settled = settled_stack_status(case, &router, REGION, row.stack).await;
        assert_eq!(
            settled, settles_in,
            "case {case:?}: expected the stack to settle {settles_in}, got {settled:?}"
        );

        let mut sequence = vec![status::CREATE_IN_PROGRESS, status::CREATE_COMPLETE];
        sequence.extend_from_slice(row.update_statuses);
        let recorded = stack_own_events(&router, case, row.stack).await;
        assert_the_stacks_own_failure(
            case,
            &recorded,
            &sequence,
            status::UPDATE_FAILED,
            status::UPDATE_ROLLBACK_IN_PROGRESS,
            &row.expected_failure,
        );
    }
}
