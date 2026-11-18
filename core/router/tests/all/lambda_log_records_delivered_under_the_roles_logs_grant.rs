// SPDX-License-Identifier: CC0-1.0

//! The FORM each record of an invocation's log takes in the function's own Amazon CloudWatch
//! Logs group: the bytes a client matching one reads.
//!
//! RULE under test: **a run's log is a stream of newline-terminated records, and AWS Lambda
//! delivers each of them to CloudWatch Logs as the write that produced it left it — the
//! record's own text CLOSED BY THAT LINE BREAK, exactly one per event — for the platform's own
//! framing and for whatever the handler wrote alike, whichever managed runtime wrote it and
//! whichever `LogFormat` the function renders its records in. Whether there is a group to read
//! at all is decided by the function's execution role: Lambda makes the delivery's
//! `logs:CreateLogGroup`, `logs:CreateLogStream` and `logs:PutLogEvents` calls FOR the
//! function, so a role allowing none of them leaves no group behind, while the invocation
//! itself is answered the same either way.**
//!
//! A client reading a function's log matches a record it is looking for — a line a handler
//! printed, a frame of the run — against what the group holds. A record delivered short of the
//! break it was written with is a record no such match finds, so an operator polling a group
//! for the line their handler printed waits forever while the log is right there.
//!
//! Two services have to agree for that to hold — AWS Lambda runs the handler and writes the
//! records as the function's execution role, IAM answers for what that role may do, and Amazon
//! CloudWatch Logs is where the records either are or are not — so the suite lives here rather
//! than in any one crate; CloudFormation deploys each row's role and the function running as
//! it. What the neighbours pin and this one does not restate:
//! [`crate::cloudformation_lambda_function_log_destination`] drives WHERE a deployed function's
//! records land (and owns the wire harness this suite reuses),
//! `cfn_deployed_lambda_log_group_retention_and_role_gate` the table of grant and non-grant
//! documents the delivery is gated by, and `lambda_invoke_log_tail_is_the_runs_own_log` the
//! `LogResult` an invocation answers. What only this suite reaches is the exact bytes of each
//! delivered record.
//!
//! The rows differ along what the rule generalises over rather than restating one template: the
//! managed RUNTIME that wrote the records, the `LogFormat` they are rendered in (the plain-text
//! framing and the structured one, whose records are JSON objects), the FORM the role's grant
//! takes (AWS's own managed policy for the purpose, attached; an equivalent document embedded
//! in the role), and a role carrying no permissions at all, which leaves nothing to read.
//!
//! No expected value here comes from a recording: each marker and function name is this suite's
//! own, and every asserted record is built from them.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
// The ONE assembled emulator, wire harness and group readers this test crate owns for a
// deployed function's log, rather than second copies of them.
use crate::cloudformation_lambda_function_log_destination::{
    cloudformation, emulator, filtered, groups_named, invoke, messages_at_least, messages_in,
    trust_policy, wait_until_active, CREATE_COMPLETE, DEFAULT_GROUP_PREFIX, FUNCTION_TIMEOUT,
};
use crate::query_wire::outputs_of;

/// The logical ids each row's template declares.
const ROLE_ID: &str = "ExecutionRole";
const FUNCTION_ID: &str = "Logger";

/// The ARN of the policy AWS publishes for a function that writes its own logs, which is what
/// a least-privilege execution role carries for the purpose.
const BASIC_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The three calls an invocation's log delivery makes, which is what that policy allows and
/// what one row's own document names instead of it.
const DELIVERY_ACTIONS: [&str; 3] = [
    "logs:CreateLogGroup",
    "logs:CreateLogStream",
    "logs:PutLogEvents",
];

/// The fixed text of the frames the plain-text format writes around one run.
const START_PREFIX: &str = "START RequestId: ";
/// See [`START_PREFIX`].
const END_PREFIX: &str = "END RequestId: ";
/// See [`START_PREFIX`].
const REPORT_PREFIX: &str = "REPORT RequestId: ";
/// See [`START_PREFIX`]: the frames of one run, which is the least the platform itself writes.
const TEXT_FRAMES: [&str; 3] = [START_PREFIX, END_PREFIX, REPORT_PREFIX];

