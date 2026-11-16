// SPDX-License-Identifier: CC0-1.0

//! The SCHEDULE an image pipeline fires on: what `schedule.scheduleExpression` has to be,
//! and how a value this service cannot read is refused.
//!
//! "The expression determines how often EC2 Image Builder evaluates your
//! `pipelineExecutionStartCondition`. You can specify a cron expression, or a rate expression
//! such as `rate(1 day)`" (`smithy_query(query="doc",shape="Schedule",member="scheduleExpression")`)
//! — so the value is an expression in a GRAMMAR, which the model cannot state: its
//! `NonEmptyString` target carries a length bound and no pattern, so the syntax is the
//! service's own check rather than one the centralized constraint walk can make.
//!
//! The grammar, and the two parts a refusal of an expression states — the service's statement
//! that the value is not a schedule, and the parser's statement of what stopped it, naming
//! the offset into the expression body and the token it stopped on — are the shared reading
//! of a schedule expression (`service_common::schedule_expression`), which every service that
//! takes one reads it with, so an expression is read here exactly as it is at those services
//! and no second cron grammar is written for this one. What this module adds is only which
//! PARAMETER of this API the refusal names, and the one sentence this service states every
//! parameter refusal in.

use crate::ops::types::Schedule;

/// The parameter a refusal of a pipeline's schedule expression names, as the API addresses
/// it: the member of the `schedule` structure the value sat at.
pub const SCHEDULE_EXPRESSION_MEMBER: &str = "schedule.scheduleExpression";

/// The start condition a stored schedule carries when the request named none.
///
/// "The start condition configures when the pipeline should trigger a new image build … If no
/// value is set Image Builder defaults to
/// `EXPRESSION_MATCH_AND_DEPENDENCY_UPDATES_AVAILABLE`"
/// (`smithy_query(query="doc",shape="Schedule",member="pipelineExecutionStartCondition")`) —
/// so the condition a pipeline will actually start on is the one a read reports, exactly as
/// the test settings a create stores report the defaults Image Builder fills in for them.
pub const DEFAULT_PIPELINE_EXECUTION_START_CONDITION: &str =
    "EXPRESSION_MATCH_AND_DEPENDENCY_UPDATES_AVAILABLE";

/// The schedule a create or an update STORES for the one its request carried: the members as
/// submitted, with the documented default filled in where the request named no start
/// condition ([`DEFAULT_PIPELINE_EXECUTION_START_CONDITION`]).
///
/// A request that carried no schedule at all stores none: "If you don't provide a schedule,
/// the pipeline runs only when you call StartImagePipelineExecution."
#[must_use]
pub fn stored_schedule(schedule: Option<Schedule>) -> Option<Schedule> {
    schedule.map(|schedule| Schedule {
        pipeline_execution_start_condition: schedule
            .pipeline_execution_start_condition
            .or_else(|| Some(DEFAULT_PIPELINE_EXECUTION_START_CONDITION.to_string())),
        ..schedule
    })
}

/// Why the schedule expression `schedule` carries is refused, or `None` when the request
/// carries a schedule this service can read — no schedule at all included: "If you don't
/// provide a schedule, the pipeline runs only when you call StartImagePipelineExecution",
/// and a `schedule` structure carrying no expression names no occurrence either.
///
/// The refusal is this service's one parameter-value sentence
/// (`service_common::validation_message::parameter_value_mismatch`), naming the member the
/// value sat at, whose requirement clause is the parser's own statement of what stopped it.
/// It is read BEFORE anything is written, so a pipeline a refused create named is never
/// stored and the name it asked for stays free.
#[must_use]
pub fn unreadable_schedule_expression(schedule: Option<&Schedule>) -> Option<String> {
    let expression = schedule?.schedule_expression.as_deref()?;
    ::service_common::schedule_expression::parse(expression)
        .err()
        .map(|detail| {
            ::service_common::validation_message::parameter_value_mismatch(
                SCHEDULE_EXPRESSION_MEMBER,
                &detail,
            )
        })
}

#[cfg(test)]
mod tests {
    //! The sentence, over the expressions this service's own member carries. The wire
    //! behaviour — that the create and the update of a pipeline both refuse an unreadable
    //! expression and store nothing — is asserted through the router in
    //! `tests/all/image_pipeline_schedule.rs`.

    use super::*;

