// SPDX-License-Identifier: CC0-1.0

//! Which `HandlerErrorCode` class the event of a failed `AWS::Pipes::Pipe` reports, over the
//! deployment path the resource was on when it failed and over a create the owning service
//! REFUSES outright.
//!
//! RULE: the class on a failed resource's event reports WHAT became of that resource, as the
//! resource's own provider classified it — a provider that gives up on a resource the owning
//! service ACCEPTED and then settled in a state it does not serve the resource from reports its
//! own did-not-stabilize account under `NotStabilized`, on whichever path was bringing the
//! resource up (the create of a new resource, the update of one already deployed), while a call
//! that service REFUSED is reported under the class THAT refusal's own error code falls in.
//!
//! Why it matters: `HandlerErrorCode` is the machine-readable half of a failed deployment, and
//! the two kinds of failure ask a client for opposite things. A resource that was provisioned
//! and never converged is one to retry, to give a longer window, or to go and inspect — which is
//! what deployment tooling, Cloud Control API callers and CI gates branch on `NotStabilized` to
//! do — while a refused call says the declaration itself is at fault and no retry of the same
//! template can clear it. A convergence failure reported as a service fault (or the other way
//! round) mis-routes every such retry loop and every rollback-classification dashboard, even
//! where the status, the single failed event and the reason text are otherwise right.
//!
//! The rows differ along what the rule generalises over: the PATH the resource was on when its
//! provider gave up (the create of a new pipe — the table in
//! `crate::cfn_pipes_pipe_stabilisation_failure_class` — against the update of a deployed one,
//! which this suite adds) and WHY EventBridge Pipes could not bring the pipe up (a role that
//! states nothing at all about the pipe's source, and one whose wildcard allow is overridden by
//! a deny on every call its source stage makes). The second test is the OTHER side of the rule
//! on the same type: a create that service refuses outright, whose event carries the class that
//! refusal falls in rather than the class of a resource that never converged — so what the rows
//! above pin is a provider's classification of what it reported rather than one class this type
//! always gets.
//!
//! Each update row's own POSITIVE CONTROL is the deployment it starts from: the same template on
//! a role that may make every call reaches `CREATE_COMPLETE` with no failure recorded for the
//! pipe at all, so what the update then records is a change that could not stabilise rather than
//! an estate nothing can deploy.
//!
//! Four crates have to agree for this to hold — CloudFormation drives the deployment, the update
//! and the rollback, EventBridge Pipes decides whether the pipe can come up, IAM holds the roles
//! and Amazon SQS holds the queues — so the suite lives here rather than in any one of them. The
//! helpers it drives the emulator through are the create path's own, in
//! `crate::cfn_pipes_pipe_stabilisation_failure_class`.
//!
//! Every wait here looks again until the condition it needs holds, on the deadline that suite's
//! poll uses; nothing is asserted about elapsed time.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::cfn_pipes_pipe_stabilisation_failure_class::{
    build_router, cloudformation, did_not_stabilize, pipes_trust_policy, queue_arn, reasons_for,
    settled_stack, BEFORE_THE_TOKEN, CREATE_COMPLETE, CREATE_FAILED, ENVELOPE_OPENS,
    NOT_STABILIZED, PIPE, PIPE_TYPE, ROLLBACK_COMPLETE, RUNNING, WITHHELD_SOURCE_READ,
};
use super::query_wire::opt_element;

/// The statuses an update that cannot be carried out leaves behind: the resource's own, and the
/// stack's once the declaration that was in force has been put back.
const UPDATE_FAILED: &str = "UPDATE_FAILED";
const UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";

/// The `HandlerErrorCode` class a refusal of the REQUEST the declaration asked for falls in — a
/// value of the CloudFormation model's own `HandlerErrorCode` enum, and the class every
/// validation refusal of an owning service reaches the event under.
const INVALID_REQUEST: &str = "InvalidRequest";

/// Where the class is stated inside the envelope, in the same parenthesis as the request token.
const CLASS_LABEL: &str = ", HandlerErrorCode: ";

