// SPDX-License-Identifier: CC0-1.0

//! The LAZY creation of a function's log destination, and the DEFAULTS the group it creates
//! carries.
//!
//! RULE: a function's log group is not a resource its create makes — it does not exist until a
//! run of the function needs it, and the first run that its execution role allows to create it
//! creates it with CloudWatch Logs' own defaults for a group nothing configured (the `STANDARD`
//! class, no retention and no customer key), opens one stream of it named for the date the run
//! started, the version that executed and the run's own id, and puts that run's whole log —
//! the platform framing around whatever the handler wrote — into it; a role short of
//! `logs:CreateLogGroup` leaves the group absent without failing the invocation, and once the
//! group exists the same role's stream and put grants land its lines in it.
//!
//! This is the half of the delivery its siblings take as given: [`crate::invocation_log_delivery`]
//! pins WHAT reaches the group and [`crate::invocation_log_group_search`] pins that a client can
//! FIND it, and neither states the CONFIGURATION of the group the delivery brought into being or
//! what becomes of a run whose role may write a stream but not create the group it belongs to.
//! Both are what a client depends on: one reads the group's defaults off `DescribeLogGroups` to
//! learn that nothing has configured it, and one tells "absent because the role may not create
//! it" apart from "absent because nothing ran".
//!
//! The rows differ along what the rule generalises over: which group the function logs to (the
//! default `/aws/lambda/<function name>`, and one its `LoggingConfig` names, in and out of that
//! prefix), which runtime ran (the delivery is the host's, so both engines are driven), how many
//! lines the handler wrote, and which of the three log writes the execution role carries.
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda, CloudWatch
//! Logs and IAM mounted on ONE engine — the same harness
//! [`crate::invocation_log_delivery::emulator`] builds, reused rather than copied — so what is
//! asserted is what a client reads off `DescribeLogGroups`, `DescribeLogStreams`, `GetLogEvents`
//! and `FilterLogEvents`.
//!
//! Every test here RUNS handlers, so each takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role};
use crate::invocation_log_delivery::{
    create_function, delivered, emulator, events, groups, invocation_request, logs, streams,
    Created, NODE_RUNTIME, PYTHON_RUNTIME, LOGS, LOG_WRITES,
};

/// The two writes a run makes INTO a group that already exists: [`LOG_WRITES`] without the one
/// that brings the group into being, which is the grant the negative rows withhold.
const STREAM_WRITES: [&str; 2] = ["CreateLogStream", "PutLogEvents"];

/// The class CloudWatch Logs gives a group nothing asked a class for. Source:
/// `smithy_query(query="doc",shape="CreateLogGroupRequest",member="logGroupClass")` — "If you
/// omit this parameter, the default of STANDARD is used".
const DEFAULT_LOG_GROUP_CLASS: &str = "STANDARD";

/// The version segment a stream of the unpublished function carries, as
/// `$LATEST` is written into a Lambda log stream's name.
const UNPUBLISHED_SEGMENT: &str = "[$LATEST]";

/// How many hex characters of the run's own id close a stream's name: a request id is a UUID, so
/// it is 32 of them once its hyphens are dropped.
const REQUEST_ID_HEX: usize = 32;

/// The header `Invoke` reports a handler's own failure on, which no row here may see: a log write
/// the role withholds is not an invocation failure.
const FUNCTION_ERROR_HEADER: &str = "x-amz-function-error";

/// One row of the lazy-creation table.
struct Row {
    case: &'static str,
    /// The handler's source. It writes [`Row::written`] and returns the destination the runtime
    /// told it about.
    source: &'static str,
    /// The runtime that runs it.
    runtime: &'static str,
    /// The lines the handler itself wrote, each of which the delivery must leave in the group.
    written: &'static [&'static str],
    /// The group the function's `LoggingConfig` names, or `None` for the default one.
    log_group: Option<&'static str>,
}

/// A python handler that writes `lines` and answers the destination its context names.
macro_rules! python_handler {
    ($($line:literal),+ $(,)?) => {
        concat!(
            "def lambda_handler(event, context):\n",
            $("    print('", $line, "')\n",)+
            "    return {'group': context.log_group_name, 'stream': context.log_stream_name}\n",
        )
    };
}

