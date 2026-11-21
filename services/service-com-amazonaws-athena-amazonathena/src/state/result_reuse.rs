// SPDX-License-Identifier: CC0-1.0

//! RESULT REUSE: whether a submission is answered out of an EARLIER query's results rather
//! than by running its statement again.
//!
//! `ResultReuseByAgeConfiguration` is what asks for it: `Enabled` is "true if previous query
//! results can be reused when the query is run; otherwise, false", and `MaxAgeInMinutes`
//! "specifies, in minutes, the maximum age of a previous query result that Athena should
//! consider for reuse. The default is 60." So a submission that asks for reuse is answered
//! out of a previous result when there IS one of the same query inside that window — and
//! the execution then reports `ResultReuseInformation$ReusedPreviousResult` true, "true if a
//! previous query result was reused; false if the result was generated from a new run of the
//! query".
//!
//! Two questions, and both are answered here so the one rule decides them:
//!
//! * WHICH submissions may be answered that way ([`reuse_window`]): the ones whose
//!   configuration enables reuse, for the age it names;
//! * WHICH earlier execution answers one ([`ComAmazonawsAthenaAmazonathenaState::reusable_result`]):
//!   the most recent execution of the SAME query that succeeded inside the window.
//!
//! A reused execution SCANNED nothing: its answer came out of results that were already
//! there, which is why its statistics report no data scanned and no rows read rather than
//! the earlier run's. The rows it serves are the earlier run's own — a result reused inside
//! the window is served whether or not the data under the query has changed since, which is
//! exactly what a client that enables reuse is asking for.
//!
//! Reuse never widens what a caller may read: what a statement may read is decided per
//! caller — the Data Catalog's permissions and Lake Formation's grants are resolved for the
//! principal that submitted it — so a previous result answers only a submission by the SAME
//! principal, and a submission the catalog refuses is not answered out of one at all (the
//! refusal is what such an execution reports).

use super::*;

/// The `MaxAgeInMinutes` a submission that enables reuse without naming one is considered
/// for: "The default is 60" (`ResultReuseByAgeConfiguration$MaxAgeInMinutes`).
const DEFAULT_MAX_AGE_IN_MINUTES: i64 = 60;

/// How long one minute is, which is the unit that member is stated in.
const ONE_MINUTE: Duration = Duration::from_secs(60);

/// The window a submission asks previous results to be considered inside, or `None` when it
/// asks for no reuse at all — a submission carrying no configuration, and one whose
/// configuration is not enabled.
///
/// A window of zero minutes is a window: it enables reuse and admits no previous result,
/// since no earlier execution completed at this very instant or later.
pub fn reuse_window(configuration: Option<&ResultReuseConfiguration>) -> Option<Duration> {
    let by_age = configuration?.result_reuse_by_age_configuration.as_ref()?;
    if !by_age.enabled {
        return None;
    }
    let minutes = by_age
        .max_age_in_minutes
        .map_or(DEFAULT_MAX_AGE_IN_MINUTES, i64::from)
        .max(0);
    Some(ONE_MINUTE.saturating_mul(u32::try_from(minutes).unwrap_or(u32::MAX)))
}

/// What makes an earlier execution's result THIS submission's answer: the query it would run
/// and where it would run it.
///
/// A previous result answers a submission only where every one of these is the same, because
/// each of them decides what the statement computes or who may read it: the statement itself,
/// the catalog and database an unqualified name in it resolves against, the values bound to
/// its parameters, the workgroup it runs in — a workgroup is the unit a query's settings and
/// history belong to, so one workgroup's results are no answer to another's query — and the
/// PRINCIPAL that submitted it, since what a statement may read is resolved per caller.
#[derive(Debug, Clone, Copy)]
pub struct ReuseRequest<'r> {
    /// The statement submitted, byte for byte.
    pub query_string: &'r str,
    /// The workgroup the query runs in.
    pub work_group: &'r str,
    /// The context it runs under, as the request carried it.
    pub context: &'r QueryExecutionContext,
    /// The values bound to the statement's placeholders, in order.
    pub execution_parameters: &'r [String],
    /// The `StatementType` the submitted statement is of.
    pub statement_type: Option<&'r str>,
    /// The principal this submission speaks for, as the credential it presented names it.
    pub submitted_by: &'r str,
    /// How old a previous result may be and still answer this submission ([`reuse_window`]).
    pub max_age: Duration,
}

/// An earlier execution's result, as the submission that REUSES it reports it.
#[derive(Debug, Clone)]
pub struct ReusedResult {
    /// The execution the result came out of.
    pub id: String,
    /// The outcome the reusing execution records: the earlier run's result set and row
    /// count, with no data scanned and no rows read — this execution read nothing, the
    /// result was already there.
    pub outcome: query_engine::Outcome,
}

