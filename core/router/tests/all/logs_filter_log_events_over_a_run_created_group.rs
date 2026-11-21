// SPDX-License-Identifier: CC0-1.0

//! Searching the log of a function's runs with `FilterLogEvents`: a read of a log group NOBODY
//! created through CloudWatch Logs' own API.
//!
//! RULE under test: **`FilterLogEvents` answers HTTP 200 over the events of any log group Amazon
//! CloudWatch Logs HOLDS — a group created implicitly by the delivery of an AWS Lambda
//! invocation's log included, not only one a client created itself — reporting `events` (an
//! EMPTY list where the request's filter pattern, time range, stream selector or page bound
//! admits none of the group's events, never a refusal), the `searchedLogStreams` list and the
//! `searchStatistics` counts beside it, and it reserves `ResourceNotFoundException` for a group
//! the account does not hold at all.**
//!
//! `FilterLogEvents` "lists log events from the specified log group" and models exactly two
//! client-side failures — `InvalidParameterException` for a malformed request and
//! `ResourceNotFoundException` (HTTP 400) for a group that does not exist
//! (`smithy_query(query="operation",operation="FilterLogEvents")`) — so how a group came to
//! exist cannot decide whether it can be searched: "By default, Lambda functions send logs to a
//! default log group named `/aws/lambda/<function name>`" (`LoggingConfig$LogGroup`), and that
//! group is the one a client reads a run back out of. The two reporting members are read as the
//! model documents them: `searchedLogStreams` is withdrawn — "As of May 15, 2020, this parameter
//! is no longer supported. This parameter returns an empty list." — and the `searchStatistics`
//! counts summarise that same list, so a search reporting no searched stream reports both counts
//! as zero.
//!
//! A read that answers 400 for a group that exists is indistinguishable, to an SDK, from "you
//! asked for a group that does not exist": log-based verification of a run becomes impossible,
//! a client that retries on 400 loops, and one that treats 400 as fatal aborts a run whose
//! function actually executed. So the SAME name is read here BEFORE anything ran — where the
//! refusal is the correct answer — and again after, where it is not.
//!
//! Three services have to agree for the rule to hold — AWS Lambda runs the function and delivers
//! its log, IAM answers for the execution role that delivery is made under, and CloudWatch Logs
//! holds the group it created on the function's behalf and searches it, with CloudFormation
//! provisioning the role and the function — so the suite lives here rather than in any one
//! crate. What the neighbours pin and this one does not restate:
//! [`crate::cloudformation_lambda_function_log_destination`] drives WHERE a deployed function's
//! records land (and owns the assembled-emulator wire harness this suite reuses) and
//! [`crate::lambda_log_group_outlives_delete_function`] what the delete of a function leaves
//! behind. What only this suite reaches is the SEARCH of such a group: the 400-versus-200
//! decision and the members a successful page carries.
//!
//! The rows differ along what the rule generalises over rather than restating one request: which
//! selector names the group (its name, its ARN, its name as an identifier), what narrows the
//! search (a filter pattern that selects part of the group, one that selects nothing, the empty
//! pattern that selects everything, a time range on either side of the records and one spanning
//! them, a stream list and a stream prefix, a page bound) and which direction it runs in. Every
//! bound is taken from the events the group itself reports, so no row is written against a
//! recorded value.
//!
//! No expected value here comes from a recording: every marker, function name and group name is
//! built by this suite, and every assertion is against what its own runs wrote. Nothing waits a
//! span of its own choosing — each wait looks again for the state it asserts — and nothing
//! asserts on elapsed time.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
// The ONE assembled emulator, wire harness and function-with-a-log fixture this test crate owns,
// rather than second copies of them.
use crate::cloudformation_lambda_function_log_destination::{
    cloudformation, emulator, groups_named, invoke, logs, logs_policy, messages_until_condition,
    trust_policy, wait_until_active, Answered, CREATE_COMPLETE, DEFAULT_GROUP_PREFIX,
    FUNCTION_TIMEOUT, HANDLER_SOURCE, REPORT_PREFIX,
};
use crate::query_wire::outputs_of;