/// A node handler that writes `lines` and answers the destination its context names.
macro_rules! node_handler {
    ($($line:literal),+ $(,)?) => {
        concat!(
            "exports.handler = async (event, context) => {\n",
            $("  console.log('", $line, "');\n",)+
            "  return { group: context.logGroupName, stream: context.logStreamName };\n",
            "};\n",
        )
    };
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a python handler writing one line into its default group",
            source: python_handler!("lazy-group-one"),
            runtime: PYTHON_RUNTIME,
            written: &["lazy-group-one"],
            log_group: None,
        },
        Row {
            case: "a node handler writing three lines into its default group",
            source: node_handler!("lazy-group-many-0", "lazy-group-many-1", "lazy-group-many-2"),
            runtime: NODE_RUNTIME,
            written: &[
                "lazy-group-many-0",
                "lazy-group-many-1",
                "lazy-group-many-2",
            ],
            log_group: None,
        },
        Row {
            case: "a function whose configuration names a group under the same prefix",
            source: python_handler!("lazy-group-named"),
            runtime: PYTHON_RUNTIME,
            written: &["lazy-group-named"],
            log_group: Some("/aws/lambda/lazy-named-elsewhere"),
        },
        Row {
            case: "a function whose configuration names a group outside that prefix",
            source: node_handler!("lazy-group-vended"),
            runtime: NODE_RUNTIME,
            written: &["lazy-group-vended"],
            log_group: Some("/aws/vendedlogs/lazy-creation-suite"),
        },
    ]
}