impl ComAmazonawsAthenaAmazonathenaState {
    /// The previous result that answers `request`, or `None` when no execution this account
    /// holds is one.
    ///
    /// The most recent qualifying execution answers, because the freshest result inside the
    /// window is the one a rerun of the query would have produced most recently. An
    /// execution qualifies when it is the SAME query ([`ReuseRequest`]), it SUCCEEDED, it
    /// produced a result set to serve, and it completed no longer than `max_age` ago.
    ///
    /// Only a statement whose result is a RESULT SET is answered this way: a statement that
    /// defines or changes something — the classes Athena calls DDL and UTILITY — is
    /// performed every time it is submitted, since its effect is the point of running it.
    /// And only the principal the earlier execution was run for is answered out of it: what
    /// a statement may read is decided per caller, so another principal's result is no
    /// answer to this submission.
    pub fn reusable_result(
        &self,
        request: &ReuseRequest<'_>,
        now: SystemTime,
    ) -> Option<ReusedResult> {
        if request.statement_type != Some(query_engine::STATEMENT_TYPE_DML) {
            return None;
        }
        let oldest_completion = now.checked_sub(request.max_age)?;
        self.query_execution_order
            .iter()
            .rev()
            .filter_map(|id| self.query_executions.get(id))
            .filter(|record| self.answers(record, request, now, oldest_completion))
            .find_map(|record| {
                let query_engine::Outcome::Succeeded {
                    result,
                    update_count,
                    ..
                } = record.outcome.as_ref()?
                else {
                    return None;
                };
                Some(ReusedResult {
                    id: record.id.clone(),
                    outcome: query_engine::Outcome::Succeeded {
                        result: result.clone(),
                        update_count: *update_count,
                        // Nothing was scanned and nothing was read: the answer came out of
                        // results that were already there.
                        data_scanned_bytes: 0,
                        input_rows: 0,
                    },
                })
            })
    }

    /// Whether `record` is a previous result that answers `request` at `now`: the same
    /// query, succeeded, and completed no earlier than `oldest_completion`.
    fn answers(
        &self,
        record: &QueryExecutionRecord,
        request: &ReuseRequest<'_>,
        now: SystemTime,
        oldest_completion: SystemTime,
    ) -> bool {
        record.query_string == request.query_string
            && record.work_group == request.work_group
            && record.statement_type.as_deref() == request.statement_type
            && record.submitted_by == request.submitted_by
            && record.context.database == request.context.database
            && record.context.catalog == request.context.catalog
            && record.execution_parameters == request.execution_parameters
            && record.state_at(now) == QUERY_STATE_SUCCEEDED
            && record
                .completion_at(now)
                .is_some_and(|completed| completed >= oldest_completion)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the window a submission asks previous results to be considered inside is the
    /// age its configuration names, in minutes, and reuse is asked for only where the
    /// configuration enables it.
    ///
    /// The rows span what the rule generalises over: no configuration at all, one with no
    /// age of its own (the documented default), reuse turned off, the two published bounds
    /// of the age and a value between them, and the age zero.
    #[test]
    fn the_reuse_window_is_the_age_the_configuration_names() {
        let by_age = |enabled: bool, minutes: Option<i32>| {
            Some(ResultReuseConfiguration {
                result_reuse_by_age_configuration: Some(
                    crate::ops::types::ResultReuseByAgeConfiguration {
                        enabled,
                        max_age_in_minutes: minutes,
                    },
                ),
            })
        };
        // (case, the configuration the submission carried, the window it asks for)
        let cases: Vec<(&str, Option<ResultReuseConfiguration>, Option<Duration>)> = vec![
            ("no configuration at all", None, None),
            (
                "a configuration naming no age, which is the documented default",
                by_age(true, None),
                Some(ONE_MINUTE * 60),
            ),
            ("reuse turned off", by_age(false, Some(60)), None),
            (
                "reuse turned off with no age either",
                by_age(false, None),
                None,
            ),
            ("the smallest age the model admits", by_age(true, Some(0)), Some(Duration::ZERO)),
            ("an age inside the published range", by_age(true, Some(5)), Some(ONE_MINUTE * 5)),
            (
                "the largest age the model admits",
                by_age(true, Some(10_080)),
                Some(ONE_MINUTE * 10_080),
            ),
            (
                "a configuration carrying no age structure",
                Some(ResultReuseConfiguration {
                    result_reuse_by_age_configuration: None,
                }),
                None,
            ),
        ];

        for (case, configuration, expected) in &cases {
            let actual = reuse_window(configuration.as_ref());
            assert_eq!(
                &actual, expected,
                "case {case:?}: expected the reuse window {expected:?}, got {actual:?}"
            );
        }
    }
}
