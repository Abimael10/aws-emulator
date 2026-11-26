// SPDX-License-Identifier: CC0-1.0

//! What a client that POLLS an execution for a verdict reads when the one `Task` state's
//! Lambda invocation is a call the state machine's execution role is not permitted to
//! make — driven over each service's own wire protocol through the production
//! `MultiServiceRouter` with Step Functions, IAM and Lambda mounted on ONE engine (the
//! shape `core/router/src/lib.rs` mounts them in), so the documents the call is judged
//! against are the ones IAM's own handlers wrote and the function the `Resource` names is
//! one Lambda really holds.
//!
//! RULE: a `Task` state's integration call that the documents in force on the state
//! machine's `roleArn` do not allow is that state's FAILURE, so the execution always
//! settles — `DescribeExecution` leaves `RUNNING` for a terminal `FAILED` with a
//! `stopDate`, the `error` and `cause` of the invoked service's own access-denied refusal,
//! and `GetExecutionHistory` REPORTS the same refusal twice over: in the failed attempt's
//! own event details, under the event family the `Resource` named the work in, and again
//! in the terminal event that closes the history, whose `error` and `cause` are the ones
//! the terminal read reports. A waiter on such an execution therefore reaches a verdict
//! instead of waiting for an answer nothing will send.
//!
//! The rows differ along what the rule generalises over: HOW the role withholds the
//! invocation (its only grant being of another service's action, an explicit `Deny` over a
//! grant of the invocation, a grant of the invocation scoped to another function, and a
//! grant of a different action of the same service on the same function) and the FORM the
//! `Resource` names the invocation in (the function's own ARN, the optimized
//! `lambda:invoke` integration and the AWS SDK one), which decides which family of history
//! events the attempt is recorded under. A row whose role IS granted the invocation is the
//! positive control: the very same definition's call is then made, the function really
//! runs, and the execution settles `SUCCEEDED` on its answer — which is what says the
//! denial every other row reads is the DOCUMENT's doing and not a Lambda task that fails
//! whatever the role says.
//!
//! What the neighbours pin and this one does not repeat:
//! `denied_lambda_task_integration_patterns` holds the decision over the integration
//! PATTERN, and `denied_task_settles_the_execution` the same settling over another
//! service's integration families and over the `Retry`/`Catch` wrappings of the denied
//! state. What only this suite reaches is the history's own REPORT of the refusal — the
//! failed attempt's details and the terminal event's — read beside the terminal
//! `DescribeExecution` that a poller stops on.
//!
//! Every wait here waits for the CONDITION (`Chain::settled` looks again until the
//! execution has left `RUNNING`), so nothing here depends on how fast the host runs. The
//! harness is `lambda_task_control_flow`'s `Chain::with_iam` and the principal reader is
//! `task_refusal_session_principal`'s; `tests/all/main.rs` is this binary's crate root, so
//! both are reached through `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    EVENT_EXECUTION_FAILED, EVENT_EXECUTION_SUCCEEDED, EVENT_LAMBDA_FUNCTION_FAILED,
    EVENT_TASK_FAILED,
};

use crate::lambda_task_control_flow::Chain;
use crate::state_machine_lifecycle::string;
use crate::task_refusal_session_principal::session_principal;

/// The handler the function under test carries. It answers the event it was handed, so a
/// row whose cause carried any of it would be a row whose call was made after all.
const HANDLER_SOURCE: &str = r#"
def handler(event, context):
    return {"saw": event}
"#;

/// The member that handler answers under, which no denied row's cause may carry.
const SAW: &str = "saw";

/// The document every row's execution is started with.
const ORDER: &str = "o-1";

/// The IAM action an invocation is subject to: the one Lambda PUBLISHES for `Invoke`,
/// which is not the operation's own name.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// An action a least-privilege orchestration role carries that says nothing about any
/// invocation.
const UNRELATED_ACTION: &str = "logs:PutLogEvents";