/// The logical ids of the two roles every template declares: the one that may make every call
/// its source stage makes, and the row's own, which withholds them.
const GOOD_ROLE: &str = "GoodRole";
const BAD_ROLE: &str = "BadRole";

/// The batch size a pipe that is meant to come up is configured with — one within every target's
/// cap.
const WITHIN_THE_TARGET_CAP: i64 = 1;

/// The largest batch an Amazon SQS target takes per delivery: `SendMessageBatch` accepts at most
/// ten entries per call, which is the cap EventBridge Pipes states when it refuses a larger one.
const SQS_TARGET_BATCH_CAP: i64 = 10;

/// A batch size past that cap, which is the configuration the refusal row declares.
const OVER_THE_TARGET_CAP: i64 = SQS_TARGET_BATCH_CAP + 1;

/// The statements of a role that may make every call a pipe's source stage and its delivery make
/// on anything: the role every row's create deploys the pipe on, and the one the refusal row
/// keeps, so what fails that row is its declared batch size and nothing else.
fn admits_every_call() -> Value {
    json!([{
        "Effect": "Allow",
        "Action": [
            "sqs:ReceiveMessage",
            "sqs:DeleteMessage",
            "sqs:GetQueueAttributes",
            "sqs:SendMessage",
        ],
        "Resource": "*",
    }])
}

/// One `AWS::IAM::Role` the pipe may act as, carrying `statements` as its one inline policy.
fn role(statements: Value) -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "AssumeRolePolicyDocument": pipes_trust_policy(),
            "Policies": [{
                "PolicyName": "move-records",
                "PolicyDocument": { "Version": "2012-10-17", "Statement": statements },
            }],
        },
    })
}

/// One row: the names it deploys under and how the role it names withholds the calls the pipe's
/// source stage makes.
struct Row {
    case: &'static str,
    /// The prefix every name this row declares carries, so two rows never share a resource.
    prefix: &'static str,
    /// The statements of the row's own role, built from the row's queue names: what the rule
    /// turns on is the (action, RESOURCE) pair the call that takes the source's records is made
    /// on.
    withheld: fn(&Row) -> Value,
}

impl Row {
    fn stack(&self) -> String {
        format!("{}-stack", self.prefix)
    }

    fn source(&self) -> String {
        format!("{}-source", self.prefix)
    }

    fn target(&self) -> String {
        format!("{}-target", self.prefix)
    }

    fn pipe(&self) -> String {
        format!("{}-pipe", self.prefix)
    }

    /// The template this row deploys: the two queues, both roles, and the pipe acting as the role
    /// whose logical id is `pipe_role`, polling its source `batch_size` records at a time.
    ///
    /// Both roles are declared whichever one the pipe acts as, so the ONE thing an update of this
    /// template changes is which of them it is.
    fn template(&self, pipe_role: &str, batch_size: i64) -> String {
        json!({
            "Resources": {
                "SourceQueue": {
                    "Type": "AWS::SQS::Queue",
                    "Properties": { "QueueName": self.source() },
                },
                "TargetQueue": {
                    "Type": "AWS::SQS::Queue",
                    "Properties": { "QueueName": self.target() },
                },
                GOOD_ROLE: role(admits_every_call()),
                BAD_ROLE: role((self.withheld)(self)),
                PIPE: {
                    "Type": PIPE_TYPE,
                    "Properties": {
                        "Name": self.pipe(),
                        "Source": { "Fn::GetAtt": ["SourceQueue", "Arn"] },
                        "Target": { "Fn::GetAtt": ["TargetQueue", "Arn"] },
                        "RoleArn": { "Fn::GetAtt": [pipe_role, "Arn"] },
                        "SourceParameters": { "SqsQueueParameters": { "BatchSize": batch_size } },
                        "DesiredState": RUNNING,
                    },
                },
            },
        })
        .to_string()
    }
}