/// The logical ids the template below declares.
const ROLE_ID: &str = "ExecutionRole";
const FUNCTION_ID: &str = "Logger";

/// The stack and the function it deploys. The function declares no `LoggingConfig`, so the group
/// searched below is the default one for its name and nothing but the first run's own delivery
/// ever creates it.
const STACK: &str = "logs-filter-over-a-run-created-group";
const FUNCTION: &str = "filter-log-events-run-created";

/// The managed runtime the function runs, and how many times it is invoked: more than one run, so
/// a search narrowed to one run's line is observably narrower than the whole group.
const RUNTIME: &str = "python3.12";
const RUNS: usize = 2;

/// What the shared handler prints for the marker its event carries ([`HANDLER_SOURCE`]), which is
/// the line of a run a client looks for in the group.
const PRINTED_PREFIX: &str = "logged ";

/// A well-formed log group name — `LogGroupName` is `@length(1..512)` with pattern
/// `^[\.\-_/#A-Za-z0-9]+$` — that nothing in this suite ever creates, which is what a genuine
/// `ResourceNotFoundException` is owed to.
const ABSENT_GROUP: &str = "/aws/lambda/filter-log-events-no-such-function";

/// The refusal a group the account does not hold is owed: `ResourceNotFoundException`, whose
/// modelled status is HTTP 400
/// (`smithy_query(query="operation",operation="FilterLogEvents")`), carrying this service's
/// sentence for a missing resource of that kind.
const NOT_FOUND_SHAPE: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 400;
const NOT_FOUND_MESSAGE: &str = "The specified log group does not exist.";

/// A term no record of this group carries, spelled as the syntax's `"…"` string so the hyphens
/// in it are text rather than the syntax's own exclusion prefix.
const ABSENT_TERM: &str = "a-term-no-record-of-this-group-carries";

/// A log stream name prefix no stream of the group carries. `logStreamNamePrefix` FILTERS the
/// results ("only events from log streams that have names starting with this prefix"), so one no
/// stream carries selects nothing rather than refusing the search.
const ABSENT_STREAM_PREFIX: &str = "no-stream-of-this-group/";

/// Every member of `FilteredLogEvent` a matched event carries
/// (`smithy_query(query="shape",shape="FilteredLogEvent")`).
const EVENT_MEMBERS: &[&str] = &[
    "logStreamName",
    "timestamp",
    "message",
    "ingestionTime",
    "eventId",
];

/// What the search a row makes must come to.
#[derive(Debug, Clone)]
enum Expect {
    /// Every record both runs left in the group: the narrowing excludes nothing it holds.
    EveryRecord,
    /// At least one event, and every event the page carries holds this text.
    OnlyCarrying(String),
    /// No event at all — and still a successful page, because the group exists.
    NoEvent,
    /// At most this many events, and a continuation token, because a page that carried events
    /// says nothing about what lies past it.
    AtMost(usize),
    /// At least one event, every one of them a record the group holds: the narrowing selects
    /// some of the group without naming which records in advance.
    SomeRecord,
}

/// One row: how the request names and narrows the search, and what it must come to.
#[derive(Debug, Clone)]
struct Row {
    case: String,
    /// The whole request body, group selector included.
    request: Value,
    expect: Expect,
}

/// The `logGroupArn` CloudWatch Logs reports for `group`, which is the other way a request can
/// name it (`logGroupIdentifier` takes "the ARN of the log group"). The group is resolved through
/// the harness's own reader, so this states no second describe-and-filter walk.
async fn group_arn(router: &MultiServiceRouter, case: &str, group: &str) -> String {
    let described = groups_named(router, case, group).await;
    described
        .first()
        .and_then(|group| group.get("logGroupArn"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            panic!("case {case:?}: DescribeLogGroups reported no ARN for {group:?}: {described:?}")
        })
}

