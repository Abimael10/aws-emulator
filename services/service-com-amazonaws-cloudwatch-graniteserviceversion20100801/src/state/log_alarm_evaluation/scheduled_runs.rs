// SPDX-License-Identifier: CC0-1.0

//! RUNNING one execution of the scheduled query CloudWatch manages for a log alarm.
//!
//! "When you create a log alarm, the operation creates a service-managed CloudWatch Logs
//! scheduled query that runs the query string you provide on the schedule you configure. Each
//! scheduled query execution returns one or more aggregated values determined by the
//! AggregationExpression" (`PutLogAlarm`) — the query reads LOG EVENTS and the aggregation is
//! written in that service's own query language, so the whole of a run belongs to CloudWatch
//! Logs: one `StartQuery` of the alarm's query AND its aggregation ([`super::aggregated_query`])
//! and the `GetQueryResults` that reads the aggregated rows back, both made over the in-process
//! callback ([`smithy_server_runtime::OperationContext::callback`]) exactly as an alarm action is
//! executed over it. Nothing here opens a socket, nothing here parses that query language, and no
//! log event is read from anywhere but the CloudWatch Logs this emulator mounts: the values the
//! alarm compares are the ones that service computed.
//!
//! The store's guard is never held across a run: the runs the clock owes are claimed under it
//! ([`super::State::due_log_alarm_runs`]), the two requests then happen with it released, and it
//! is taken again only to record what each run produced
//! ([`super::State::record_log_alarm_run`]).
//!
//! A run that could not be DISPATCHED at all — an emulator with no CloudWatch Logs mounted to
//! ask — records nothing: no execution happened, so none is reported, and the run is made at the
//! next instant the alarm's own schedule span makes one due. A run CloudWatch Logs REFUSED, or
//! one whose query it could not execute, is a different thing: the query or the aggregation the
//! alarm is configured with is one nothing can run, which is what `EvaluationState` reports as a
//! configuration error.
//!
//! A run is made UNDER AN IDENTITY: "the IAM role that CloudWatch assumes when executing the
//! scheduled query against the configured log groups"
//! (`ScheduledQueryConfiguration$ScheduledQueryRoleARN`), so the role is taken on BEFORE the
//! query is started and a role the account's IAM holds that trusts none of the principals this
//! execution is made by stops the run before it reaches the log events — the alarm's own
//! configuration names a role nothing can execute its query under, which is the other
//! configuration error `EvaluationState` reports.

use ::smithy_server_runtime::EmbeddedEmulator;

use super::{aggregated_query, aggregated_values, LogAlarmQueryRun, LogAlarmRunOutcome, QueryRow};

/// How long ONE SETTLE spends waiting for the queries it started to complete.
///
/// A started Insights query reaches its results a short span later (CloudWatch Logs advances it on
/// the clock), so a run polls for that CONDITION rather than sleeping a fixed time — and the
/// WHOLE pass shares this one generous ceiling, so a settle that owes several runs cannot add up
/// to a wait a caller notices. A run that is not complete inside it produced nothing yet, which
/// is reported as the failure it is rather than as a value nothing computed, and the runs the
/// pass did not reach are made by the next settle.
const MAX_PASS_WAIT: ::std::time::Duration = ::std::time::Duration::from_secs(10);

/// How often that poll looks again.
const POLL_INTERVAL: ::std::time::Duration = ::std::time::Duration::from_millis(25);

/// The `QueryStatus` of a query whose results are final: "Complete" — the only status whose rows
/// are the whole answer ("If the value of the Status field is Complete, the results are
/// complete", `GetQueryResults`).
const QUERY_STATUS_COMPLETE: &str = "Complete";

/// The `QueryStatus` of a query this service could not run at all — a query string naming
/// something it cannot execute over the events it holds. That string is the alarm's own query and
/// aggregation, so the alarm reports such a run as a configuration error of its own setup.
const QUERY_STATUS_FAILED: &str = "Failed";

/// The `QueryStatus` values a query that will produce nothing more ends in for a reason that is
/// NOT its own text.
const QUERY_STATUS_ENDED: [&str; 3] = ["Cancelled", "Timeout", "Unknown"];

