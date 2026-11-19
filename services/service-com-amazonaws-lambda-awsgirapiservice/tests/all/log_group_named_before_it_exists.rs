// SPDX-License-Identifier: CC0-1.0

//! A function NAMES its log destination from the moment it is created, while CloudWatch Logs
//! holds no such group until the function has actually run — and the reads that address the
//! group say so.
//!
//! RULE: a function's `LoggingConfig` is answered in full from the create on, the group it names
//! (`LogGroup`, else the default `/aws/lambda/<function name>`) beside the format its records are
//! rendered in (`LogFormat`, else the applied default plain text), and that group is NOT a
//! resource the create makes: until a run of the function needs it, CloudWatch Logs reports no
//! group of that name and refuses a read of its streams as a missing log group, and the first run
//! brings exactly that one group into being holding exactly one stream — the one its execution
//! environment writes to, named for the UTC date the run started, the version that executed and
//! the run's own id. A group the function does not log to is untouched by the run, and reads of it
//! keep answering as they did before.
//!
//! That pair of answers is the whole observation an operator makes to tell "this function has
//! never run" from "this function ran": the configuration names the destination either way, so the
//! ABSENCE of the group is what carries the information, and a group that never appears after a
//! real run makes the operator read the opposite of what happened.
//!
//! The siblings beside this file pin the other halves of the delivery and take this one as given:
//! [`crate::invocation_log_delivery`] pins WHAT a run puts in the group,
//! [`crate::invocation_log_group_lazy_creation`] the group's own CONFIGURATION and the role grants
//! that decide whether it is created at all, and [`crate::invocation_log_group_search`] that a
//! client can find the delivered records. None of them states what the FUNCTION reports about a
//! destination that does not exist yet, or how the stream listing of the named group reads before
//! the first run.
//!
//! The rows differ along what the rule generalises over: whether the create named a `LoggingConfig`
//! at all, whether the one it named carries a group, a format, or both, whether that group is
//! inside the default `/aws/lambda/` prefix or outside it, and which runtime ran — the creation of
//! the group is the host's, so both engines drive it.
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda, CloudWatch
//! Logs and IAM mounted on ONE engine — the harness [`crate::invocation_log_delivery::emulator`]
//! builds, reused rather than copied — so what is asserted is what a client reads off
//! `GetFunctionConfiguration`, `DescribeLogGroups` and `DescribeLogStreams`.
//!
//! The tests here RUN handlers, so each takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::{iso8601, VERSION_LATEST};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role};
use crate::event_source_mappings::call;
use crate::invocation_log_delivery::{
    create_function_logging, emulator, groups, invocation_request, logs_answer, Created,
    FUNCTION_ERROR_HEADER, LOGS, LOG_WRITES, NODE_RUNTIME, PYTHON_RUNTIME,
};

/// Where the function operations are bound (`GET /2015-03-31/functions/{FunctionName}/configuration`
/// reads one back).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The prefix of the group a function that names none logs to: "Lambda functions send logs to a
/// default log group named `/aws/lambda/<function name>`" (`LoggingConfig$LogGroup`).
const DEFAULT_PREFIX: &str = "/aws/lambda/";

/// The error class CloudWatch Logs refuses a read of a resource it does not hold under.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The whole message a read of a log group the account does not hold is refused with: fixed text,
/// interpolating nothing from the request.
const LOG_GROUP_NOT_FOUND: &str = "The specified log group does not exist.";

/// The header `Invoke` names the run it served in, which an SDK surfaces as the request id — and
/// which the stream of that run's log is named for.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// One row: the `LoggingConfig` the create carries, the group that leaves the function logging to,
/// and the format it therefore reports.
struct Row {
    case: &'static str,
    /// The handler's source. Nothing here reads what it wrote, so it only has to return.
    source: &'static str,
    /// The runtime that runs it.
    runtime: &'static str,
    /// The whole `LoggingConfig` the create names, or `None` for a create that names none.
    logging_config: Option<Value>,
    /// The group the function logs to, or `None` for its default one.
    log_group: Option<&'static str>,
    /// The `LogFormat` every read of the function must report: "The default log format is plain
    /// text" where the create named none.
    log_format: &'static str,
}