/// Assert that `answered` is the refusal a group the account does not hold is owed, and nothing
/// else: the modelled status, the modelled shape and the service's sentence for the class.
fn assert_group_absent(case: &str, group: &str, answered: &Answered) {
    assert_eq!(
        answered.status, NOT_FOUND_STATUS,
        "case {case:?}: expected HTTP {NOT_FOUND_STATUS} from FilterLogEvents on {group:?}, which \
         nothing created, got {} with {:?}",
        answered.status,
        answered.text()
    );
    let document = answered.json();
    let shape = document
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        shape.ends_with(NOT_FOUND_SHAPE),
        "case {case:?}: expected FilterLogEvents on {group:?} to be refused {NOT_FOUND_SHAPE}, got \
         {:?}",
        answered.text()
    );
    let message = document
        .get("message")
        .or_else(|| document.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        message, NOT_FOUND_MESSAGE,
        "case {case:?}: expected the refusal of {group:?} to read {NOT_FOUND_MESSAGE:?}, got \
         {message:?}"
    );
}

/// One successful search: assert the page itself — the status, the members every answer carries
/// and the shape of every event on it — and answer the events it reported beside the `nextToken`
/// it offered, if any. Nothing is asserted about WHETHER a page carries a token: "a returned page
/// might only be partially full, or even empty [...] partially full or empty pages don't
/// necessarily mean that pagination is finished", so the token is reported here and the walk
/// that follows it is what pins the termination rule.
async fn searched(
    router: &MultiServiceRouter,
    case: &str,
    request: &Value,
) -> (Vec<Value>, Option<String>) {
    let answered = logs(router, "FilterLogEvents", request.clone()).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from FilterLogEvents({request}), got {} with {:?}",
        answered.status,
        answered.text()
    );
    let document = answered.json();

    // The withdrawn per-stream reporting, and the two counts that summarise it.
    let searched_log_streams = document
        .get("searchedLogStreams")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a searchedLogStreams list in {document}")
        });
    assert!(
        searched_log_streams.is_empty(),
        "case {case:?}: expected searchedLogStreams to be the empty list the member documents, \
         got {searched_log_streams:?}"
    );
    let statistics = document.get("searchStatistics").cloned().unwrap_or_else(|| {
        panic!("case {case:?}: expected a searchStatistics block in {document}")
    });
    // Both counts are counts of the searched-stream reporting above, which the member documents
    // as the empty list, so a search reports neither stream nor count of one.
    for count in ["approximateTotalLogStreamCount", "completedLogStreamCount"] {
        let reported = statistics.get(count).and_then(Value::as_i64);
        assert_eq!(
            reported,
            Some(0),
            "case {case:?}: expected {count} to be 0 beside the empty searchedLogStreams, got \
             {reported:?}"
        );
    }

    // The events, each a `FilteredLogEvent` with every member of that shape.
    let events = document
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an events list in {document}"));
    for event in &events {
        for member in EVENT_MEMBERS {
            assert!(
                event.get(member).is_some_and(|value| !value.is_null()),
                "case {case:?}: expected every matched event to carry {member:?}, got {event}"
            );
        }
    }

    let token = document
        .get("nextToken")
        .and_then(Value::as_str)
        .map(str::to_string);
    (events, token)
}

/// Every message `group` holds, waited for until the runtime's closing frame has arrived for each
/// of the `runs` runs: the condition that says the whole of every run's log is in, which is what
/// makes the pages read afterwards stable. Driven through the harness's own poll-until-the-
/// condition reader, so nothing here sleeps a span of its own.
async fn every_run_delivered(router: &MultiServiceRouter, group: &str, runs: usize) -> Vec<String> {
    messages_until_condition(router, group, |held| {
        held.iter()
            .filter(|message| message.starts_with(REPORT_PREFIX))
            .count()
            >= runs
    })
    .await
}