/// Make every scheduled query execution the clock owes this store's log alarms, record what each
/// produced, and evaluate the alarm it belongs to.
///
/// The one entry point the alarm settle calls ([`super::super::alarm_actions`]): the transitions
/// the runs produce are queued as notifications, so the same settle delivers them.
pub async fn settle_log_alarms<Eng: Send + Sync + 'static>(
    engine: &Eng,
    callback: &EmbeddedEmulator,
) {
    let Some(state) = super::super::service_state(engine) else {
        return;
    };
    let runs = state
        .lock()
        .due_log_alarm_runs(super::super::now_secs());
    // ONE deadline for the whole pass, so the wait a caller's settle inherits is bounded by it
    // however many runs that pass owes.
    let deadline = ::std::time::Instant::now() + MAX_PASS_WAIT;
    for run in runs {
        let Some(outcome) = execute(callback, &run, deadline).await else {
            continue;
        };
        // The fold is dated at the instant of the EXECUTION it folds, not at the instant the run
        // happened to land: the alarm's state is what that execution's aggregate made it, so the
        // state's own instant and the execution's are one instant a client can line up — the
        // execution the alarm rests on is in every window that starts when the alarm moved.
        state
            .lock()
            .record_log_alarm_run(&run, &outcome, run.triggered_secs);
    }
}

