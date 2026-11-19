// SPDX-License-Identifier: CC0-1.0

//! What an invocation's log delivery does to a log group a CLIENT already created and
//! configured — the group the function's own `LoggingConfig` names.
//!
//! RULE: a function logs to the group its `LoggingConfig$LogGroup` names and to no other —
//! "the name of the Amazon CloudWatch log group the function sends logs to [...] To use a
//! different log group, enter an existing log group or enter a new log group name", with "a
//! default log group named `/aws/lambda/<function name>`" where it names none
//! (`smithy_query(query="doc",shape="LoggingConfig",member="LogGroup")`). An EXISTING group is
//! ADOPTED by that delivery rather than replaced: the name still resolves to exactly one group,
//! and whatever the client configured on it — the `retentionInDays` its own
//! `PutRetentionPolicy` set — is still reported after every run, while a group the delivery
//! itself had to create carries CloudWatch Logs' own defaults and so no retention of its own.
//! The group the function does NOT log to is never created for it.
//!
//! That is what "route my function's logs into the group I manage" means to a client: the group
//! is created and retained once, by the client, and every run's records land in it with its
//! retention intact. A delivery that re-created the group would silently drop the retention a
//! compliance policy depends on, and one that also filled `/aws/lambda/<function name>` would
//! leave the records in a group nothing retains.
//!
//! What the siblings pin and this suite does not restate:
//! [`crate::invocation_log_delivery`] pins WHAT one run puts in its group,
//! [`crate::invocation_log_group_lazy_creation`] the defaults of a group the delivery brought
//! into being, and [`crate::log_group_named_before_it_exists`] that the named group does not
//! exist before the first run. None of them has a client configure the destination FIRST, which
//! is the only way the adoption of an existing group is observable.
//!
//! The rows differ along what the rule generalises over: whether the function names a group at
//! all, whether that group is inside the default `/aws/lambda/` prefix or outside it, whether
//! the declaration names the record format beside the group, whether a client had created and
//! retained the destination before the function ran, which published retention span it set, and
//! which runtime ran — the delivery is the host's, so both engines drive it. Every row invokes
//! twice, so a delivery that re-creates or re-configures the group on any run but the first
//! fails the row.
//!
//! No expected value here is a literal from a recording: the retention spans are the owning
//! service's own published list (quoted on [`PUBLISHED_RETENTIONS`]), every group name is the
//! row's own or derived from the function name it declares, and each marker is built by the row.
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda,
//! CloudWatch Logs and IAM mounted on ONE engine — the harness
//! [`crate::invocation_log_delivery::emulator`] builds, reused rather than copied — so what is
//! asserted is what a client reads off `GetFunctionConfiguration`, `DescribeLogGroups` and
//! `FilterLogEvents`.
//!
//! The test here RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role};
use crate::event_source_mappings::call;
use crate::invocation_log_delivery::{
    create_function_logging, emulator, groups, invocation_request_carrying, logs_answer, searched,
    Created, FUNCTION_ERROR_HEADER, LOGS, LOG_WRITES, NODE_RUNTIME, PYTHON_RUNTIME,
};

/// Where the function operations are bound (`GET /2015-03-31/functions/{FunctionName}/configuration`
/// reads one back).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The prefix of the group a function whose `LoggingConfig` names none logs to: "a default log
/// group named `/aws/lambda/<function name>`" (`LoggingConfig$LogGroup`).
const DEFAULT_PREFIX: &str = "/aws/lambda/";

/// The spans CloudWatch Logs publishes for a log group's retention: "Possible values are: 1, 3,
/// 5, 7, 14, 30, 60, 90, 120, 150, 180, 365, 400, 545, 731, 1096, 1827, 2192, 2557, 2922, 3288,
/// and 3653" (that service's own `Days` shape,
/// `smithy_query(query="doc",shape="Days",model_path="services/service-com-amazonaws-cloudwatchlogs-logs-20140328/smithy-model/com-amazonaws-cloudwatchlogs-logs-20140328.json")`).
/// The rows below take their spans from this list — its two ends and two entries inside it.
const PUBLISHED_RETENTIONS: [i64; 22] = [
    1, 3, 5, 7, 14, 30, 60, 90, 120, 150, 180, 365, 400, 545, 731, 1096, 1827, 2192, 2557, 2922,
    3288, 3653,
];