/// The `message` of each event on a page, in the order the page reported them.
fn messages_of(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| event.get("message").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// The `timestamp` of each event on a page.
fn timestamps_of(events: &[Value]) -> Vec<i64> {
    events
        .iter()
        .filter_map(|event| event.get("timestamp").and_then(Value::as_i64))
        .collect()
}

/// The template this suite deploys: the execution role with the basic-execution logs grant, and
/// the function pointed at no log group of its own, so its first run's delivery creates one.
fn template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE_ID: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [{
                        "PolicyName": "basic-execution",
                        "PolicyDocument": logs_policy()
                    }]
                }
            },
            FUNCTION_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Role": { "Fn::GetAtt": [ROLE_ID, "Arn"] },
                    "Runtime": RUNTIME,
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": HANDLER_SOURCE }
                }
            }
        },
        "Outputs": {
            "FunctionName": { "Value": { "Ref": FUNCTION_ID } }
        }
    })
    .to_string()
}

/// The rows, built from what the group itself reports: the selectors that name it, the
/// narrowings taken from its own events' instants and streams, and the page bound.
fn rows(group: &str, arn: &str, span: (i64, i64), streams: &[String], printed: &str) -> Vec<Row> {
    let (oldest, newest) = span;
    let by_name = json!({ "logGroupName": group });
    let mut rows = vec![
        Row {
            case: "the group's name, with nothing narrowing the search".to_string(),
            request: by_name.clone(),
            expect: Expect::EveryRecord,
        },
        Row {
            case: "the empty filter pattern, which matches every event".to_string(),
            request: json!({ "logGroupName": group, "filterPattern": "" }),
            expect: Expect::EveryRecord,
        },
        Row {
            case: "a filter pattern naming the line one run printed".to_string(),
            request: json!({ "logGroupName": group, "filterPattern": format!("\"{printed}\"") }),
            expect: Expect::OnlyCarrying(printed.to_string()),
        },
        Row {
            case: "a filter pattern naming a term no record of the group carries".to_string(),
            request: json!({
                "logGroupName": group,
                "filterPattern": format!("\"{ABSENT_TERM}\""),
            }),
            expect: Expect::NoEvent,
        },
        Row {
            case: "a time range spanning every event the group holds".to_string(),
            request: json!({ "logGroupName": group, "startTime": oldest, "endTime": newest }),
            expect: Expect::EveryRecord,
        },
        Row {
            case: "a time range that ends before the oldest event".to_string(),
            request: json!({ "logGroupName": group, "endTime": oldest - 1 }),
            expect: Expect::NoEvent,
        },
        Row {
            case: "a time range that starts after the newest event".to_string(),
            request: json!({ "logGroupName": group, "startTime": newest + 1 }),
            expect: Expect::NoEvent,
        },
        Row {
            case: "every log stream the group's events are in, named one by one".to_string(),
            request: json!({ "logGroupName": group, "logStreamNames": streams }),
            expect: Expect::EveryRecord,
        },
        Row {
            case: "a log stream name prefix no stream of the group carries".to_string(),
            request: json!({
                "logGroupName": group,
                "logStreamNamePrefix": ABSENT_STREAM_PREFIX,
            }),
            expect: Expect::NoEvent,
        },
        Row {
            case: "a page bound of one event".to_string(),
            request: json!({ "logGroupName": group, "limit": 1 }),
            expect: Expect::AtMost(1),
        },
        Row {
            case: "the search run newest-first rather than oldest-first".to_string(),
            request: json!({ "logGroupName": group, "startFromHead": false }),
            expect: Expect::EveryRecord,
        },
        Row {
            case: "the group named by its ARN as a log group identifier".to_string(),
            request: json!({ "logGroupIdentifier": arn }),
            expect: Expect::EveryRecord,
        },
        Row {
            case: "the group named by its own name as a log group identifier".to_string(),
            request: json!({ "logGroupIdentifier": group }),
            expect: Expect::EveryRecord,
        },
    ];
    // One stream of the group, named as the prefix it starts with: a prefix a stream DOES carry
    // selects that stream rather than nothing.
    if let Some(stream) = streams.first() {
        rows.push(Row {
            case: format!("a log stream name prefix one stream of the group carries ({stream:?})"),
            request: json!({ "logGroupName": group, "logStreamNamePrefix": stream }),
            expect: Expect::SomeRecord,
        });
    }
    rows
}