/// The member a structured platform record names its own kind under, and the prefix every one
/// of those kinds carries — "select between plain text and structured JSON"
/// (`smithy_query(query="doc",shape="LoggingConfig",member="LogFormat")`), where each frame of
/// the run is one JSON object instead of one line.
const TYPE_MEMBER: &str = "type";
/// The member TYPE_MEMBER's companion: see [`TYPE_MEMBER`].
const PLATFORM_PREFIX: &str = "platform.";

/// The `LogFormat` a row renders its records in: the two the member declares.
const TEXT_FORMAT: &str = "Text";
/// See [`TEXT_FORMAT`].
const JSON_FORMAT: &str = "JSON";

/// How many records ONE run of these handlers leaves: the three frames of the run, and the one
/// line the handler printed.
const RECORDS_PER_RUN: usize = 4;

/// The python handler every python row deploys: it prints the marker the event carries and
/// answers it, so the record it wrote can be told from any other row's.
const PYTHON_HANDLER: &str = r#"
def handler(event, context):
    print("out-%s" % event["marker"])
    return {"logged": event["marker"]}
"#;

/// The node handler, writing the same record through that runtime's own console.
const NODE_HANDLER: &str = r#"
exports.handler = async (event) => {
    console.log(`out-${event.marker}`);
    return { logged: event.marker };
};
"#;

/// Where a row's grant of the delivery's calls comes from, which is what decides whether there
/// is a group to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grant {
    /// AWS's own managed policy for the purpose, attached by `ManagedPolicyArns`.
    AwsManagedPolicy,
    /// An equivalent document embedded in the role itself.
    EmbeddedPolicy,
    /// Nothing at all: the role carries its trust document and no permissions.
    None,
}

/// One row: the runtime that writes the records, the format they are rendered in, and the
/// grant its execution role carries.
struct Row {
    case: &'static str,
    /// The `FunctionName` the template states, which is also the subject of its group's name.
    function: &'static str,
    runtime: &'static str,
    handler: &'static str,
    source: &'static str,
    /// The `LoggingConfig$LogFormat` the function renders its records in.
    format: &'static str,
    /// Whether the record the handler's own write leaves is that text ALONE: a `print` from a
    /// python handler is delivered as the line it wrote, while a node runtime frames a console
    /// line with the instant, the invocation's id and the level ahead of it.
    whole_record: bool,
    grant: Grant,
}

/// The rows driven: each managed runtime, each format, each form the grant takes, and the role
/// that carries none.
const ROWS: &[Row] = &[
    Row {
        case: "a python run on the plain-text format, granted by AWS's own managed policy",
        function: "xsvc-log-record-python-text",
        runtime: "python3.12",
        handler: "index.handler",
        source: PYTHON_HANDLER,
        format: TEXT_FORMAT,
        whole_record: true,
        grant: Grant::AwsManagedPolicy,
    },
    Row {
        case: "a python run on the structured format, granted by a document of the role's own",
        function: "xsvc-log-record-python-json",
        runtime: "python3.12",
        handler: "index.handler",
        source: PYTHON_HANDLER,
        format: JSON_FORMAT,
        whole_record: true,
        grant: Grant::EmbeddedPolicy,
    },
    Row {
        case: "a node run on the plain-text format",
        function: "xsvc-log-record-node-text",
        runtime: "nodejs22.x",
        handler: "index.handler",
        source: NODE_HANDLER,
        format: TEXT_FORMAT,
        whole_record: false,
        grant: Grant::AwsManagedPolicy,
    },
    Row {
        case: "a python run whose role carries no permissions at all",
        function: "xsvc-log-record-ungranted",
        runtime: "python3.12",
        handler: "index.handler",
        source: PYTHON_HANDLER,
        format: TEXT_FORMAT,
        whole_record: true,
        grant: Grant::None,
    },
];

impl Row {
    /// Whether this row's role lets the delivery be made at all.
    fn delivers(&self) -> bool {
        self.grant != Grant::None
    }

