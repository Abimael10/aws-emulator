// SPDX-License-Identifier: CC0-1.0

//! What the state machine's EXECUTION ROLE decides about the Lambda invocation a `Task`
//! state asks for, over every FORM and every PATTERN it can ask for one in — driven over
//! each service's own wire protocol through the production `MultiServiceRouter` with Step
//! Functions, IAM and Lambda mounted on ONE engine, the shape `core/router/src/lib.rs`
//! mounts them in, so the documents the call is judged against are the ones IAM's own
//! handlers wrote and an allowed call really runs the function.
//!
//! RULE: a `Task` state's integration call is made as the state machine's `roleArn` — "the
//! Amazon Resource Name (ARN) of the IAM role to use for this state machine"
//! (`CreateStateMachine$roleArn`) — and whether the documents in force on that role allow
//! it is decided BEFORE the request goes anywhere, whatever form the `Resource` names the
//! call in and whatever integration pattern it asks for. A call no document in force allows
//! is therefore not made at all: the task fails with the invoked service's own
//! access-denied refusal, whose sentence blames the assumed-role session of that role and
//! names the action the invoked operation publishes and the ARN of the resource the request
//! addressed; nothing of the work happens, so a callback (`.waitForTaskToken`) task hands
//! no token over and its history claims no submission, and the execution settles `FAILED`
//! rather than waiting for an answer nothing will send. Where the same role IS granted the
//! invocation, the very same definition's call is made and the function's own answer
//! decides the state — which is what tells a call that was never made from one that was
//! made and failed.
//!
//! The refusal arrives under Lambda's BASE SDK exception: `Invoke` models no access-denied
//! error of its own (`smithy_query(query="operation",operation="Invoke",
//! model_path="services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/
//! com-amazonaws-lambda-awsgirapiservice.json")`), so no client carries a class for the
//! code such an answer names and the base exception is what the code deserializes into.
//!
//! The rows differ along what the rule generalises over: the form the `Task` names the
//! invocation in (the function's own ARN, the optimized `lambda:invoke` integration and the
//! AWS SDK `aws-sdk:lambda:invoke` one), the integration pattern it asks for
//! (request-response, and the callback `.waitForTaskToken` one), and what the role's
//! document says about the invocation — the granted rows being the positive controls that
//! the same call is made, and the function really invoked, when it is allowed.
//!
//! What the neighbours pin and this one does not repeat: `refused_invocation_series` holds
//! the `AWS/States` and `AWS/Lambda` series such an attempt is accounted in, and
//! `task_lifecycle_under_execution_role` the events every attempt records whatever the role
//! decided. What only this suite reaches is the decision over the PATTERN the `Resource`
//! asks the call in, and with it that a denied callback task waits for nothing.
//!
//! The harness is `lambda_task_control_flow`'s `Chain::with_iam` and the principal reader is
//! `task_refusal_session_principal`'s; `tests/all/main.rs` is this binary's crate root, so
//! both are reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    EVENT_LAMBDA_FUNCTION_FAILED, EVENT_TASK_FAILED, EVENT_TASK_SUBMITTED,
};

use crate::lambda_task_control_flow::Chain;
use crate::state_machine_lifecycle::string;
use crate::task_refusal_session_principal::session_principal;

/// The handler every row's function carries: it answers with the event it was handed, so a
/// row that reads its own answer back knows the invocation really happened and what it
/// carried — including the callback token, for a row whose `Parameters` hand it one.
const HANDLER_SOURCE: &str = r#"
def handler(event, context):
    return {"saw": event}
"#;

/// The member the handler answers under, which is what a row's reading of its answer looks
/// for.
const SAW: &str = "saw";

/// The document every row's execution is started with, and so the event a request-response
/// row's invocation is handed.
const ORDER: &str = "o-1";

/// The IAM action an invocation is subject to: the one Lambda PUBLISHES for `Invoke`, which
/// is not the operation's own name.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// An action a least-privilege orchestration role carries that says nothing about any
/// invocation, which is what a DENIED row's document grants instead.
const UNRELATED_ACTION: &str = "logs:PutLogEvents";

/// The error a `Task` state reports when the role may not make the call: Lambda's base SDK
/// exception, which is what a refusal `Invoke` models no error for arrives under.
const REFUSED_ERROR: &str = "Lambda.AWSLambdaException";