/// Another action of the INVOKED service on the very same function, which likewise says
/// nothing about invoking it.
const OTHER_LAMBDA_ACTION: &str = "lambda:GetFunction";

/// The name a function OTHER than the one every row's `Task` addresses carries, so a
/// grant scoped to it grants nothing about the call that is made.
const OTHER_FUNCTION_NAME: &str = "not-the-one";

/// The ARN of that other function: the addressed function's own ARN with its name
/// replaced, so the two differ in nothing but the resource they name.
fn other_function(function_arn: &str) -> String {
    let (head, _) = function_arn
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("expected a Lambda function ARN, got {function_arn:?}"));
    format!("{head}:{OTHER_FUNCTION_NAME}")
}

/// The error a `Task` state reports when the role may not make the invocation: Lambda's
/// base SDK exception, which is what a refusal `Invoke` models no error for arrives under
/// (`smithy_query(query="operation",operation="Invoke",model_path="services/
/// service-com-amazonaws-lambda-awsgirapiservice/smithy-model/
/// com-amazonaws-lambda-awsgirapiservice.json")` models no access-denied error, so no
/// client carries a class for the code such an answer names).
const REFUSED_ERROR: &str = "Lambda.AWSLambdaException";

/// The fixed text of the sentence that refusal carries, which is what makes the cause a
/// DENIAL rather than any other refusal.
const REFUSED_TEXT: &str = "not authorized to perform: lambda:InvokeFunction";

/// The clause IAM's sentence closes with where no document in force states anything about
/// the call, so the default deny decides it.
const UNSTATED_CLAUSE: &str = "because no identity-based policy allows the \
                               lambda:InvokeFunction action";

/// The clause it closes with instead where an explicit `Deny` overturns a grant, which is
/// a different reading of the same documents and so a different sentence.
const EXPLICIT_DENY_CLAUSE: &str = "with an explicit deny in an identity-based policy";

/// Which form a `Task` state's `Resource` names the invocation in.
#[derive(Clone, Copy, Debug)]
enum Form {
    /// The function's OWN ARN.
    FunctionArn,
    /// `arn:aws:states:::lambda:invoke`, the optimized integration.
    Optimized,
    /// `arn:aws:states:::aws-sdk:lambda:invoke`, the AWS SDK family.
    AwsSdk,
}

/// How the role's documents withhold the invocation. Each is a different reading in IAM's
/// own order — an explicit `Deny` refuses outright, and every other row states nothing
/// about the call and so withholds it.
#[derive(Clone, Copy, Debug)]
enum Withheld {
    /// The role's only grant is of another service's action.
    UnrelatedAction,
    /// The role IS granted the invocation, and an explicit `Deny` overturns it.
    ExplicitDeny,
    /// The role is granted the invocation on a DIFFERENT function.
    OtherResource,
    /// The role is granted another action of the same service on the same function.
    OtherAction,
}

impl Withheld {
    /// The clause IAM's sentence closes with for this reading of the documents.
    fn because(self) -> &'static str {
        match self {
            Self::ExplicitDeny => EXPLICIT_DENY_CLAUSE,
            Self::UnrelatedAction | Self::OtherResource | Self::OtherAction => UNSTATED_CLAUSE,
        }
    }
}

/// One row: the case, the form the `Resource` names the invocation in, and how the role
/// withholds the invocation — `None` for the positive control, whose role is granted it.
struct Row {
    case: &'static str,
    form: Form,
    withheld: Option<Withheld>,
}

impl Row {
    /// The `Resource` this row's one `Task` state carries.
    fn resource(&self, function_arn: &str) -> String {
        match self.form {
            Form::FunctionArn => function_arn.to_string(),
            Form::Optimized => "arn:aws:states:::lambda:invoke".to_string(),
            Form::AwsSdk => "arn:aws:states:::aws-sdk:lambda:invoke".to_string(),
        }
    }

