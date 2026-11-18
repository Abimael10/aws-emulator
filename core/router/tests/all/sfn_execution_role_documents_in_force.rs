// SPDX-License-Identifier: CC0-1.0

//! WHICH documents decide a Step Functions `Task` state's integration call, driven across
//! the three services the question spans — Step Functions decides and makes the call, IAM
//! holds the documents it is judged against, and Amazon SQS serves the call when it is
//! made — mounted on ONE engine exactly as `core/router/src/lib.rs` mounts them.
//!
//! RULE: an identity-based decision about a role this account holds is an implicit deny
//! until a document in force on that role allows the call, so a grant has to come from a
//! document: a role carrying no identity-based policy at all allows nothing, and a role
//! whose documents say nothing about the call allows it no more than one that says nothing
//! at all. Only an attachment whose document this account cannot read withholds the
//! refusal, because what such a policy grants is unknown here rather than absent.
//!
//! The rows differ along what the rule generalises over: WHERE the documents in force come
//! from (nothing at all, an inline role policy, a customer-managed policy attached, one
//! attached and then detached again, and one of AWS's own published policies) and whether
//! what they say covers the call. Each row drives the same one-`Task` state machine over a
//! queue of its own, so what the gate decided is read off the queue as well as off the
//! execution.
//!
//! `all/sfn_task_target_authorization.rs` asks which RESOURCE the access request is over,
//! and its harness — the services on one engine, the IAM caller, the role with one inline
//! policy, the queue, the state machine and the execution reader — is reached here through
//! `crate::sfn_task_target_authorization`, because this target is one crate root
//! (`tests/all/main.rs`, which declares every suite beside it).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::element;
use crate::sfn_task_target_authorization::{
    allow, emulator, iam, logs_only, machine, queue, ran, role, session_principal, string,
    trust_document, waiting,
};

/// The `Resource` every row's `Task` state names: the optimized integration that sends one
/// message to the queue its `QueueUrl` parameter addresses.
const SEND: &str = "arn:aws:states:::sqs:sendMessage";

/// The message body every row's task sends, which is what a served call leaves on the
/// queue.
const BODY: &str = "hello";

/// The IAM action the send is subject to, in the lower case Amazon SQS's own authorization
/// layer words a refusal in.
const SEND_ACTION: &str = "sqs:sendmessage";

/// The action a policy GRANTS it under, which is the name that service publishes for the
/// `SendMessage` API. IAM compares action names without regard to case, so this and
/// [`SEND_ACTION`] name one action.
const SEND_GRANT: &str = "sqs:SendMessage";

/// The error a `Task` state reports when the send was refused: Amazon SQS's base SDK
/// exception, which is what a refusal its `SendMessage` does not model is reported under.
const SEND_REFUSAL_ERROR: &str = "SQS.AmazonSQSException";

/// One of AWS's own published policies that grants every action — "AdministratorAccess",
/// whose document this emulator's IAM catalog carries, so what it grants is readable here
/// and decides the call.
const ADMINISTRATOR_ACCESS: &str = "arn:aws:iam::aws:policy/AdministratorAccess";

/// One that grants reads of another service and nothing of this call —
/// "AmazonEC2ReadOnlyAccess", from the same catalog.
const EC2_READ_ONLY: &str = "arn:aws:iam::aws:policy/AmazonEC2ReadOnlyAccess";

/// Where the documents in force on a row's role come from.
#[derive(Debug, Clone, Copy)]
enum InForce {
    /// The role carries nothing: no inline policy, no attachment. It is the position every
    /// freshly created role is in, and the position a role whose only policy was taken
    /// away is left in.
    Nothing,
    /// One INLINE role policy, which grants the send when `covers_the_send` and speaks only
    /// about this service's own log writes otherwise.
    Inline { covers_the_send: bool },
    /// One customer-managed policy, attached, with the same two contents.
    Attached { covers_the_send: bool },
    /// One customer-managed policy that DID grant the send, attached and then detached
    /// again, so the role carries nothing once more.
    Detached,
    /// One of AWS's own published policies, by ARN.
    AwsManaged(&'static str),
}

/// The statements a row's document carries: the grant for the send over the row's own
/// queue, or a statement about this service's log writes, which says nothing about any
/// integration call.
fn statements(covers_the_send: bool, queue_arn: &str) -> Value {
    let statement = match covers_the_send {
        true => allow(SEND_GRANT, queue_arn),
        false => logs_only(),
    };
    json!({ "Version": "2012-10-17", "Statement": [statement] })
}

/// A role called `name`, carrying what `in_force` says it carries, by its own ARN.
///
/// Every row's role is trusted by this service the same way; what differs is the
/// PERMISSIONS, which are provisioned through IAM's own calls.
async fn role_carrying(
    router: &MultiServiceRouter,
    name: &str,
    in_force: InForce,
    queue_arn: &str,
) -> String {
    match in_force {
        // `CreateRole` plus one inline policy is exactly what the harness's own role helper
        // does, so this row is provisioned by it rather than by a second copy of the pair.
        InForce::Inline { covers_the_send } => {
            role(router, name, &statements(covers_the_send, queue_arn)).await
        }
        InForce::Nothing => bare_role(router, name).await,
        InForce::Attached { covers_the_send } => {
            let role_arn = bare_role(router, name).await;
            let policy_arn = managed(router, name, covers_the_send, queue_arn).await;
            attach(router, name, &policy_arn).await;
            role_arn
        }
        InForce::Detached => {
            let role_arn = bare_role(router, name).await;
            let policy_arn = managed(router, name, true, queue_arn).await;
            attach(router, name, &policy_arn).await;
            detach(router, name, &policy_arn).await;
            role_arn
        }
        InForce::AwsManaged(policy_arn) => {
            let role_arn = bare_role(router, name).await;
            attach(router, name, policy_arn).await;
            role_arn
        }
    }
}

/// A role this service is trusted to act as and that carries NOTHING else: no inline
/// policy and no attachment, which is the position every freshly created role is in.
async fn bare_role(router: &MultiServiceRouter, name: &str) -> String {
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name.to_string()),
            ("AssumeRolePolicyDocument", trust_document().to_string()),
        ],
    )
    .await;
    element(&created, "Arn")
}