/// The rows, differing in how the role the update points the pipe at withholds the calls its
/// source stage makes on the pipe's source. Each withholds EVERY one of them: a role left any
/// one is a role the pipe still comes up under, so only a role that leaves it none is a change
/// that cannot stabilise.
const ROWS: &[Row] = &[
    Row {
        case: "an update onto a role that states nothing at all about the pipe's source",
        prefix: "pipe-class-update-unstated",
        withheld: |_| json!([{ "Effect": "Allow", "Action": ["sqs:SendMessage"], "Resource": "*" }]),
    },
    Row {
        case: "an update onto a role whose wildcard allow is overridden by a deny on every \
               source call",
        prefix: "pipe-class-update-deny",
        withheld: |row| {
            json!([
                { "Effect": "Allow", "Action": ["sqs:*"], "Resource": "*" },
                {
                    "Effect": "Deny",
                    "Action": [
                        "sqs:ReceiveMessage",
                        "sqs:DeleteMessage",
                        "sqs:GetQueueAttributes",
                    ],
                    "Resource": queue_arn(&format!("{}-source", row.prefix)),
                },
            ])
        },
    },
];

/// The row the refusal test deploys: its role may make every call, and its declared batch size is
/// larger than the target takes per delivery, which is a configuration EventBridge Pipes refuses
/// rather than a pipe it accepts and cannot bring up.
const REFUSED: Row = Row {
    case: "a create whose declared batch size is larger than the target takes per delivery",
    prefix: "pipe-class-refused",
    withheld: |_| admits_every_call(),
};

/// The sentence EventBridge Pipes refuses such a configuration with, which CloudFormation quotes:
/// the batch size the declaration asked for, the cap the target accepts and the target it was
/// proved of. Its own table is
/// `services/service-com-amazonaws-pipes-pipes/tests/all/pipe_configuration_rules.rs`; it is
/// written out again here because what this suite asserts is the text CloudFormation QUOTES.
fn over_the_cap_refusal() -> String {
    format!(
        "The configured batch size {OVER_THE_TARGET_CAP} is greater than the max supported batch \
         size {SQS_TARGET_BATCH_CAP} for target sqs."
    )
}

/// The three parts of the one envelope CloudFormation reports a resource provider's answer in:
/// the message it quoted, the token of the resource request that answer belongs to, and the
/// `HandlerErrorCode` class the answer falls in.
fn envelope_parts(case: &str, reason: &str) -> (String, String, String) {
    let (head, tail) = reason
        .rsplit_once(BEFORE_THE_TOKEN)
        .unwrap_or_else(|| panic!("case {case:?}: no request token in {reason:?}"));
    let quoted = head
        .strip_prefix(ENVELOPE_OPENS)
        .unwrap_or_else(|| panic!("case {case:?}: expected the provider envelope, got {reason:?}"));
    let (token, class) = tail
        .trim_end_matches(')')
        .split_once(CLASS_LABEL)
        .unwrap_or_else(|| panic!("case {case:?}: no HandlerErrorCode in {reason:?}"));
    (quoted.to_string(), token.to_string(), class.to_string())
}

/// The ONE reason recorded for the pipe's resource in `status`, with the parts of its envelope
/// read off it: a resource fails once, and a second event reporting the same failure would be a
/// second report of one answer.
async fn the_one_failure(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    status: &str,
) -> (String, String, String) {
    let failures = reasons_for(router, case, stack, PIPE, status).await;
    assert_eq!(
        failures.len(),
        1,
        "case {case:?}: expected exactly one {status} event for {PIPE}, got {failures:?}"
    );
    let reason = failures.first().cloned().unwrap_or_default();
    let (quoted, token, class) = envelope_parts(case, &reason);
    assert!(
        !quoted.contains(ENVELOPE_OPENS),
        "case {case:?}: expected the provider's answer to be quoted ONCE, got {quoted:?}"
    );
    assert!(
        !token.is_empty(),
        "case {case:?}: expected the resource request's own token, got {reason:?}"
    );
    (quoted, token, class)
}

/// Create a stack and poll it to the status it settles in, answering the status it reported.
async fn deploy(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) -> String {
    let (status, created) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack answered {status} {created}"
    );
    let described = settled_stack(router, case, stack).await;
    opt_element(&described, "StackStatus").unwrap_or_default()
}