/// The fixed framing the runtime closes every run it finishes with, which is what says a run's
/// log really was delivered beside the line the handler wrote.
const REPORT_PREFIX: &str = "REPORT RequestId: ";

/// The `LogFormat` the rows that name one declare: "select between plain text and structured
/// JSON" (`smithy_query(query="doc",shape="LoggingConfig",member="LogFormat")`), of which this
/// is the plain-text one — the format the records asserted below are read as.
const TEXT_FORMAT: &str = "Text";

/// The markers the two runs of each row carry, so a record of one run is told from the other's.
const MARKERS: [&str; 2] = ["first-run", "second-run"];

/// A python handler that writes the marker its event carries and answers it.
const PYTHON_SOURCE: &str = "def lambda_handler(event, context):\n    \
                             print('logged %s' % event['marker'])\n    \
                             return {'logged': event['marker']}\n";

/// A node handler writing the same record through that runtime's own console.
const NODE_SOURCE: &str = "exports.handler = async (event) => {\n  \
                           console.log(`logged ${event.marker}`);\n  \
                           return { logged: event.marker };\n};\n";

/// One row: the function it creates, where that function logs, how the declaration says so, and
/// what a client had already done to the destination.
struct Row {
    case: &'static str,
    /// The `FunctionName`, which is also the subject of its default group's name.
    function: &'static str,
    runtime: &'static str,
    source: &'static str,
    /// The group the function's `LoggingConfig` names, or `None` for a create that names none.
    log_group: Option<&'static str>,
    /// Whether the declaration names the record format beside the group.
    names_format: bool,
    /// The index into [`PUBLISHED_RETENTIONS`] of the span a client retained the destination for
    /// BEFORE the function ran, or `None` where nothing created that group beforehand.
    retention: Option<usize>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a declared group outside the default prefix, retained for the shortest \
                   published span before the function ran",
            function: "declared-group-retained-shortest",
            runtime: PYTHON_RUNTIME,
            source: PYTHON_SOURCE,
            log_group: Some("/scenario/declared/retained-shortest"),
            names_format: true,
            retention: Some(0),
        },
        Row {
            case: "a declared group under the default prefix, retained for the longest \
                   published span before the function ran",
            function: "declared-group-retained-longest",
            runtime: NODE_RUNTIME,
            source: NODE_SOURCE,
            log_group: Some("/aws/lambda/declared-group-elsewhere"),
            names_format: false,
            retention: Some(PUBLISHED_RETENTIONS.len() - 1),
        },
        Row {
            case: "a declared group retained for a span inside the published list",
            function: "declared-group-retained-inside",
            runtime: NODE_RUNTIME,
            source: NODE_SOURCE,
            log_group: Some("/aws/vendedlogs/declared-retained-inside"),
            names_format: true,
            retention: Some(5),
        },
        Row {
            case: "a declared group nothing created before the function ran",
            function: "declared-group-unconfigured",
            runtime: PYTHON_RUNTIME,
            source: PYTHON_SOURCE,
            log_group: Some("/scenario/declared/unconfigured"),
            names_format: false,
            retention: None,
        },
        Row {
            case: "a function declaring no group, whose default group nothing created",
            function: "default-group-unconfigured",
            runtime: NODE_RUNTIME,
            source: NODE_SOURCE,
            log_group: None,
            names_format: false,
            retention: None,
        },
        Row {
            case: "a function declaring no group, whose default group a client retained first",
            function: "default-group-retained",
            runtime: PYTHON_RUNTIME,
            source: PYTHON_SOURCE,
            log_group: None,
            names_format: true,
            retention: Some(11),
        },
    ]
}