/// The fixed text of the sentence that refusal carries.
const REFUSED_TEXT: &str = "not authorized to perform: lambda:InvokeFunction";

/// The status the metadata of that sentence reports, which is the one Lambda answers an
/// unauthorized call with. Each integration family RENDERS that metadata in the style of the
/// client it is served by, so what every row is held to here is the figure both carry rather
/// than one family's spelling of the whole tail.
const REFUSED_STATUS: &str = "Status Code: 403";

/// The `output` this suite answers a granted callback row's token with, which is then the
/// execution's own.
const ANSWERED_OUTPUT: &str = r#"{"answered":"from outside the invocation"}"#;

/// How long one poll of a condition the work owes sleeps between reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a granted callback row waits for the invocation to come back: a ceiling on
/// patience, never a span anything here counts on — the invocation starts an interpreter and
/// runs a caller's own code, so how long it takes is the host's business and no row's
/// verdict.
const SUBMISSION_PATIENCE: Duration = Duration::from_secs(120);

/// Which form a `Task` state's `Resource` names the invocation in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    /// The function's OWN ARN, which is the same call the optimized integration makes.
    FunctionArn,
    /// `arn:aws:states:::lambda:invoke`, the optimized integration, whose request is the
    /// state's `Parameters`.
    Optimized,
    /// `arn:aws:states:::aws-sdk:lambda:invoke`, the AWS SDK family, which makes the same
    /// call on the same function.
    AwsSdk,
}

/// Which integration PATTERN the `Resource` asks the call in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pattern {
    /// The call answers the task: request-response.
    RequestResponse,
    /// `.waitForTaskToken`: the call carries a token, and whoever holds it answers the task.
    Callback,
}

/// One row: the case, the form and pattern the `Resource` names the call in, and whether the
/// role's document grants the invocation.
struct Row {
    case: &'static str,
    form: Form,
    pattern: Pattern,
    granted: bool,
}

impl Row {
    /// The `Resource` this row's one `Task` state carries, which for the bare form is the
    /// function's own ARN and otherwise the integration token, with the suffix the pattern
    /// asks for.
    fn resource(&self, function_arn: &str) -> String {
        let named = match self.form {
            Form::FunctionArn => return function_arn.to_string(),
            Form::Optimized => "arn:aws:states:::lambda:invoke",
            Form::AwsSdk => "arn:aws:states:::aws-sdk:lambda:invoke",
        };
        match self.pattern {
            Pattern::RequestResponse => named.to_string(),
            Pattern::Callback => format!("{named}.waitForTaskToken"),
        }
    }

    /// The event that closes this row's attempt as a failure: a `Resource` that is the
    /// function's own ARN records the LAMBDA FUNCTION family of events, and an integration
    /// token the TASK family — the same work, recorded under the family the `Resource` named
    /// it in.
    fn failure_event(&self) -> &'static str {
        match self.form {
            Form::FunctionArn => EVENT_LAMBDA_FUNCTION_FAILED,
            Form::Optimized | Form::AwsSdk => EVENT_TASK_FAILED,
        }
    }

    /// The statements the row's role carries: a grant of the invocation on the function's
    /// own ARN, or one that says nothing about any invocation at all.
    fn statements(&self, function_arn: &str) -> Value {
        match self.granted {
            true => json!([{
                "Effect": "Allow",
                "Action": INVOKE_ACTION,
                "Resource": function_arn,
            }]),
            false => json!([{
                "Effect": "Allow",
                "Action": UNRELATED_ACTION,
                "Resource": "*",
            }]),
        }
    }

    /// The one-state definition the row runs.
    fn definition(&self, function_arn: &str) -> Value {
        let task = match self.form {
            // The bare form takes no request of its own: the state's input IS the event.
            Form::FunctionArn => json!({
                "Type": "Task",
                "Resource": self.resource(function_arn),
                "End": true,
            }),
            Form::Optimized | Form::AwsSdk => {
                let mut payload = execution_input();
                if self.pattern == Pattern::Callback {
                    // The token the pattern mints for this state, out of the context
                    // object, so the invocation that receives it can be seen to have
                    // received it.
                    payload["token.$"] = json!("$$.Task.Token");
                }
                json!({
                    "Type": "Task",
                    "Resource": self.resource(function_arn),
                    "Parameters": { "FunctionName": function_arn, "Payload": payload },
                    "End": true,
                })
            }
        };
        json!({ "StartAt": "Invoke", "States": { "Invoke": task } })
    }
}