    /// The event that closes this row's attempt as a failure: a `Resource` that is the
    /// function's own ARN records the LAMBDA FUNCTION family of events, and an integration
    /// token the TASK family — the same work, recorded under the family the `Resource`
    /// named it in.
    fn failure_event(&self) -> &'static str {
        match self.form {
            Form::FunctionArn => EVENT_LAMBDA_FUNCTION_FAILED,
            Form::Optimized | Form::AwsSdk => EVENT_TASK_FAILED,
        }
    }

    /// The member of a history event of [`Self::failure_event`] that carries its details.
    fn failure_details(&self) -> &'static str {
        match self.form {
            Form::FunctionArn => "lambdaFunctionFailedEventDetails",
            Form::Optimized | Form::AwsSdk => "taskFailedEventDetails",
        }
    }

    /// The statements the row's role carries: for every withholding row one that leaves
    /// the invocation of the function the `Task` addresses unallowed, and for the positive
    /// control a grant of exactly that invocation.
    fn statements(&self, function_arn: &str) -> Value {
        let Some(withheld) = self.withheld else {
            return json!([{
                "Effect": "Allow",
                "Action": INVOKE_ACTION,
                "Resource": function_arn,
            }]);
        };
        match withheld {
            Withheld::UnrelatedAction => json!([{
                "Effect": "Allow",
                "Action": UNRELATED_ACTION,
                "Resource": "*",
            }]),
            Withheld::ExplicitDeny => json!([
                {
                    "Effect": "Allow",
                    "Action": INVOKE_ACTION,
                    "Resource": function_arn,
                },
                {
                    "Effect": "Deny",
                    "Action": INVOKE_ACTION,
                    "Resource": function_arn,
                },
            ]),
            Withheld::OtherResource => json!([{
                "Effect": "Allow",
                "Action": INVOKE_ACTION,
                "Resource": other_function(function_arn),
            }]),
            Withheld::OtherAction => json!([{
                "Effect": "Allow",
                "Action": OTHER_LAMBDA_ACTION,
                "Resource": function_arn,
            }]),
        }
    }

    /// The one-state definition the row runs, which is the whole workflow: the client
    /// under test has nothing else to read a verdict from.
    fn definition(&self, function_arn: &str) -> Value {
        let task = match self.form {
            // The bare form takes no request of its own: the state's input IS the event.
            Form::FunctionArn => json!({
                "Type": "Task",
                "Resource": self.resource(function_arn),
                "End": true,
            }),
            Form::Optimized | Form::AwsSdk => json!({
                "Type": "Task",
                "Resource": self.resource(function_arn),
                "Parameters": {
                    "FunctionName": function_arn,
                    "Payload": execution_input(),
                },
                "End": true,
            }),
        };
        json!({ "StartAt": "Invoke", "States": { "Invoke": task } })
    }
}

/// The rows, spanning how the document withholds the invocation and the form the
/// `Resource` asks for it in, with the granted rows as the positive controls.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the function's own ARN, under a role whose only grant is of another \
                   service's action",
            form: Form::FunctionArn,
            withheld: Some(Withheld::UnrelatedAction),
        },
        Row {
            case: "the function's own ARN, under a role whose grant of the invocation an \
                   explicit deny overturns",
            form: Form::FunctionArn,
            withheld: Some(Withheld::ExplicitDeny),
        },
        Row {
            case: "the function's own ARN, under a role granted another action of the same \
                   service on it",
            form: Form::FunctionArn,
            withheld: Some(Withheld::OtherAction),
        },
        Row {
            // The positive control for the bare form: the same definition, under a role
            // granted exactly the invocation the rows above are withheld.
            case: "the function's own ARN, under a role granted the invocation",
            form: Form::FunctionArn,
            withheld: None,
        },
        Row {
            case: "the optimized integration, under a role whose invocation grant names \
                   another function",
            form: Form::Optimized,
            withheld: Some(Withheld::OtherResource),
        },
        Row {
            case: "the optimized integration, under a role whose grant of the invocation \
                   an explicit deny overturns",
            form: Form::Optimized,
            withheld: Some(Withheld::ExplicitDeny),
        },
        Row {
            case: "the AWS SDK integration, under a role whose only grant is of another \
                   service's action",
            form: Form::AwsSdk,
            withheld: Some(Withheld::UnrelatedAction),
        },
        Row {
            case: "the AWS SDK integration, under a role whose invocation grant names \
                   another function",
            form: Form::AwsSdk,
            withheld: Some(Withheld::OtherResource),
        },
        Row {
            // The positive control for the integration families, whose answer the task
            // reports under the invocation's own `Payload` member.
            case: "the optimized integration, under a role granted the invocation",
            form: Form::Optimized,
            withheld: None,
        },
    ]
}