/// A node handler that returns without writing anything.
const NODE_SOURCE: &str = "exports.handler = async () => ({ ok: true });\n";

/// A python handler that returns without writing anything.
const PYTHON_SOURCE: &str = "def lambda_handler(event, context):\n    return {'ok': True}\n";

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a create naming no LoggingConfig at all",
            source: NODE_SOURCE,
            runtime: NODE_RUNTIME,
            logging_config: None,
            log_group: None,
            log_format: "Text",
        },
        Row {
            case: "a LoggingConfig naming only a group, under the default prefix",
            source: PYTHON_SOURCE,
            runtime: PYTHON_RUNTIME,
            logging_config: Some(json!({ "LogGroup": "/aws/lambda/named-before-it-exists" })),
            log_group: Some("/aws/lambda/named-before-it-exists"),
            log_format: "Text",
        },
        Row {
            case: "a LoggingConfig naming a group outside that prefix and the JSON format",
            source: NODE_SOURCE,
            runtime: NODE_RUNTIME,
            logging_config: Some(json!({
                "LogGroup": "/aws/vendedlogs/named-before-it-exists",
                "LogFormat": "JSON",
            })),
            log_group: Some("/aws/vendedlogs/named-before-it-exists"),
            log_format: "JSON",
        },
        Row {
            case: "a LoggingConfig naming only the plain-text format",
            source: NODE_SOURCE,
            runtime: NODE_RUNTIME,
            logging_config: Some(json!({ "LogFormat": "Text" })),
            log_group: None,
            log_format: "Text",
        },
    ]
}

/// What `DescribeLogStreams` answered for one group: the refusal where CloudWatch Logs holds no
/// such group, and the streams it reported where it does.
struct Listing {
    status: u16,
    /// The error class it refused with, as the wire names it, or the empty string where it served.
    code: String,
    /// The message it refused with, or the empty string where it served.
    message: String,
    /// The names of the streams it reported.
    streams: Vec<String>,
}