/// `Invoke` the function `name` synchronously and answer what the handler returned, together with
/// the `FunctionError` signal the answer carried — which every row asserts the absence of.
async fn invoked(router: &MultiServiceRouter, case: &str, name: &str) -> (Option<String>, Value) {
    let response = router.dispatch(invocation_request(name, &[])).await;
    let (parts, body) = response.into_parts();
    assert_eq!(
        parts.status.as_u16(),
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        parts.status,
        String::from_utf8_lossy(&body)
    );
    let failure = parts
        .headers
        .get(FUNCTION_ERROR_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let payload: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (failure, payload)
}

/// The whole record `DescribeLogGroups` reports for the one group named `group`, or `None` when
/// the prefix lookup finds no such group — the read a client uses to learn whether the group
/// exists at all and, when it does, how it is configured.
async fn described(router: &MultiServiceRouter, case: &str, group: &str) -> Option<Value> {
    let answered = logs(
        router,
        case,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": group }),
    )
    .await;
    answered["logGroups"]
        .as_array()
        .and_then(|found| {
            found
                .iter()
                .find(|reported| reported["logGroupName"] == json!(group))
                .cloned()
        })
}

/// The `(logStreamName, message)` pairs `FilterLogEvents` reports for `group` under the term
/// `pattern` — the search a client that has just invoked a function polls for its output.
async fn searched(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    pattern: &str,
) -> Vec<(String, String)> {
    let answered = logs(
        router,
        case,
        "FilterLogEvents",
        &json!({ "logGroupName": group, "filterPattern": pattern }),
    )
    .await;
    answered["events"]
        .as_array()
        .map(|found| {
            found
                .iter()
                .map(|event| {
                    (
                        event["logStreamName"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        event["message"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `stream` is named the way a Lambda log stream is: `YYYY/MM/DD/[$LATEST]` followed by
/// the run's own id as 32 lower-case hex characters.
///
/// Checked segment by segment rather than against a pattern, so the message below can say which
/// part of the shape the name missed.
fn stream_shape(stream: &str) -> Result<String, String> {
    let segments: Vec<&str> = stream.split('/').collect();
    let [year, month, day, tail] = segments.as_slice() else {
        return Err(format!(
            "expected four '/'-separated segments, got {} in {stream:?}",
            segments.len()
        ));
    };
    for (what, segment, width) in [("year", year, 4), ("month", month, 2), ("day", day, 2)] {
        if segment.len() != width || !segment.chars().all(|digit| digit.is_ascii_digit()) {
            return Err(format!(
                "expected the {what} to be {width} digits, got {segment:?} in {stream:?}"
            ));
        }
    }
    let Some(id) = tail.strip_prefix(UNPUBLISHED_SEGMENT) else {
        return Err(format!(
            "expected the last segment to open on {UNPUBLISHED_SEGMENT:?}, got {tail:?} in \
             {stream:?}"
        ));
    };
    if id.len() != REQUEST_ID_HEX || !id.chars().all(|hex| matches!(hex, '0'..='9' | 'a'..='f')) {
        return Err(format!(
            "expected the run's id as {REQUEST_ID_HEX} lower-case hex characters, got {id:?} in \
             {stream:?}"
        ));
    }
    Ok(id.to_string())
}

/// The request id the framing of a run's log names, as the `START` and `REPORT` lines must agree
/// on it.
fn framed_request_id(case: &str, lines: &[String]) -> String {
    let opening = lines
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected a log with a START line, got {lines:?}"));
    let id = opening
        .strip_prefix("START RequestId: ")
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the log to open on its START line, got {opening:?}")
        })
        .split(' ')
        .next()
        .unwrap_or_default()
        .to_string();
    let closing = lines.last().cloned().unwrap_or_default();
    assert!(
        closing.starts_with(&format!("REPORT RequestId: {id}")),
        "case {case:?}: expected the log to close on the REPORT of {id:?}, got {closing:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line == &format!("END RequestId: {id}")),
        "case {case:?}: expected an END line for {id:?}, got {lines:?}"
    );
    id
}

/// The first run of a function brings its log group into being, with CloudWatch Logs' own
/// defaults, one stream named for the run, and the run's whole log in it.
#[tokio::test]
async fn the_first_run_creates_the_functions_log_group_with_the_services_defaults() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let role = given_role(
            &router,
            case,
            &format!("lazy-log-writer-{index}"),
            Some(json!([allow(LOGS, &LOG_WRITES, "*")])),
        )
        .await;
        let name = format!("lazy-log-group-{index}");
        let default_group = format!("/aws/lambda/{name}");
        let group = row.log_group.unwrap_or(&default_group).to_string();
        create_function(
            &router,
            &Created {
                case,
                name: &name,
                source: row.source,
                runtime: row.runtime,
                role: &role,
                log_group: row.log_group,
            },
        )
        .await;

        // The create made no group: the function is Active and its destination does not exist.
        let before = described(&router, case, &group).await;
        assert!(
            before.is_none(),
            "case {case:?}: expected {group:?} not to exist before the first run, got {before:?}"
        );

        let (failure, payload) = invoked(&router, case, &name).await;
        assert_eq!(
            failure, None,
            "case {case:?}: a run that creates its own log group is no function error, got \
             {failure:?}"
        );

        // The group the run brought into being, as a client reads it: nothing configured it, so
        // it carries the service's own class and neither a retention nor a customer key.
        let reported = described(&router, case, &group)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected the run to create {group:?}"));
        assert_eq!(
            reported["logGroupClass"],
            json!(DEFAULT_LOG_GROUP_CLASS),
            "case {case:?}: expected the default class for {group:?}, got {reported}"
        );
        for absent in ["retentionInDays", "kmsKeyId"] {
            assert!(
                reported[absent].is_null(),
                "case {case:?}: expected {absent} to be absent from a group nothing configured, \
                 got {reported}"
            );
        }
        assert!(
            reported["arn"].as_str().is_some_and(|arn| arn.contains(&group)),
            "case {case:?}: expected the group's own ARN to name it, got {reported}"
        );
        assert!(
            reported["creationTime"].as_i64().unwrap_or_default() > 0,
            "case {case:?}: expected the group to report when it was created, got {reported}"
        );
        // And only that group: a lazily created destination is one group, not a tree of them.
        let under = groups(&router, case, &group).await;
        assert_eq!(
            under,
            vec![group.clone()],
            "case {case:?}: expected the run to create exactly {group:?}, got {under:?}"
        );
        // The function's default group stays uncreated when it logs elsewhere.
        if row.log_group.is_some() {
            let untouched = groups(&router, case, &default_group).await;
            assert!(
                untouched.is_empty(),
                "case {case:?}: expected {default_group:?} to stay uncreated, got {untouched:?}"
            );
        }

        // One run is one stream, named for the date, the version and the run itself.
        let held = streams(&router, case, &group).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected one stream for the one run, got {held:?}"
        );
        let stream = held[0].clone();
        let hex = stream_shape(&stream)
            .unwrap_or_else(|wrong| panic!("case {case:?}: {wrong}"));

        // The destination the RUNTIME told the handler about is the one the delivery used, which
        // is what makes a handler's own report of its log stream usable by its caller.
        assert_eq!(
            (&payload["group"], &payload["stream"]),
            (&json!(group.clone()), &json!(stream.clone())),
            "case {case:?}: expected the context to name {group:?} and {stream:?}, got {payload}"
        );

        // The run's whole log is in that stream: the framing, and the handler's own lines.
        let (lines, _) = delivered(&router, case, &group, &stream).await;
        let request_id = framed_request_id(case, &lines);
        assert_eq!(
            request_id.replace('-', ""),
            hex,
            "case {case:?}: expected the stream to be named for the run {request_id:?}, got \
             {stream:?}"
        );
        for written in row.written {
            assert!(
                lines.iter().any(|line| line.contains(written)),
                "case {case:?}: expected the handler's line {written:?} in {stream:?}, got \
                 {lines:?}"
            );
        }
        // Every event carries an instant of the run, in order.
        let stamps: Vec<i64> = events(&router, case, &group, &stream)
            .await
            .iter()
            .map(|(at, _)| *at)
            .collect();
        assert!(
            stamps.iter().all(|at| *at > 0) && stamps.windows(2).all(|pair| pair[0] <= pair[1]),
            "case {case:?}: expected ordered instants of the run, got {stamps:?}"
        );

        // And it is an ordinary event of an ordinary group, so the group-wide search finds it
        // under a term of its own text — the read a client polls after an invoke.
        let first = row.written.first().copied().unwrap_or_default();
        let found = searched(&router, case, &group, &format!("\"{first}\"")).await;
        assert!(
            found
                .iter()
                .any(|(held, message)| held == &stream && message.contains(first)),
            "case {case:?}: expected a search of {group:?} for {first:?} to find it in \
             {stream:?}, got {found:?}"
        );
    }
}

/// One row of the create-group table: which of the three log writes the execution role carries,
/// and whether that leaves the function's group created by its own run.
struct Grant {
    case: &'static str,
    /// The log actions the role's one inline policy allows on the function's own group.
    actions: &'static [&'static str],
    /// Whether the run has to have created the group.
    creates: bool,
}

fn grant_rows() -> Vec<Grant> {
    vec![
        Grant {
            case: "a role carrying the group create beside the stream writes",
            actions: &LOG_WRITES,
            creates: true,
        },
        Grant {
            case: "a role carrying the stream writes but not the group create",
            actions: &STREAM_WRITES,
            creates: false,
        },
    ]
}

/// A role short of `logs:CreateLogGroup` leaves the group absent — without failing the
/// invocation — and once the group exists the same role's stream and put grants land its lines in
/// it, which is what tells a least-privilege role's absent group apart from one nothing ran for.
#[tokio::test]
async fn a_role_short_of_the_group_create_leaves_it_absent_until_the_group_exists() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in grant_rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let name = format!("lazy-log-grant-{index}");
        let group = format!("/aws/lambda/{name}");
        // Scoped to the function's own group exactly as AWS's own least-privilege example for a
        // function's logs is: the group, with its streams under it.
        let scope = format!("arn:aws:logs:us-east-1:000000000000:log-group:{group}:*");
        let role = given_role(
            &router,
            case,
            &format!("lazy-log-grantee-{index}"),
            Some(json!([allow(LOGS, row.actions, &scope)])),
        )
        .await;
        create_function(
            &router,
            &Created {
                case,
                name: &name,
                source: python_handler!("lazy-grant-first"),
                runtime: PYTHON_RUNTIME,
                role: &role,
                log_group: None,
            },
        )
        .await;

        // The first run: the group is there only if the role could create it, and either way the
        // invocation itself is answered without a function error.
        let (failure, payload) = invoked(&router, case, &name).await;
        assert_eq!(
            failure, None,
            "case {case:?}: a withheld log write is no invocation failure, got {failure:?}"
        );
        assert_eq!(
            payload["group"],
            json!(group.clone()),
            "case {case:?}: expected the run to be told it logs to {group:?}, got {payload}"
        );
        let after_first = groups(&router, case, &group).await;
        assert_eq!(
            !after_first.is_empty(),
            row.creates,
            "case {case:?}: expected the first run to have created {group:?}: {}, got \
             {after_first:?}",
            row.creates
        );

        // Bring the group into being out of band for the role that could not, so both rows go on
        // from the same state: the group exists and nothing has written to it but the runs the
        // role was allowed to make.
        if !row.creates {
            logs(
                &router,
                case,
                "CreateLogGroup",
                &json!({ "logGroupName": group }),
            )
            .await;
            let created = described(&router, case, &group).await;
            assert!(
                created.is_some(),
                "case {case:?}: expected the out-of-band create to make {group:?}"
            );
        }

        // A second run, now that the group exists: the same role's stream and put grants land its
        // lines in it, whichever row we are on.
        let (failure, payload) = invoked(&router, case, &name).await;
        assert_eq!(
            failure, None,
            "case {case:?}: the second run is answered too, got {failure:?}"
        );
        let stream = payload["stream"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the run to report its stream, got {payload}")
            })
            .to_string();
        stream_shape(&stream).unwrap_or_else(|wrong| panic!("case {case:?}: {wrong}"));
        let (lines, _) = delivered(&router, case, &group, &stream).await;
        framed_request_id(case, &lines);
        assert!(
            lines.iter().any(|line| line.contains("lazy-grant-first")),
            "case {case:?}: expected the run's own line in {stream:?} once {group:?} exists, got \
             {lines:?}"
        );
        // How many runs are in the group states the rule exactly: the row that could not create
        // it lost its first run's log, and the row that could kept both.
        let held = streams(&router, case, &group).await;
        let expected = if row.creates { 2 } else { 1 };
        assert_eq!(
            held.len(),
            expected,
            "case {case:?}: expected {expected} run(s) to have left a stream in {group:?}, got \
             {held:?}"
        );
    }
}