/// The document every row's execution carries.
fn execution_input() -> Value {
    json!({ "order": ORDER })
}

/// The events of `execution`, in the order `GetExecutionHistory` answers them.
async fn history(chain: &Chain, case: &str, execution: &str) -> Vec<Value> {
    let answered = chain
        .sfn(
            case,
            "GetExecutionHistory",
            &json!({ "executionArn": execution }),
        )
        .await;
    answered
        .get("events")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected events in {answered}"))
        .clone()
}

/// The one event of type `named` in `events`.
///
/// `pub(crate)` because every suite that reads one event of a failed execution's history
/// reads it this way: `tests/all/main.rs` is one crate root, so the reading is stated here
/// once.
pub(crate) fn only(case: &str, events: &[Value], named: &str) -> Value {
    let found: Vec<&Value> = events
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some(named))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one {named} event, got {} in {:?}",
        found.len(),
        types(events)
    );
    found[0].clone()
}

/// The types of `events`, for an assertion that names what it actually saw.
fn types(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| event.get("type").and_then(Value::as_str))
        .collect()
}

/// The `error` and `cause` the detail member `member` of `event` reports.
fn verdict(case: &str, event: &Value, member: &str) -> (String, String) {
    let details = event
        .get(member)
        .unwrap_or_else(|| panic!("case {case:?}: expected {member} in {event}"));
    (string(details, "error"), string(details, "cause"))
}

/// What the invocation answered, as the row's own form reports it: the function's return
/// value itself where the `Resource` named the function, and under the `Payload` member of
/// the invocation's answer for either integration family.
fn answered(case: &str, form: Form, output: &Value) -> Value {
    match form {
        Form::FunctionArn => output.clone(),
        Form::Optimized | Form::AwsSdk => output.get("Payload").cloned().unwrap_or_else(|| {
            panic!("case {case:?}: expected the invocation's Payload in {output}")
        }),
    }
}