/// The rows, spanning the forms, the patterns and what the document says.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the function's own ARN, under a role that says nothing about the invocation",
            form: Form::FunctionArn,
            pattern: Pattern::RequestResponse,
            granted: false,
        },
        Row {
            case: "the function's own ARN, under a role granted the invocation",
            form: Form::FunctionArn,
            pattern: Pattern::RequestResponse,
            granted: true,
        },
        Row {
            case: "the optimized integration, under a role that says nothing about it",
            form: Form::Optimized,
            pattern: Pattern::RequestResponse,
            granted: false,
        },
        Row {
            case: "the optimized integration, under a role granted the invocation",
            form: Form::Optimized,
            pattern: Pattern::RequestResponse,
            granted: true,
        },
        Row {
            case: "the optimized integration's callback pattern, under a role that says \
                   nothing about it",
            form: Form::Optimized,
            pattern: Pattern::Callback,
            granted: false,
        },
        Row {
            case: "the optimized integration's callback pattern, under a role granted the \
                   invocation",
            form: Form::Optimized,
            pattern: Pattern::Callback,
            granted: true,
        },
        Row {
            case: "the AWS SDK integration, under a role that says nothing about it",
            form: Form::AwsSdk,
            pattern: Pattern::RequestResponse,
            granted: false,
        },
        Row {
            case: "the AWS SDK integration's callback pattern, under a role that says \
                   nothing about it",
            form: Form::AwsSdk,
            pattern: Pattern::Callback,
            granted: false,
        },
        Row {
            case: "the AWS SDK integration, under a role granted the invocation",
            form: Form::AwsSdk,
            pattern: Pattern::RequestResponse,
            granted: true,
        },
    ]
}

/// The document every row's execution carries.
fn execution_input() -> Value {
    json!({ "order": ORDER })
}