/// RULE, every row: a pipe that deployed and then could not be CHANGED into the declaration the
/// update asked for records exactly one `UPDATE_FAILED` event, carrying the provider's
/// did-not-stabilize report inside the handler envelope and closing with the class a resource
/// that did not stabilize is reported under — the same class the create path reports, because
/// what decides it is the provider's own account of the resource rather than which call was being
/// carried out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_update_a_pipe_cannot_stabilise_under_fails_as_not_stabilized() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let stack = row.stack();

        // The control this row starts from: the same template on the role that may make every
        // call deploys, and nothing is recorded as failed for the pipe.
        let reported = deploy(
            &router,
            case,
            &stack,
            &row.template(GOOD_ROLE, WITHIN_THE_TARGET_CAP),
        )
        .await;
        assert_eq!(
            reported, CREATE_COMPLETE,
            "case {case:?}: expected the deployment the update starts from to reach \
             {CREATE_COMPLETE}, got {reported:?} with the pipe's failures {:?}",
            reasons_for(&router, case, &stack, PIPE, CREATE_FAILED).await
        );
        let before = reasons_for(&router, case, &stack, PIPE, CREATE_FAILED).await;
        assert!(
            before.is_empty(),
            "case {case:?}: expected the deployed pipe to record no failure, got {before:?}"
        );

        // The update: the same template with the pipe acting as the row's own role.
        let (status, updated) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &row.template(BAD_ROLE, WITHIN_THE_TARGET_CAP)),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateStack answered {status} {updated}"
        );
        let described = settled_stack(&router, case, &stack).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            reported, UPDATE_ROLLBACK_COMPLETE,
            "case {case:?}: expected the update the pipe could not stabilise under to roll back \
             to {UPDATE_ROLLBACK_COMPLETE}, got {reported:?} with the pipe's failures {:?}",
            reasons_for(&router, case, &stack, PIPE, UPDATE_FAILED).await
        );

        let (quoted, _, class) = the_one_failure(&router, case, &stack, UPDATE_FAILED).await;
        assert_eq!(
            quoted,
            did_not_stabilize(&row.pipe(), WITHHELD_SOURCE_READ),
            "case {case:?}: expected the provider's did-not-stabilize report, got {quoted:?}"
        );
        assert_eq!(
            class, NOT_STABILIZED,
            "case {case:?}: expected a resource that did not stabilize to be classified \
             {NOT_STABILIZED}, got {class:?} around {quoted:?}"
        );
    }
}

/// The other side of the rule on the same type: a create the owning service REFUSES is reported
/// in the same envelope under the class THAT refusal's own error code falls in, quoting that
/// service's own sentence — so the class says which kind of failure it was rather than naming the
/// one kind this type's provider also reports.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_create_the_owning_service_refuses_carries_that_refusals_class() {
    let router = build_router();
    let case = REFUSED.case;
    let stack = REFUSED.stack();

    let reported = deploy(
        &router,
        case,
        &stack,
        &REFUSED.template(GOOD_ROLE, OVER_THE_TARGET_CAP),
    )
    .await;
    assert_eq!(
        reported, ROLLBACK_COMPLETE,
        "case {case:?}: expected the stack of a refused create to roll back to \
         {ROLLBACK_COMPLETE}, got {reported:?} with the pipe's failures {:?}",
        reasons_for(&router, case, &stack, PIPE, CREATE_FAILED).await
    );

    let (quoted, _, class) = the_one_failure(&router, case, &stack, CREATE_FAILED).await;
    assert!(
        quoted.contains(&over_the_cap_refusal()),
        "case {case:?}: expected the refusal to carry that service's own sentence {:?}, got \
         {quoted:?}",
        over_the_cap_refusal()
    );
    assert_eq!(
        class, INVALID_REQUEST,
        "case {case:?}: expected the class the refusal's own code falls in, got {class:?} around \
         {quoted:?}"
    );
    assert_ne!(
        class, NOT_STABILIZED,
        "case {case:?}: expected a REFUSED call not to be reported as a resource that did not \
         stabilize, got {quoted:?}"
    );
}