impl Row {
    /// The whole `LoggingConfig` this row's create carries, or `None` for a create that names
    /// none at all.
    fn logging_config(&self) -> Option<Value> {
        match (self.log_group, self.names_format) {
            (None, false) => None,
            (None, true) => Some(json!({ "LogFormat": TEXT_FORMAT })),
            (Some(group), false) => Some(json!({ "LogGroup": group })),
            (Some(group), true) => {
                Some(json!({ "LogFormat": TEXT_FORMAT, "LogGroup": group }))
            }
        }
    }
}

/// The whole record `DescribeLogGroups` reports for the one group named `group`, or `None` when
/// the account holds no such group — the read a client makes to learn how its group is
/// configured.
async fn described(router: &MultiServiceRouter, case: &str, group: &str) -> Option<Value> {
    let (status, answered) = logs_answer(
        router,
        case,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": group }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeLogGroups for {group:?} to answer 200, got {status}: \
         {answered}"
    );
    answered["logGroups"].as_array().and_then(|found| {
        found
            .iter()
            .find(|reported| reported["logGroupName"] == json!(group))
            .cloned()
    })
}

/// The `retentionInDays` CloudWatch Logs reports for `group`, or `None` where it reports none —
/// which is what a group nothing configured carries.
async fn retention_of(router: &MultiServiceRouter, case: &str, group: &str) -> Option<i64> {
    described(router, case, group)
        .await
        .and_then(|reported| reported["retentionInDays"].as_i64())
}