/// The events of `execution`, as `GetExecutionHistory` answers them.
async fn events(chain: &Chain, case: &str, execution: &str) -> Vec<Value> {
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

/// The event of type `named` in `execution`'s history, or `None` while the history carries
/// none.
async fn event(chain: &Chain, case: &str, execution: &str, named: &str) -> Option<Value> {
    events(chain, case, execution)
        .await
        .into_iter()
        .find(|event| event.get("type").and_then(Value::as_str) == Some(named))
}

/// The document the callback task's own call ANSWERED, as its `TaskSubmitted` event reports
/// it, waited out: the call is made before the token can be answered, so a granted row looks
/// again until the history carries the submission.
async fn submitted(chain: &Chain, case: &str, execution: &str) -> Value {
    let began = Instant::now();
    loop {
        if let Some(found) = event(chain, case, execution, EVENT_TASK_SUBMITTED).await {
            let details = found.get("taskSubmittedEventDetails").unwrap_or_else(|| {
                panic!("case {case:?}: expected the submission's details in {found}")
            });
            return serde_json::from_str(&string(details, "output")).unwrap_or_else(|why| {
                panic!("case {case:?}: the submission's output is JSON: {why}")
            });
        }
        assert!(
            began.elapsed() < SUBMISSION_PATIENCE,
            "case {case:?}: expected a {EVENT_TASK_SUBMITTED} event within \
             {SUBMISSION_PATIENCE:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// What the invocation answered, as the row's own form reports it: the function's return
/// value itself where the `Resource` named the function, and under the `Payload` member of
/// the invocation's answer for either integration family.
fn answered(form: Form, result: &Value, case: &str) -> Value {
    match form {
        Form::FunctionArn => result.clone(),
        Form::Optimized | Form::AwsSdk => result.get("Payload").cloned().unwrap_or_else(|| {
            panic!("case {case:?}: expected the invocation's Payload in {result}")
        }),
    }
}

/// Every row: the role's documents decide the invocation before it is made, whatever form and
/// pattern the `Resource` asked for it in — so a denied row's function is never invoked, its
/// callback task hands no token over, and its execution fails with the refusal, while a
/// granted row's call really runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_execution_roles_decision_is_taken_before_the_call_whatever_pattern_asks_for_it() {
    let chain = Chain::with_iam();
    let function = chain
        .given_function_of("pattern-gate", HANDLER_SOURCE, Some(20))
        .await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let role = chain
            .role(
                case,
                &format!("PatternRole{index}"),
                &row.statements(&function),
            )
            .await;
        let created = chain
            .sfn(
                case,
                "CreateStateMachine",
                &json!({
                    "name": format!("pattern-gate-{index}"),
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

        // A GRANTED callback row's call is made, so its token is handed over and the task
        // waits for an answer: the invocation's own answer carries the token it received —
        // which is what says the function really ran — and the answer this suite then sends
        // finishes the execution.
        let received = if row.granted && row.pattern == Pattern::Callback {
            let reported = submitted(&chain, case, &execution).await;
            let saw = answered(row.form, &reported, case);
            let token = saw
                .get(SAW)
                .and_then(|event| event.get("token"))
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected the token the invocation saw in {saw}")
                })
                .to_string();
            chain
                .sfn(
                    case,
                    "SendTaskSuccess",
                    &json!({ "taskToken": token, "output": ANSWERED_OUTPUT }),
                )
                .await;
            Some(saw)
        } else {
            None
        };

        let settled = chain.settled(case, &execution).await;

        if row.granted {
            assert_eq!(
                settled.get("status").and_then(Value::as_str),
                Some("SUCCEEDED"),
                "case {case:?}: expected the granted call to be made, got {settled}"
            );
            let output: Value = serde_json::from_str(&string(&settled, "output"))
                .unwrap_or_else(|why| panic!("case {case:?}: the output is JSON: {why}"));
            match received {
                // A callback row's outcome is the answer to its token; what the invocation
                // itself answered was read above, and the token it carried was this
                // state's own.
                Some(saw) => {
                    assert_eq!(
                        output,
                        serde_json::from_str::<Value>(ANSWERED_OUTPUT)
                            .expect("the output this suite sends is JSON"),
                        "case {case:?}: expected the answer the token was finished with, got \
                         {output}"
                    );
                    assert_eq!(
                        saw.get(SAW).and_then(|event| event.get("order")),
                        Some(&json!(ORDER)),
                        "case {case:?}: expected the invocation to have been handed the \
                         state's own document, got {saw}"
                    );
                }
                // A request-response row's outcome IS the invocation's answer.
                None => assert_eq!(
                    answered(row.form, &output, case),
                    json!({ SAW: execution_input() }),
                    "case {case:?}: expected the function's own answer, got {output}"
                ),
            }
            continue;
        }

        // A DENIED row: the refusal is the execution's own outcome, and it names the
        // identity the call would have been made as, the action it was subject to and the
        // function it addressed.
        assert_eq!(
            (
                settled.get("status").and_then(Value::as_str),
                settled.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(REFUSED_ERROR)),
            "case {case:?}: expected the refusal to end the execution, got {settled}"
        );
        let cause = string(&settled, "cause");
        let principal = session_principal(&role, &cause);
        for fragment in [
            REFUSED_TEXT,
            REFUSED_STATUS,
            function.as_str(),
            principal.as_str(),
        ] {
            assert!(
                cause.contains(fragment),
                "case {case:?}: expected the cause to carry {fragment:?}, got {cause:?}"
            );
        }
        // Nothing of the work happened: the attempt closed on the refusal, and no
        // submission was claimed for it — so a callback row's token went nowhere and the
        // execution waited for no answer.
        let history = events(&chain, case, &execution).await;
        let types: Vec<&str> = history
            .iter()
            .filter_map(|event| event.get("type").and_then(Value::as_str))
            .collect();
        let failed = row.failure_event();
        assert!(
            types.contains(&failed),
            "case {case:?}: expected the attempt to close with {failed}, got {types:?}"
        );
        assert!(
            !types.contains(&EVENT_TASK_SUBMITTED),
            "case {case:?}: a call that was never made claims no submission, got {types:?}"
        );
        assert!(
            !cause.contains(SAW),
            "case {case:?}: expected nothing of the function's own answer, got {cause:?}"
        );
    }
}