    /// The template this row deploys: its execution role, and the function that runs as it.
    fn template(&self) -> String {
        let mut role = Map::new();
        role.insert("AssumeRolePolicyDocument".to_string(), trust_policy());
        match self.grant {
            Grant::AwsManagedPolicy => {
                role.insert(
                    "ManagedPolicyArns".to_string(),
                    json!([BASIC_EXECUTION_POLICY]),
                );
            }
            Grant::EmbeddedPolicy => {
                role.insert(
                    "Policies".to_string(),
                    json!([{
                        "PolicyName": "log-delivery",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": DELIVERY_ACTIONS,
                                "Resource": "*"
                            }]
                        }
                    }]),
                );
            }
            Grant::None => {}
        }
        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": {
                ROLE_ID: {
                    "Type": "AWS::IAM::Role",
                    "Properties": Value::Object(role)
                },
                FUNCTION_ID: {
                    "Type": "AWS::Lambda::Function",
                    "Properties": {
                        "FunctionName": self.function,
                        "Role": { "Fn::GetAtt": [ROLE_ID, "Arn"] },
                        "Runtime": self.runtime,
                        "Handler": self.handler,
                        "Timeout": FUNCTION_TIMEOUT,
                        "LoggingConfig": { "LogFormat": self.format },
                        "Code": { "ZipFile": self.source }
                    }
                }
            },
            "Outputs": {
                "FunctionName": { "Value": { "Ref": FUNCTION_ID } }
            }
        })
        .to_string()
    }
}

/// The group a function whose `LoggingConfig` names none logs to: "a default log group named
/// `/aws/lambda/<function name>`", which is where every row's records go.
fn default_group(function: &str) -> String {
    format!("{DEFAULT_GROUP_PREFIX}{function}")
}

/// Deploy `row`'s stack and answer the function name it published, once the stack is complete
/// and AWS Lambda reports that function active.
async fn given_deployed(router: &MultiServiceRouter, row: &Row, stack: &str) -> String {
    let case = row.case;
    let template = row.template();
    let created = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &template),
            // The template declares an IAM role, which is acknowledged with CAPABILITY_IAM.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {} with {}",
        created.status,
        created.text()
    );
    let (reached, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );
    let function = outputs_of(&described)
        .get("FunctionName")
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the stack published no function name in {described}")
        });
    // The function has to be invocable before any of this can be read: "During this time, you
    // can't invoke or modify the function" (`CreateFunction`). The span is that service's own,
    // so the wait is on that CONDITION.
    wait_until_active(router, case, &function).await;
    function
}

/// The text of ONE delivered record: the message the group holds with the single line break
/// that closed it taken off, asserting it was there and was the only one.
///
/// This is the rule under test applied once per record: a record is text closed by exactly one
/// break, so anything else — no terminator, a second one, a record the delivery split — fails
/// the row here rather than at the assertion that reads the text.
fn record_text(case: &str, message: &str) -> String {
    let text = record_body(message).unwrap_or_else(|| {
        panic!("case {case:?}: every delivered record is closed by its line break, got {message:?}")
    });
    assert!(
        !text.ends_with('\n'),
        "case {case:?}: a record carries the ONE break that closed it, got {message:?}"
    );
    text.to_string()
}

/// The text a delivered record carries, or `None` for a message that is not closed by a line
/// break at all.
///
/// `pub(crate)` because the suites that read a function's records for what they SAY rather than
/// for their form go through one statement of it ([`crate::s3_notification_invokes_its_lambda_target`],
/// [`crate::s3_notification_event_classes_run_their_lambda_targets`]), so this crate says once
/// that the break belongs to the record.
#[must_use]
pub(crate) fn record_body(message: &str) -> Option<&str> {
    message.strip_suffix('\n')
}

