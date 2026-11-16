// SPDX-License-Identifier: CC0-1.0

//! A stack REDEPLOYED through a second change set reaches a TERMINAL status, on the whole
//! assembled router.
//!
//! The rule under test: **every stack operation a change set's execution opens reaches a
//! terminal status that `DescribeStacks` publishes — `UPDATE_COMPLETE` for an `UPDATE` change
//! set whose template the emulator can deploy, `UPDATE_ROLLBACK_COMPLETE` for one whose
//! deployment cannot be resolved — and the stack takes the next change set afterwards.** It
//! is the composite the AWS CLI's `deploy` drives (CreateChangeSet, DescribeChangeSet,
//! ExecuteChangeSet, DescribeStacks) run twice over, the second time against the stack the
//! first run created.
//!
//! Two crates have to agree for that to hold — CloudFormation plans and executes the change
//! set, Amazon SQS owns the queues and is the only side that can say whether the queue the
//! second deployment ADDED really exists — so the suite lives here rather than in either
//! crate.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// How long a poll loop waits for a stack operation to reach a terminal status. Generous:
/// what a case asserts is the CONDITION (a status that is not an operation in flight), never
/// the time it took, so a loaded host cannot fail one.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
/// See [`SETTLE_DEADLINE`].
const SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request. The signature is never checked: this router runs with authentication
/// off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=deadbeef"
    )
}