/// Every row: a `Task` state whose Lambda invocation the execution role withholds drives
/// the execution to a terminal `FAILED` that a poller reads, and the history reports that
/// very refusal — in the failed attempt's own details and in the terminal event that
/// closes it — while the same definition under a role granted the invocation really runs
/// the function.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_denied_invocation_settles_the_execution_and_the_history_reports_the_refusal() {
    let chain = Chain::with_iam();
    let function = chain
        .given_function_of("history-verdict", HANDLER_SOURCE, Some(20))
        .await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let role = chain
            .role(
                case,
                &format!("VerdictRole{index}"),
                &row.statements(&function),
            )
            .await;
        let created = chain
            .sfn(
                case,
                "CreateStateMachine",
                &json!({
                    "name": format!("history-verdict-{index}"),
                    "definition": row.definition(&function).to_string(),
                    "roleArn": role,
                }),
            )
            .await;
        let started = chain
            .sfn(
                case,
                "StartExecution",
                &json!({
                    "stateMachineArn": string(&created, "stateMachineArn"),
                    "name": "only",
                    "input": execution_input().to_string(),
                }),
            )
            .await;
        let execution = string(&started, "executionArn");

        // A poller reaches a verdict either way: the execution leaves RUNNING on its own,
        // and the terminal read carries the instant it stopped.
        let settled = chain.settled(case, &execution).await;
        let start_date = settled
            .get("startDate")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("case {case:?}: expected a startDate in {settled}"));
        let stop_date = settled.get("stopDate").and_then(Value::as_f64).unwrap_or_else(|| {
            panic!("case {case:?}: expected a terminal execution to report a stopDate in {settled}")
        });
        assert!(
            stop_date >= start_date,
            "case {case:?}: expected the execution to stop no earlier than it started, got \
             start {start_date} and stop {stop_date}"
        );

        // The positive control: the very same definition, under a role granted the
        // invocation, makes the call — so the function really runs and its own answer is
        // the execution's outcome.
        let Some(withheld) = row.withheld else {
            assert_eq!(
                settled.get("status").and_then(Value::as_str),
                Some("SUCCEEDED"),
                "case {case:?}: expected the granted call to be made, got {settled}"
            );
            let output: Value = serde_json::from_str(&string(&settled, "output"))
                .unwrap_or_else(|why| panic!("case {case:?}: the output is JSON: {why}"));
            assert_eq!(
                answered(case, row.form, &output),
                json!({ SAW: execution_input() }),
                "case {case:?}: expected the function's own answer, got {output}"
            );
            let events = history(&chain, case, &execution).await;
            assert_eq!(
                events.last().and_then(|event| event.get("type")),
                Some(&json!(EVENT_EXECUTION_SUCCEEDED)),
                "case {case:?}: expected the history to close on {EVENT_EXECUTION_SUCCEEDED}, \
                 got {:?}",
                types(&events)
            );
            continue;
        };

        // A withholding row: the terminal read carries the refusal.
        assert_eq!(
            settled.get("status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: expected the denied invocation to fail the execution, got \
             {settled}"
        );
        let described_error = string(&settled, "error");
        let described_cause = string(&settled, "cause");
        assert_eq!(
            described_error, REFUSED_ERROR,
            "case {case:?}: expected the invoked service's own refusal, got {settled}"
        );
        let principal = session_principal(&role, &described_cause);
        // The clause the sentence closes with is the row's own reading of the documents, so
        // an explicit deny cannot be reported as a grant merely absent, or the other way
        // round.
        for fragment in [
            REFUSED_TEXT,
            withheld.because(),
            function.as_str(),
            principal.as_str(),
        ] {
            assert!(
                described_cause.contains(fragment),
                "case {case:?}: expected the cause to carry {fragment:?}, got \
                 {described_cause:?}"
            );
        }
        assert!(
            !described_cause.contains(SAW),
            "case {case:?}: a call that was never made carries nothing the function would \
             have answered, got {described_cause:?}"
        );

        // The history reports the refusal as the ATTEMPT's own failure, under the family
        // the `Resource` named the work in.
        let events = history(&chain, case, &execution).await;
        let failed = only(case, &events, row.failure_event());
        let (attempt_error, attempt_cause) = verdict(case, &failed, row.failure_details());
        assert_eq!(
            attempt_error, REFUSED_ERROR,
            "case {case:?}: expected the attempt's details to name the refusal, got {failed}"
        );
        for fragment in [
            REFUSED_TEXT,
            withheld.because(),
            function.as_str(),
            principal.as_str(),
        ] {
            assert!(
                attempt_cause.contains(fragment),
                "case {case:?}: expected the attempt's cause to carry {fragment:?}, got \
                 {attempt_cause:?}"
            );
        }

        // And it CLOSES on the terminal event, which says exactly what the terminal read
        // said — so the two readings of one failed execution cannot disagree.
        let terminal = only(case, &events, EVENT_EXECUTION_FAILED);
        assert_eq!(
            events.last().and_then(|event| event.get("type")),
            Some(&json!(EVENT_EXECUTION_FAILED)),
            "case {case:?}: expected the history to close on {EVENT_EXECUTION_FAILED}, got \
             {:?}",
            types(&events)
        );
        assert_eq!(
            verdict(case, &terminal, "executionFailedEventDetails"),
            (described_error, described_cause),
            "case {case:?}: expected the closing event to report what the terminal read \
             reports, got {terminal}"
        );
    }
}