/// `DescribeLogStreams` on `group`, answered or refused.
async fn stream_listing(router: &MultiServiceRouter, case: &str, group: &str) -> Listing {
    let (status, answered) = logs_answer(
        router,
        case,
        "DescribeLogStreams",
        &json!({ "logGroupName": group }),
    )
    .await;
    Listing {
        status,
        code: answered["__type"]
            .as_str()
            .unwrap_or_default()
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string(),
        message: answered["message"].as_str().unwrap_or_default().to_string(),
        streams: answered["logStreams"]
            .as_array()
            .map(|found| {
                found
                    .iter()
                    .filter_map(|stream| stream["logStreamName"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// Assert that CloudWatch Logs holds no group named `group` `when`: the prefix listing reports
/// none of them, and the stream listing is refused as a missing log group.
async fn assert_absent(router: &MultiServiceRouter, case: &str, when: &str, group: &str) {
    let listed = groups(router, case, group).await;
    assert!(
        listed.is_empty(),
        "case {case:?}: expected no group named {group:?} {when}, got {listed:?}"
    );
    let refused = stream_listing(router, case, group).await;
    assert_eq!(
        (
            refused.status,
            refused.code.as_str(),
            refused.message.as_str()
        ),
        (400, NOT_FOUND, LOG_GROUP_NOT_FOUND),
        "case {case:?}: expected DescribeLogStreams on {group:?} to be refused {when}, got {} \
         {:?} {:?}",
        refused.status,
        refused.code,
        refused.message
    );
}

/// The `LoggingConfig` a read of the function reports, as its `(LogGroup, LogFormat)` pair.
fn reported_logging(answered: &Value) -> (Option<String>, Option<String>) {
    let member = |name: &str| {
        answered
            .get("LoggingConfig")
            .and_then(|logging| logging.get(name))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    (member("LogGroup"), member("LogFormat"))
}

/// One synchronous `Invoke` of `name`, and the id of the run it served — which no row may see a
/// function error on.
async fn invoked(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let response = router.dispatch(invocation_request(name, &[])).await;
    let (parts, body) = response.into_parts();
    assert_eq!(
        parts.status.as_u16(),
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        parts.status,
        String::from_utf8_lossy(&body)
    );
    let header = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    let failure = header(FUNCTION_ERROR_HEADER);
    assert_eq!(
        failure, None,
        "case {case:?}: a run that creates its own log group is no function error, got {failure:?}"
    );
    header(REQUEST_ID_HEADER).unwrap_or_else(|| {
        panic!("case {case:?}: expected the answer to name its run in {REQUEST_ID_HEADER}")
    })
}

/// The `YYYY/MM/DD` of `at` in UTC, as a Lambda log stream's name opens on it — taken from the
/// crate's own rendering of an instant rather than from calendar arithmetic here.
fn utc_date(at: SystemTime) -> String {
    iso8601(at)
        .split('T')
        .next()
        .unwrap_or_default()
        .replace('-', "/")
}

/// The names the one stream of a run may carry: the UTC date the run started, the version that
/// executed and the run's own id — with both dates the run could have started on, so a run made
/// across midnight UTC is one of them rather than a failure.
fn stream_names(dates: &[String], request_id: &str) -> Vec<String> {
    dates
        .iter()
        .map(|date| format!("{date}/[{VERSION_LATEST}]{}", request_id.replace('-', "")))
        .collect()
}

/// A function names its log group from the create on, CloudWatch Logs holds no such group until the
/// function runs, and the first run creates exactly that group with exactly one stream of that run.
#[tokio::test]
async fn the_named_log_group_is_absent_until_the_first_run_creates_it() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let role = given_role(
            &router,
            case,
            &format!("named-group-writer-{index}"),
            Some(json!([allow(LOGS, &LOG_WRITES, "*")])),
        )
        .await;
        let name = format!("named-before-it-exists-{index}");
        let default_group = format!("{DEFAULT_PREFIX}{name}");
        let group = row.log_group.unwrap_or(&default_group).to_string();
        create_function_logging(
            &router,
            &Created {
                case,
                name: &name,
                source: row.source,
                runtime: row.runtime,
                role: &role,
                log_group: row.log_group,
            },
            row.logging_config.as_ref(),
        )
        .await;

        // The function names its destination from the create on, with the format its records are
        // rendered in — the applied default where the create named none.
        let read = call(
            &router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
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
        let reported = reported_logging(&read.json());
        assert_eq!(
            reported,
            (Some(group.clone()), Some(row.log_format.to_string())),
            "case {case:?}: expected the function to name {group:?} and {:?}, got {reported:?} \
             in {}",
            row.log_format,
            read.text()
        );

        // And CloudWatch Logs holds no such group: nothing has run.
        assert_absent(&router, case, "before the first run", &group).await;

        let opened = utc_date(SystemTime::now());
        let request_id = invoked(&router, case, &name).await;
        let closed = utc_date(SystemTime::now());

        // The run brought exactly that one group into being.
        let created = groups(&router, case, &group).await;
        assert_eq!(
            created,
            vec![group.clone()],
            "case {case:?}: expected the first run to create exactly {group:?}, got {created:?}"
        );

        // Holding exactly one stream: the one the environment that served the run writes to, named
        // for the date it started, the unpublished version it ran and the run's own id.
        let listed = stream_listing(&router, case, &group).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected DescribeLogStreams on {group:?} to be served once the run \
             created it, got {} {:?}",
            listed.status, listed.message
        );
        let dates = if opened == closed {
            vec![opened.clone()]
        } else {
            vec![opened.clone(), closed.clone()]
        };
        let wanted = stream_names(&dates, &request_id);
        assert_eq!(
            listed.streams.len(),
            1,
            "case {case:?}: expected the one run to leave one stream in {group:?}, got {:?}",
            listed.streams
        );
        assert!(
            wanted.contains(&listed.streams[0]),
            "case {case:?}: expected the stream of run {request_id:?} to be one of {wanted:?}, \
             got {:?}",
            listed.streams
        );

        // A group the function does not log to is untouched by the run, so the same two reads of
        // it answer as they did before: that is what makes the absence of the function's OWN group
        // the statement that the function has not run.
        if row.log_group.is_some() {
            assert_absent(
                &router,
                case,
                "after a run that logs elsewhere",
                &default_group,
            )
            .await;
        }
    }
}