/// Create the group `group` and retain it for `days`, through CloudWatch Logs' own calls — the
/// way a client sets up the destination it manages before any function logs into it.
async fn given_retained_group(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    days: i64,
) {
    let (status, created) = logs_answer(
        router,
        case,
        "CreateLogGroup",
        &json!({ "logGroupName": group }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateLogGroup {group:?} to answer 200, got {status}: {created}"
    );
    let (status, retained) = logs_answer(
        router,
        case,
        "PutRetentionPolicy",
        &json!({ "logGroupName": group, "retentionInDays": days }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected PutRetentionPolicy on {group:?} to answer 200, got {status}: \
         {retained}"
    );
}

/// Invoke `function` once with `marker`, and assert the invocation itself was clean: HTTP 200,
/// no `FunctionError`, and the handler's own answer in the payload. What became of the run's
/// log is asserted on the group, not here.
async fn invoked(router: &MultiServiceRouter, case: &str, function: &str, marker: &str) {
    let event = json!({ "marker": marker }).to_string();
    let response = router
        .dispatch(invocation_request_carrying(
            function,
            &[],
            event.as_bytes(),
        ))
        .await;
    let (parts, body) = response.into_parts();
    assert_eq!(
        parts.status.as_u16(),
        200,
        "case {case:?}: expected HTTP 200 from Invoke of {function:?} with {marker:?}, got {} {}",
        parts.status,
        String::from_utf8_lossy(&body)
    );
    let failure = parts
        .headers
        .get(FUNCTION_ERROR_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        failure, None,
        "case {case:?}: expected no FunctionError from the run of {marker:?}, got {failure:?} \
         with {}",
        String::from_utf8_lossy(&body)
    );
    let payload: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    assert_eq!(
        payload["logged"],
        json!(marker),
        "case {case:?}: expected the handler's own result for {marker:?}, got {payload}"
    );
}

/// RULE: the group a function's `LoggingConfig` names takes every run's records and is adopted
/// as it stands — one group under that name, still carrying the retention its client gave it —
/// while a destination the delivery had to create carries no retention of its own and the group
/// the function does not log to is never created for it.
#[tokio::test]
async fn the_declared_log_group_takes_the_runs_and_keeps_its_retention() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for row in rows() {
        let case = row.case;
        // One emulator per row: the function, the role and the groups a row reads are its own.
        let router = emulator();
        let role = given_role(
            &router,
            case,
            &format!("{}-writer", row.function),
            Some(json!([allow(LOGS, &LOG_WRITES, "*")])),
        )
        .await;
        let default_group = format!("{DEFAULT_PREFIX}{}", row.function);
        let group = row.log_group.unwrap_or(&default_group).to_string();
        let days = row.retention.map(|index| PUBLISHED_RETENTIONS[index]);

        // 1. The destination as the row's client left it: created and retained through
        //    CloudWatch Logs' own calls, or not there at all.
        if let Some(days) = days {
            given_retained_group(&router, case, &group, days).await;
            let before = retention_of(&router, case, &group).await;
            assert_eq!(
                before,
                Some(days),
                "case {case:?}: expected the group {group:?} to report the retention {days} \
                 before the function exists, got {before:?}"
            );
        } else {
            let listed = groups(&router, case, &group).await;
            assert!(
                listed.is_empty(),
                "case {case:?}: expected no group named {group:?} before anything ran, got \
                 {listed:?}"
            );
        }

        // 2. The function, declaring where it logs.
        create_function_logging(
            &router,
            &Created {
                case,
                name: row.function,
                source: row.source,
                runtime: row.runtime,
                role: &role,
                log_group: row.log_group,
            },
            row.logging_config().as_ref(),
        )
        .await;

        // 3. What Lambda itself reports as the destination: the group the declaration named,
        //    else the documented default for the function's own name.
        let read = call(
            &router,
            "GET",
            &format!("{FUNCTIONS_URI}/{}/configuration", row.function),
            &Value::Null,
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected HTTP 200 from GetFunctionConfiguration, got {} {}",
            read.status,
            read.text()
        );
        let reported = read.json();
        assert_eq!(
            reported["LoggingConfig"]["LogGroup"],
            json!(group.clone()),
            "case {case:?}: expected the function to name {group:?} as its log group, got {}",
            read.text()
        );

        // 4. Two runs, each with a marker of its own.
        for marker in MARKERS {
            invoked(&router, case, row.function, marker).await;
        }

        // 5. The name still resolves to exactly ONE group — the delivery adopted the group that
        //    was there and created the one that was not, rather than adding a second.
        let resolved = groups(&router, case, &group).await;
        assert_eq!(
            resolved,
            vec![group.clone()],
            "case {case:?}: expected the name {group:?} to resolve to exactly that one group \
             after both runs, got {resolved:?}"
        );

        // 6. ... still carrying the retention its client gave it, and carrying none at all
        //    where the delivery itself had to create it.
        let after = retention_of(&router, case, &group).await;
        assert_eq!(
            after, days,
            "case {case:?}: expected the group {group:?} to report the retention {days:?} after \
             both runs, got {after:?}"
        );

        // 7. Both runs' records are readable in that one group: the line each handler wrote, and
        //    the runtime's own closing frame per run.
        let held = searched(&router, case, &group).await;
        for marker in MARKERS {
            let printed = format!("logged {marker}");
            assert!(
                held.iter().any(|message| message.contains(&printed)),
                "case {case:?}: expected the group {group:?} to hold the record {printed:?}, got \
                 {held:?}"
            );
        }
        let reports = held
            .iter()
            .filter(|message| message.contains(REPORT_PREFIX))
            .count();
        assert_eq!(
            reports,
            MARKERS.len(),
            "case {case:?}: expected one {REPORT_PREFIX:?} record per run in {group:?}, got \
             {held:?}"
        );

        // 8. And nowhere else: a function logging into a group of its own leaves the default
        //    group for its own name uncreated.
        if row.log_group.is_some() {
            let elsewhere = groups(&router, case, &default_group).await;
            assert!(
                elsewhere.is_empty(),
                "case {case:?}: expected no group {default_group:?} for a function logging to \
                 {group:?}, got {elsewhere:?}"
            );
        }
    }
}