#[tokio::test]
async fn filter_log_events_searches_the_log_group_a_runs_own_delivery_created() {
    let router = emulator();
    let case = "a python function logging to the default group its own delivery created";

    // 1. The stack: the execution role with the three logs calls a delivery makes, and the
    //    function that writes the records. Nothing here declares a log group.
    let template = template();
    let created = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            // The template declares an IAM role, which is acknowledged with CAPABILITY_IAM.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
    let (reached, described) = wait_for_stack_status(&router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );
    let function = outputs_of(&described)
        .get("FunctionName")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack published no function name"));
    let group = format!("{DEFAULT_GROUP_PREFIX}{function}");

    // 2. BEFORE anything ran, that name is a group the account does not hold — which is the one
    //    case the refusal belongs to, and what makes the answers below about existence rather
    //    than about the request.
    let before = logs(
        &router,
        "FilterLogEvents",
        json!({ "logGroupName": group.clone() }),
    )
    .await;
    assert_group_absent(case, &group, &before);
    let absent = logs(
        &router,
        "FilterLogEvents",
        json!({ "logGroupName": ABSENT_GROUP }),
    )
    .await;
    assert_group_absent(case, ABSENT_GROUP, &absent);

    // 3. The runs: each one answers, and each leaves its own line in the group its delivery
    //    creates on the function's behalf.
    wait_until_active(&router, case, &function).await;
    let markers: Vec<String> = (0..RUNS).map(|run| format!("{FUNCTION}-{run}")).collect();
    for marker in &markers {
        invoke(&router, case, &function, marker).await;
    }
    let held = every_run_delivered(&router, &group, RUNS).await;
    for marker in &markers {
        let printed = format!("{PRINTED_PREFIX}{marker}");
        assert!(
            held.iter().any(|message| message.contains(&printed)),
            "case {case:?}: expected {group:?} to hold the line a run printed, {printed:?}, got \
             {held:?}"
        );
    }
    assert_eq!(
        held.iter()
            .filter(|message| message.starts_with(REPORT_PREFIX))
            .count(),
        RUNS,
        "case {case:?}: expected {group:?} to hold one closing frame per run, got {held:?}"
    );

    // 4. The whole group, as the search reports it: the baseline every narrowed row is compared
    //    with, and where the bounds the rows narrow by come from.
    let (whole, _) = searched(&router, case, &json!({ "logGroupName": group.clone() })).await;
    let every_message = messages_of(&whole);
    let stamps = timestamps_of(&whole);
    assert_eq!(
        stamps.len(),
        whole.len(),
        "case {case:?}: expected every event to carry an instant, got {stamps:?}"
    );
    assert!(
        stamps.windows(2).all(|pair| pair[0] <= pair[1]),
        "case {case:?}: expected the default search to report events oldest-first, got {stamps:?}"
    );
    let span = (
        stamps.iter().copied().min().unwrap_or_default(),
        stamps.iter().copied().max().unwrap_or_default(),
    );
    let streams: Vec<String> = whole
        .iter()
        .filter_map(|event| event.get("logStreamName").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();
    assert!(
        !streams.is_empty(),
        "case {case:?}: expected the runs' records to be in a stream of {group:?}, got none"
    );

    // ... and reading it again changes nothing a later caller sees: the same events under the
    // same ids.
    let (again, _) = searched(&router, case, &json!({ "logGroupName": group.clone() })).await;
    assert_eq!(
        again, whole,
        "case {case:?}: expected a second search of {group:?} to report the same page, got \
         {again:?} where it reported {whole:?}"
    );

    // 5. Every way of naming and narrowing the search of a group that EXISTS is answered, and
    //    answered over the events the narrowing admits.
    let printed = format!("{PRINTED_PREFIX}{}", markers[0]);
    let arn = group_arn(&router, case, &group).await;
    for row in rows(&group, &arn, span, &streams, &printed) {
        let case = row.case.as_str();
        let (events, _) = searched(&router, case, &row.request).await;
        let messages = messages_of(&events);
        match &row.expect {
            Expect::EveryRecord => {
                for message in &every_message {
                    assert!(
                        messages.contains(message),
                        "case {case:?}: expected the search to report every record of the group, \
                         {message:?} included, got {messages:?}"
                    );
                }
            }
            Expect::OnlyCarrying(term) => {
                assert!(
                    !messages.is_empty(),
                    "case {case:?}: expected the search to report the events it selects, got none"
                );
                assert!(
                    messages.iter().all(|message| message.contains(term)),
                    "case {case:?}: expected every reported event to carry {term:?}, got \
                     {messages:?}"
                );
            }
            Expect::NoEvent => {
                assert!(
                    messages.is_empty(),
                    "case {case:?}: expected a successful page of no events, got {messages:?}"
                );
            }
            Expect::AtMost(bound) => {
                assert!(
                    !messages.is_empty() && messages.len() <= *bound,
                    "case {case:?}: expected at most {bound} event(s) and at least one, got \
                     {messages:?}"
                );
                for message in &messages {
                    assert!(
                        every_message.contains(message),
                        "case {case:?}: expected a bounded page to report events of the group, \
                         got {message:?} which it does not hold"
                    );
                }
            }
            Expect::SomeRecord => {
                assert!(
                    !messages.is_empty(),
                    "case {case:?}: expected the search to report the events it selects, got none"
                );
                for message in &messages {
                    assert!(
                        every_message.contains(message),
                        "case {case:?}: expected every reported event to be a record of the \
                         group, got {message:?} which it does not hold"
                    );
                }
            }
        }
    }

    // 6. The page bound and the token together: "you can return these additional log events by
    //    providing the nextToken in a subsequent FilterLogEvents operation", and "if the results
    //    don't include a nextToken, then pagination is finished". Walking the group one event to
    //    the page therefore reaches every record it holds and then stops.
    let paging = "the group walked one event to the page, following each nextToken";
    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..=every_message.len() {
        let mut request = json!({ "logGroupName": group.clone(), "limit": 1 });
        if let Some(resume) = &token {
            request["nextToken"] = json!(resume);
        }
        let (page, offered) = searched(&router, paging, &request).await;
        assert!(
            page.len() <= 1,
            "case {paging:?}: expected at most the page bound of 1 event, got {}",
            page.len()
        );
        walked.extend(messages_of(&page));
        // A page that carried nothing has walked the scan to its end, whatever it offers.
        if page.is_empty() {
            break;
        }
        match offered {
            Some(resume) => token = Some(resume),
            // No token: pagination is finished, so the walk ends here.
            None => break,
        }
    }
    assert_eq!(
        walked, every_message,
        "case {paging:?}: expected the walk to report every record of the group, in the order one \
         page reports them, got {walked:?} where the group holds {every_message:?}"
    );

    // 7. ... while a group the account still does not hold is refused exactly as before: the
    //    successful reads above did not turn the refusal into a 200 for everything.
    let still_absent = logs(
        &router,
        "FilterLogEvents",
        json!({ "logGroupName": ABSENT_GROUP }),
    )
    .await;
    assert_group_absent(case, ABSENT_GROUP, &still_absent);
}