/// A customer-managed policy named after the role it is made for, carrying the same
/// statements an inline document of that row would.
async fn managed(
    router: &MultiServiceRouter,
    name: &str,
    covers_the_send: bool,
    queue_arn: &str,
) -> String {
    let created = iam(
        router,
        "CreatePolicy",
        &[
            ("PolicyName", format!("{name}Policy")),
            (
                "PolicyDocument",
                statements(covers_the_send, queue_arn).to_string(),
            ),
        ],
    )
    .await;
    element(&created, "Arn")
}

/// Attach the managed policy `policy_arn` to the role `name`.
async fn attach(router: &MultiServiceRouter, name: &str, policy_arn: &str) {
    iam(
        router,
        "AttachRolePolicy",
        &[
            ("RoleName", name.to_string()),
            ("PolicyArn", policy_arn.to_string()),
        ],
    )
    .await;
}

/// Take it off again, which leaves the role carrying nothing once more.
async fn detach(router: &MultiServiceRouter, name: &str, policy_arn: &str) {
    iam(
        router,
        "DetachRolePolicy",
        &[
            ("RoleName", name.to_string()),
            ("PolicyArn", policy_arn.to_string()),
        ],
    )
    .await;
}

/// One row: the case, where the documents in force come from, and whether what they say
/// covers the call.
struct Row {
    case: &'static str,
    in_force: InForce,
    granted: bool,
}

/// The rows, in the order they are driven.
const ROWS: &[Row] = &[
    Row {
        case: "a role carrying no identity-based policy at all",
        in_force: InForce::Nothing,
        granted: false,
    },
    Row {
        case: "an inline policy that says nothing about the send",
        in_force: InForce::Inline {
            covers_the_send: false,
        },
        granted: false,
    },
    Row {
        case: "an inline policy that grants the send",
        in_force: InForce::Inline {
            covers_the_send: true,
        },
        granted: true,
    },
    Row {
        case: "an attached customer-managed policy that grants the send",
        in_force: InForce::Attached {
            covers_the_send: true,
        },
        granted: true,
    },
    Row {
        case: "an attached customer-managed policy that says nothing about the send",
        in_force: InForce::Attached {
            covers_the_send: false,
        },
        granted: false,
    },
    Row {
        case: "the policy that granted the send, detached again",
        in_force: InForce::Detached,
        granted: false,
    },
    Row {
        case: "an attached AWS-managed policy that says nothing about the send",
        in_force: InForce::AwsManaged(EC2_READ_ONLY),
        granted: false,
    },
    Row {
        case: "an attached AWS-managed policy that grants every action",
        in_force: InForce::AwsManaged(ADMINISTRATOR_ACCESS),
        granted: true,
    },
];

/// A `Task` state's call is made exactly where a document in force on the execution role
/// allows it, and not where the role carries no document that does — including a role that
/// carries none at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_documents_in_force_on_the_execution_role_decide_the_call() {
    let router = emulator();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let (queue_url, queue_arn) = queue(&router, &format!("in-force-{index}")).await;
        let name = format!("InForceRole{index}");
        let role_arn = role_carrying(&router, &name, row.in_force, &queue_arn).await;
        let machine_arn = machine(
            &router,
            &format!("in-force-{index}"),
            SEND,
            &json!({ "QueueUrl": queue_url.clone(), "MessageBody": BODY }),
            &role_arn,
        )
        .await;
        let (_execution_arn, described) = ran(&router, &machine_arn, case).await;

        if row.granted {
            assert_eq!(
                described.get("status").and_then(Value::as_str),
                Some("SUCCEEDED"),
                "case {case:?}: expected the granted send to be made, got {described:?}"
            );
            assert_eq!(
                waiting(&router, &queue_url).await,
                1,
                "case {case:?}: expected the message the task sent to be on the queue"
            );
            continue;
        }

        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(SEND_REFUSAL_ERROR)),
            "case {case:?}: expected the send to be refused under Amazon SQS's own \
             exception, got {described:?}"
        );
        // The refusal is that service's own answer to an unauthorized call: it blames the
        // SESSION the call was made as, names the action in the lower case that service
        // words a refusal in, and the queue the request addressed — and it says WHY, which
        // for every refused row here is that no document in force allows the action.
        let cause = string(&described, "cause");
        let principal = session_principal(&role_arn, &cause);
        let expected = format!(
            "User: {principal} is not authorized to perform: {SEND_ACTION} on resource: \
             {queue_arn} because no identity-based policy allows the {SEND_ACTION} action "
        );
        assert!(
            cause.starts_with(&expected),
            "case {case:?}: expected the cause to open {expected:?}, got {cause:?}"
        );
        // And nothing was sent: the call no document allowed was never made.
        assert_eq!(
            waiting(&router, &queue_url).await,
            0,
            "case {case:?}: expected the queue to stay untouched"
        );
    }
}