/// Run ONE execution, or `None` when it could not be dispatched at all.
async fn execute(
    callback: &EmbeddedEmulator,
    run: &LogAlarmQueryRun,
    deadline: ::std::time::Instant,
) -> Option<LogAlarmRunOutcome> {
    // The identity FIRST: an execution CloudWatch cannot make under the role the alarm names is
    // one that never reaches the log events, so nothing is asked of CloudWatch Logs for it.
    if let Some(refusal) = unassumable_role(callback, run).await {
        ::tracing::warn!(
            alarm = %run.alarm_name,
            role = %run.scheduled_query_role_arn,
            "this log alarm's scheduled query role could not be assumed"
        );
        return Some(LogAlarmRunOutcome {
            failure: Some(refusal),
            // The role the alarm is configured with is one its query cannot be executed under:
            // the setup is what calls for review, which is the configuration-error reading of
            // `EvaluationState`.
            configuration_fault: true,
            // No query was started, so the execution did not end on anything its text says.
            execution_status: Some(super::EXECUTION_STATUS_FAILED),
            ..LogAlarmRunOutcome::default()
        });
    }
    let client = logs_client(callback, &run.region);
    // ONE query: the alarm's query string and the aggregation that folds its rows, which the
    // service that holds the log events compiles, bounds and computes.
    let started = client
        .start_query()
        .query_string(aggregated_query(
            &run.query_string,
            &run.aggregation_expression,
        ))
        .set_log_group_identifiers(
            (!run.log_group_identifiers.is_empty()).then(|| run.log_group_identifiers.clone()),
        )
        .start_time(run.start_secs)
        .end_time(run.end_secs)
        .send()
        .await;
    let query_id = match started {
        Ok(answer) => answer.query_id,
        Err(error) => {
            ::tracing::warn!(
                alarm = %run.alarm_name,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "this log alarm's scheduled query could not be started"
            );
            // Only a REFUSAL OF THE QUERY ITSELF is an execution — and one that says something
            // about the alarm's own configuration: the query or the aggregation is malformed, a
            // parameter of it is not one CloudWatch Logs takes, or the log groups it names are
            // not there. Anything else never reached the service that holds the log events, so no
            // execution happened and none is recorded.
            let refusal = error.as_service_error().and_then(|service| {
                (service.is_malformed_query_exception()
                    || service.is_invalid_parameter_exception()
                    || service.is_resource_not_found_exception())
                .then(|| {
                    ::aws_smithy_types::error::metadata::ProvideErrorMetadata::message(service)
                        .unwrap_or("CloudWatch Logs refused this scheduled query")
                        .to_string()
                })
            });
            return refusal.map(|refusal| LogAlarmRunOutcome {
                failure: Some(refusal),
                configuration_fault: true,
                ..LogAlarmRunOutcome::default()
            });
        }
    };
    // A start that answered no query id names no run to read results from, so there is no
    // execution to record either.
    let query_id = query_id?;

    // "GetQueryResults does not start running a query": the query advances on CloudWatch Logs'
    // own clock, so this waits for the CONDITION — a status whose rows are final — rather than
    // for a span, inside the pass's own ceiling.
    loop {
        let answered = client
            .get_query_results()
            .query_id(&query_id)
            .send()
            .await;
        let answer = match answered {
            Ok(answer) => answer,
            Err(error) => {
                ::tracing::warn!(
                    alarm = %run.alarm_name,
                    query = %query_id,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the results of this log alarm's scheduled query execution could not be read"
                );
                return Some(LogAlarmRunOutcome {
                    query_id: Some(query_id),
                    failure: Some(refusal_message(&error).unwrap_or_else(|| {
                        "the query results could not be read".to_string()
                    })),
                    ..LogAlarmRunOutcome::default()
                });
            }
        };
        let status = answer
            .status
            .as_ref()
            .map(|status| status.as_str().to_string())
            .unwrap_or_default();
        if status == QUERY_STATUS_COMPLETE {
            let rows: Vec<QueryRow> = answer
                .results()
                .iter()
                .map(|row| {
                    row.iter()
                        .filter_map(|field| {
                            Some((field.field()?.to_string(), field.value()?.to_string()))
                        })
                        .collect()
                })
                .collect();
            // The rows come back already folded by the engine, so what is read here is its own
            // result shape and not the expression that produced it.
            let (values, partial) = aggregated_values(&rows);
            return Some(LogAlarmRunOutcome {
                query_id: Some(query_id),
                values,
                partial,
                failure: None,
                configuration_fault: false,
                execution_status: None,
            });
        }
        if status == QUERY_STATUS_FAILED || QUERY_STATUS_ENDED.contains(&status.as_str()) {
            return Some(LogAlarmRunOutcome {
                query_id: Some(query_id),
                failure: Some(format!(
                    "the scheduled query execution ended with the status {status}"
                )),
                // A query this service could not run names something it cannot execute, and that
                // text is the alarm's own query and aggregation: a review of the alarm's setup is
                // what such a run calls for, where a cancelled or timed-out run calls for none.
                configuration_fault: status == QUERY_STATUS_FAILED,
                ..LogAlarmRunOutcome::default()
            });
        }
        if ::std::time::Instant::now() >= deadline {
            return Some(LogAlarmRunOutcome {
                query_id: Some(query_id),
                failure: Some(
                    "the scheduled query execution did not complete, so it produced no result"
                        .to_string(),
                ),
                ..LogAlarmRunOutcome::default()
            });
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Why the role this run must be made under cannot be taken on, or `None` when nothing says it
/// cannot.
///
/// The question is the shared one every service that acts under a caller-supplied role asks of
/// IAM — which role of this account the ARN names, what IAM holds under that name, and what the
/// trust document it carries admits — asked here on a task the clock owes rather than on a
/// request ([`service_common::iam_role::held_role_is_unassumable_by_every_through`]). Only a role
/// IAM ANSWERED about stops a run: a value naming no role of this account, a role this account's
/// IAM holds nothing under (such a role is routinely provisioned out of band, so its absence says
/// nothing about it) and an emulator with no IAM to ask all leave the run to be made.
async fn unassumable_role(callback: &EmbeddedEmulator, run: &LogAlarmQueryRun) -> Option<String> {
    let offered = execution_principals(&run.region);
    let principals: Vec<&str> = offered.iter().map(String::as_str).collect();
    ::service_common::iam_role::held_role_is_unassumable_by_every_through(
        callback,
        &run.region,
        &run.scheduled_query_role_arn,
        &principals,
    )
    .await
    .then(|| {
        // UNVERIFIED WORDING: CloudWatch publishes no sentence for the state reason of an alarm
        // whose query role cannot be assumed, so this one names the role the alarm is configured
        // with and the trust it would need — which is what the review `EVALUATION_ERROR` calls
        // for is about.
        format!(
            "the scheduled query role {} could not be assumed: its trust policy does not let {} \
             assume it",
            run.scheduled_query_role_arn,
            EXECUTING_PRINCIPALS.join(" or ")
        )
    })
}

/// `logs.amazonaws.com`: the service principal of CloudWatch Logs, whose scheduled query RUNS the
/// execution and whose log events it reads.
const LOGS_SERVICE_PRINCIPAL: &str = "logs.amazonaws.com";

/// The suffix every AWS service principal carries after the service's own label, which is what
/// the Regional spelling of one is written around.
const PRINCIPAL_SUFFIX: &str = ".amazonaws.com";

/// The service principals this execution is made by.
///
/// "the IAM role that CloudWatch assumes when executing the scheduled query against the
/// configured log groups" (`ScheduledQueryConfiguration$ScheduledQueryRoleARN`): the execution is
/// the run of the service-managed CloudWatch Logs scheduled query the alarm owns — it reads that
/// service's log events, which is why the run is made with its operations — and the alarm the
/// query belongs to is CloudWatch's own, so a trust document naming either of them names this
/// assume. This service's own principal is the one spelling of it this crate keeps
/// ([`super::super::alarm_actions::CLOUDWATCH_SERVICE_PRINCIPAL`]).
const EXECUTING_PRINCIPALS: [&str; 2] = [
    LOGS_SERVICE_PRINCIPAL,
    super::super::alarm_actions::CLOUDWATCH_SERVICE_PRINCIPAL,
];

/// Those principals as a trust policy may name them: each of them, and the Regional
/// `<service>.<region>.amazonaws.com` spelling a document written for one Region's resources names
/// the same service under — so a document admitting any of them admits the run.
fn execution_principals(region: &str) -> Vec<String> {
    let mut offered: Vec<String> = Vec::new();
    for principal in EXECUTING_PRINCIPALS {
        offered.push(principal.to_string());
        if let Some(service) = principal.strip_suffix(PRINCIPAL_SUFFIX) {
            offered.push(format!("{service}.{region}{PRINCIPAL_SUFFIX}"));
        }
    }
    offered
}

/// What CloudWatch Logs REFUSED this request with, or `None` when it answered nothing at all —
/// which is a request that never reached it rather than one it decided.
///
/// The target's own sentence is what the alarm reports, exactly as it is for an alarm action
/// ([`super::super::alarm_actions`]): a refusal of the alarm's own query string or log groups is
/// the detail an operator needs to correct the configuration.
fn refusal_message<Error, Response>(
    error: &::aws_smithy_runtime_api::client::result::SdkError<Error, Response>,
) -> Option<String>
where
    Error: ::aws_smithy_types::error::metadata::ProvideErrorMetadata
        + ::std::error::Error
        + 'static,
    Response: ::std::fmt::Debug,
{
    let service = error.as_service_error()?;
    Some(
        ::aws_smithy_types::error::metadata::ProvideErrorMetadata::message(service)
            .unwrap_or("CloudWatch Logs refused the scheduled query")
            .to_string(),
    )
}

/// A CloudWatch Logs client over the in-process callback into this same emulator.
///
/// The callback is the framework's documented seam for calling another emulated service: it
/// routes in process, so no network client is configured here. `region` is the Region the ALARM
/// lives in, so a run reads the log groups of that Region.
fn logs_client(
    callback: &EmbeddedEmulator,
    region: &str,
) -> ::aws_sdk_cloudwatchlogs::Client {
    let config = ::aws_sdk_cloudwatchlogs::Config::builder()
        .behavior_version(::aws_sdk_cloudwatchlogs::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_cloudwatchlogs::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://logs.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client is
        // not given. The same goes for caching the identity: the credentials are handed over
        // directly.
        .retry_config(::aws_sdk_cloudwatchlogs::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_cloudwatchlogs::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_cloudwatchlogs::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_cloudwatchlogs::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_cloudwatchlogs::Client::from_conf(config)
}