/// The emulator this suite drives: the whole router over one engine, and the runtime its
/// requests are blocked on.
struct Emulator {
    router: MultiServiceRouter,
    runtime: tokio::runtime::Runtime,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let mut router = build_router(engine);
        router.set_request_authentication(false);
        Self {
            router,
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("a current-thread runtime"),
        }
    }

    /// One `awsQuery` CloudFormation action: `POST /` with the members form-encoded.
    fn cloudformation(&self, action: &str, members: &[(&str, &str)]) -> (u16, String) {
        let mut body = format!("Action={action}&Version=2010-05-15");
        for (key, value) in members {
            body.push('&');
            body.push_str(&form_encode(key));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let host = format!("cloudformation.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", host)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", authorization("cloudformation"))
            .body(bytes::Bytes::from(body))
            .expect("a well-formed request");
        let response = self.runtime.block_on(self.router.dispatch(request));
        let status = response.status().as_u16();
        (
            status,
            String::from_utf8_lossy(&response.into_body()).into_owned(),
        )
    }

    /// One Amazon SQS operation over its `awsJson1_0` wire.
    fn sqs(&self, operation: &str, request: Value) -> (u16, Value) {
        let host = format!("sqs.{REGION}.amazonaws.com");
        let http_request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", host)
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("AmazonSQS.{operation}"))
            .header("authorization", authorization("sqs"))
            .body(bytes::Bytes::from(
                serde_json::to_vec(&request).expect("the request serializes"),
            ))
            .expect("a well-formed request");
        let response = self.runtime.block_on(self.router.dispatch(http_request));
        let status = response.status().as_u16();
        let parsed = serde_json::from_slice(&response.into_body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// Poll `DescribeChangeSet` the way a client's waiter does, and answer the status the
    /// creation SETTLED on.
    fn settled_change_set(&self, case: &str, change_set: &str, stack: &str) -> String {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        let mut seen;
        loop {
            let (status, body) = self.cloudformation(
                "DescribeChangeSet",
                &[("ChangeSetName", change_set), ("StackName", stack)],
            );
            assert_eq!(
                status, 200,
                "case {case:?}: DescribeChangeSet must answer 200, got {status} with {body}"
            );
            seen = elements(&body, "Status")
                .into_iter()
                .next()
                .unwrap_or_default();
            if seen == "CREATE_COMPLETE" || seen == "FAILED" {
                return seen;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the change set {change_set} to settle, last saw {seen:?}"
            );
            std::thread::sleep(SETTLE_POLL_INTERVAL);
        }
    }

    /// Poll `DescribeStacks` the way a client's waiter does, and answer the `StackStatus` the
    /// stack SETTLED on — the first status that is not an operation in flight.
    fn settled_stack(&self, case: &str, stack: &str) -> String {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        let mut seen;
        loop {
            let (status, body) = self.cloudformation("DescribeStacks", &[("StackName", stack)]);
            assert_eq!(
                status, 200,
                "case {case:?}: DescribeStacks must answer 200, got {status} with {body}"
            );
            seen = elements(&body, "StackStatus")
                .into_iter()
                .next()
                .unwrap_or_default();
            if !seen.is_empty() && !seen.ends_with("_IN_PROGRESS") {
                return seen;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected {stack} to settle, last saw {seen:?}"
            );
            std::thread::sleep(SETTLE_POLL_INTERVAL);
        }
    }

    /// What `aws cloudformation deploy` does to one stack: create a change set, wait for its
    /// creation, execute it, and wait for the stack's terminal status — which is what this
    /// answers.
    ///
    /// The members an AWS CLI `deploy` always sends travel with it: an empty `Parameters`,
    /// `Tags` and `Capabilities` list (the query protocol serializes an empty list as the
    /// bare member name) and the `Description` it stamps on the change set.
    fn deploy(
        &self,
        case: &str,
        stack: &str,
        change_set: &str,
        change_set_type: &str,
        template: &str,
    ) -> String {
        let (status, body) = self.cloudformation(
            "CreateChangeSet",
            &[
                ("StackName", stack),
                ("ChangeSetName", change_set),
                ("ChangeSetType", change_set_type),
                ("TemplateBody", template),
                ("Parameters", ""),
                ("Tags", ""),
                ("Capabilities", ""),
                ("Description", "Created by AWS CLI"),
            ],
        );
        assert_eq!(
            status, 200,
            "case {case:?}: CreateChangeSet must answer 200, got {status} with {body}"
        );
        // A client addresses the change set by the `Id` the create answered with, which is
        // what the CLI's own waiter and its `ExecuteChangeSet` carry.
        let id = elements(&body, "Id").into_iter().next().unwrap_or_default();
        assert!(
            !id.is_empty(),
            "case {case:?}: CreateChangeSet must answer an Id, got {body}"
        );
        let created = self.settled_change_set(case, &id, stack);
        assert_eq!(
            created, "CREATE_COMPLETE",
            "case {case:?}: a change set with changes must complete its creation"
        );
        let (status, body) = self.cloudformation(
            "ExecuteChangeSet",
            &[("ChangeSetName", &id), ("StackName", stack)],
        );
        assert_eq!(
            status, 200,
            "case {case:?}: ExecuteChangeSet must answer 200, got {status} with {body}"
        );
        self.settled_stack(case, stack)
    }

    /// The queue url Amazon SQS holds under `name`, or `None` where it holds none.
    ///
    /// `None` is only ever the modelled `QueueDoesNotExist`: any other refusal is this suite's
    /// own fault rather than the answer "there is no such queue", so it fails the case instead
    /// of reading as an absence.
    fn queue_url(&self, case: &str, name: &str) -> Option<String> {
        let (status, parsed) = self.sqs("GetQueueUrl", json!({ "QueueName": name }));
        if status == 200 {
            return parsed
                .get("QueueUrl")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        let refusal = parsed
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            refusal.ends_with("QueueDoesNotExist"),
            "case {case:?}: GetQueueUrl({name}) must answer the queue or the modelled \
             QueueDoesNotExist, got {status} with {parsed}"
        );
        None
    }
}

/// A template declaring `queues`, each an `AWS::SQS::Queue` under its own name — the shape
/// the AWS CLI's `deploy` sends as `TemplateBody`.
fn template(queues: &[(&str, &str)]) -> String {
    let resources: Vec<String> = queues
        .iter()
        .map(|(logical_id, queue_name)| {
            format!(
                "\"{logical_id}\":{{\"Type\":\"AWS::SQS::Queue\",\
                 \"Properties\":{{\"QueueName\":\"{queue_name}\"}}}}"
            )
        })
        .collect();
    format!(
        "{{\"AWSTemplateFormatVersion\":\"2010-09-09\",\"Resources\":{{{}}}}}",
        resources.join(",")
    )
}

#[test]
fn a_second_deploy_onto_an_existing_stack_reaches_a_terminal_status() {
    std::thread::Builder::new()
        .name("cfn-redeploy".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let emulator = Emulator::new();
            let stack = "cfn-redeploy-settles";

            // ---- The first deploy: one queue, created from a CREATE change set. ----
            let first = template(&[("Q1", "cfn-redeploy-q1")]);
            let settled = emulator.deploy("the first deploy", stack, "cs-1", "CREATE", &first);
            assert_eq!(
                settled, "CREATE_COMPLETE",
                "the first deploy must settle CREATE_COMPLETE, got {settled}"
            );

            // ---- The second deploy: the same stack, a template that ADDS a queue. ----
            let second = template(&[("Q1", "cfn-redeploy-q1"), ("Q2", "cfn-redeploy-q2")]);
            let settled = emulator.deploy("the second deploy", stack, "cs-2", "UPDATE", &second);
            assert_eq!(
                settled, "UPDATE_COMPLETE",
                "executing an UPDATE change set must settle the stack UPDATE_COMPLETE, got \
                 {settled}"
            );
            // ...and the queue it added really exists in the service that OWNS it, which is
            // the only side that can say so.
            let added = emulator.queue_url("the second deploy", "cfn-redeploy-q2");
            assert!(
                added.is_some(),
                "the queue the second deploy added must exist in Amazon SQS, got {added:?}"
            );
            let kept = emulator.queue_url("the second deploy", "cfn-redeploy-q1");
            assert!(
                kept.is_some(),
                "the queue the first deploy created must be untouched, got {kept:?}"
            );

            // ---- A deploy whose added resource cannot be resolved rolls the update back. ----
            let broken = concat!(
                r#"{"AWSTemplateFormatVersion":"2010-09-09","Resources":{"#,
                r#""Q1":{"Type":"AWS::SQS::Queue","#,
                r#""Properties":{"QueueName":"cfn-redeploy-q1"}},"#,
                r#""Q2":{"Type":"AWS::SQS::Queue","#,
                r#""Properties":{"QueueName":"cfn-redeploy-q2"}},"#,
                r#""Q3":{"Type":"AWS::SQS::Queue","Properties":{"#,
                r#""QueueName":"cfn-redeploy-q3","#,
                r#""Tags":[{"Key":"owner","#,
                r#""Value":{"Fn::ImportValue":"cfn-redeploy-never-declared"}}]}}}}"#,
            );
            let settled = emulator.deploy("an unresolvable import", stack, "cs-3", "UPDATE", broken);
            assert_eq!(
                settled, "UPDATE_ROLLBACK_COMPLETE",
                "an update importing an export nothing publishes must settle the stack \
                 UPDATE_ROLLBACK_COMPLETE, got {settled}"
            );
            // The resource the rolled-back update would have created was never made, and the
            // ones the stack already had are untouched.
            let never_made = emulator.queue_url("an unresolvable import", "cfn-redeploy-q3");
            assert!(
                never_made.is_none(),
                "the rolled-back update must have created no queue, got {never_made:?}"
            );
            for name in ["cfn-redeploy-q1", "cfn-redeploy-q2"] {
                let held = emulator.queue_url("an unresolvable import", name);
                assert!(
                    held.is_some(),
                    "the rollback must leave {name} in place, got {held:?}"
                );
            }

            // ---- ...and the stack takes the next deploy afterwards. ----
            let fourth = template(&[
                ("Q1", "cfn-redeploy-q1"),
                ("Q2", "cfn-redeploy-q2"),
                ("Q4", "cfn-redeploy-q4"),
            ]);
            let settled = emulator.deploy("the deploy after the rollback", stack, "cs-4", "UPDATE", &fourth);
            assert_eq!(
                settled, "UPDATE_COMPLETE",
                "a valid deploy after a rolled-back one must settle UPDATE_COMPLETE, got \
                 {settled}"
            );
            let added = emulator.queue_url("the deploy after the rollback", "cfn-redeploy-q4");
            assert!(
                added.is_some(),
                "the queue the deploy after the rollback added must exist in Amazon SQS, got \
                 {added:?}"
            );
        })
        .expect("spawn the redeploy thread")
        .join()
        .expect("a second deploy onto an existing stack must reach a terminal status");
}