/// RULE: every record of a run reaches the function's group as the write left it — its text
/// closed by exactly one line break — whichever runtime wrote it and whichever format renders
/// it, and the group exists at all only while the execution role allows the delivery's calls.
#[tokio::test]
async fn every_delivered_log_record_carries_the_line_break_that_closed_its_write() {
    // ONE emulator for every row: each row deploys a stack of its own, so the function and the
    // group a row reads are still that row's alone.
    let router = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack = format!("xsvc-log-record-{index}");
        let function = given_deployed(&router, row, &stack).await;
        let group = default_group(&function);
        let marker = format!("{function}-{index}");
        let printed = format!("out-{marker}");

        // 1. The invocation itself: answered 200 with the handler's own result and no
        //    `FunctionError`, whatever the role allows of the delivery.
        invoke(&router, case, &function, &marker).await;

        if !row.delivers() {
            // 2. A role allowing none of the delivery's calls leaves NO group behind: not an
            //    empty one, and none a client holding the name can find.
            let named = groups_named(&router, case, &group).await;
            assert!(
                named.is_empty(),
                "case {case:?}: a role allowing none of {DELIVERY_ACTIONS:?} creates no log \
                 group, got {named:?}"
            );
            let answered = filtered(&router, &group).await;
            assert_ne!(
                answered.status,
                200,
                "case {case:?}: expected no such group to read at all, got {}",
                answered.text()
            );
            assert!(
                messages_in(&router, &group).await.is_empty(),
                "case {case:?}: expected nothing of the run to be readable"
            );
            continue;
        }

        // 2. What the group holds: one event per record the run wrote, each closed by the break
        //    that closed its write — which is what [`record_text`] asserts of every one of
        //    them, including the frames of the run.
        let held = messages_at_least(&router, &group, RECORDS_PER_RUN).await;
        assert!(
            held.len() >= RECORDS_PER_RUN,
            "case {case:?}: expected at least the run's {RECORDS_PER_RUN} records in {group:?}, \
             got {held:?}"
        );
        let texts: Vec<String> = held
            .iter()
            .map(|message| record_text(case, message))
            .collect();

        // 3. The record the handler's own write left is the text it wrote CLOSED BY its break:
        //    the bytes a client matching the line it printed compares against. Where the
        //    runtime delivers that write as the record whole (a python `print`), the group holds
        //    exactly `out-<marker>\n`; where it frames the write (a node console line), the
        //    record ENDS with it, after the fields the runtime put ahead of it.
        let terminated = format!("{printed}\n");
        assert!(
            held.iter().any(|message| message.ends_with(&terminated)),
            "case {case:?}: expected {group:?} to hold the handler's write closed by its break, \
             ending {terminated:?}, got {held:?}"
        );
        if row.whole_record {
            assert!(
                held.contains(&terminated),
                "case {case:?}: expected {group:?} to hold the record {terminated:?}, got \
                 {held:?}"
            );
        }

        // 4. The platform's own framing of the run is delivered the same way: under the
        //    plain-text format as its three lines, under the structured one as platform records.
        if row.format == JSON_FORMAT {
            let frames = texts
                .iter()
                .filter(|text| {
                    serde_json::from_str::<Value>(text).ok().is_some_and(|record| {
                        record
                            .get(TYPE_MEMBER)
                            .and_then(Value::as_str)
                            .is_some_and(|kind| kind.starts_with(PLATFORM_PREFIX))
                    })
                })
                .count();
            assert_eq!(
                frames,
                TEXT_FRAMES.len(),
                "case {case:?}: expected the run's {} platform records, got {texts:?}",
                TEXT_FRAMES.len()
            );
        } else {
            for frame in TEXT_FRAMES {
                assert!(
                    texts.iter().any(|text| text.starts_with(frame)),
                    "case {case:?}: expected a record starting {frame:?} in {group:?}, got \
                     {texts:?}"
                );
            }
        }

        // 5. So the records the group holds, taken in order, ARE the run's log: concatenating
        //    them gives one line per record, which a stream of records short of their
        //    terminators — or carrying a second one — does not.
        let stream: String = held.concat();
        assert_eq!(
            stream.lines().count(),
            held.len(),
            "case {case:?}: expected the group's records to concatenate into one line each, got \
             {stream:?}"
        );
    }
}