    /// One schedule carrying `expression`.
    fn schedule(expression: &str) -> Schedule {
        Schedule {
            schedule_expression: Some(expression.to_string()),
            timezone: None,
            pipeline_execution_start_condition: None,
            auto_disable_policy: None,
        }
    }

    /// RULE: an expression the parser cannot read is refused as the value of the member it
    /// arrived in, with the parser's statement of what stopped it; one it can read, and a
    /// request with no expression at all, are not refused.
    #[test]
    fn an_unreadable_expression_is_refused_as_that_members_value() {
        // (case, the expression, the requirement the refusal states — or `None` to accept)
        let rows: [(&str, &str, Option<&str>); 5] = [
            ("an expression the parser reads", "cron(0 3 ? * SUN *)", None),
            ("a rate expression", "rate(1 day)", None),
            (
                "a minutes field that is not a minute",
                "cron(not valid)",
                Some(
                    "Invalid schedule expression. Details: Invalid CRON expression: error at \
                     position 0, details: Illegal characters for this position: 'NOT'",
                ),
            ),
            (
                "a day of week that is not a day",
                "cron(0 3 ? * funday *)",
                Some(
                    "Invalid schedule expression. Details: Invalid CRON expression: error at \
                     position 8, details: Illegal characters for this position: 'FUNDAY'",
                ),
            ),
            (
                "a value that is not an expression at all",
                "nightly",
                Some(
                    "Invalid schedule expression. Details: Schedule expressions must have the \
                     following syntax: rate(<number>\\s?(minutes?|hours?|days?)), \
                     cron(<cron_expression>) or at(yyyy-MM-dd'T'HH:mm:ss).",
                ),
            ),
        ];

        for (case, expression, requirement) in rows {
            let refused = unreadable_schedule_expression(Some(&schedule(expression)));
            match requirement {
                Some(requirement) => {
                    let expected = format!(
                        "The value supplied for parameter '{SCHEDULE_EXPRESSION_MEMBER}' is not \
                         valid. {requirement}"
                    );
                    assert_eq!(
                        refused.as_deref(),
                        Some(expected.as_str()),
                        "case {case:?}: expected {expected:?}, got {refused:?}"
                    );
                }
                None => assert!(
                    refused.is_none(),
                    "case {case:?}: expected {expression:?} to be read, got {refused:?}"
                ),
            }
        }

        // A request that carries no schedule, and one whose schedule carries no expression,
        // name no occurrence and are not refusals of a value.
        assert!(
            unreadable_schedule_expression(None).is_none(),
            "case \"no schedule at all\": expected no refusal"
        );
        assert!(
            unreadable_schedule_expression(Some(&Schedule::default())).is_none(),
            "case \"a schedule carrying no expression\": expected no refusal"
        );
    }

    /// RULE: the schedule that is STORED carries the start condition the request named, and
    /// the documented default where it named none; a request with no schedule stores none.
    #[test]
    fn a_stored_schedule_carries_the_start_condition_it_will_run_under() {
        // (case, the condition the request named, the one the stored schedule carries)
        let rows = [
            (
                "a schedule naming no start condition",
                None,
                DEFAULT_PIPELINE_EXECUTION_START_CONDITION,
            ),
            (
                "a schedule naming the other condition",
                Some("EXPRESSION_MATCH_ONLY"),
                "EXPRESSION_MATCH_ONLY",
            ),
            (
                "a schedule naming the default itself",
                Some(DEFAULT_PIPELINE_EXECUTION_START_CONDITION),
                DEFAULT_PIPELINE_EXECUTION_START_CONDITION,
            ),
        ];

        for (case, submitted, expected) in rows {
            let mut carried = schedule("cron(0 3 ? * SUN *)");
            carried.pipeline_execution_start_condition = submitted.map(str::to_string);
            let stored = stored_schedule(Some(carried)).expect("a stored schedule");
            assert_eq!(
                stored.pipeline_execution_start_condition.as_deref(),
                Some(expected),
                "case {case:?}: expected the start condition {expected:?}, got {:?}",
                stored.pipeline_execution_start_condition
            );
            assert_eq!(
                stored.schedule_expression.as_deref(),
                Some("cron(0 3 ? * SUN *)"),
                "case {case:?}: expected the expression to be stored as submitted, got {:?}",
                stored.schedule_expression
            );
        }

        assert!(
            stored_schedule(None).is_none(),
            "case \"no schedule at all\": expected nothing to be stored"
        );
    }
}
