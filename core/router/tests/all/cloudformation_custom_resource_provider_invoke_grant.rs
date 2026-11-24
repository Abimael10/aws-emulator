// SPDX-License-Identifier: CC0-1.0

//! Reaching a CUSTOM RESOURCE's provider is a call CLOUDFORMATION makes with the stack's
//! service role, and that role has to be ALLOWED it.
//!
//! RULE under test: **the request a custom resource's `ServiceToken` is sent is delivered by a
//! call CloudFormation itself makes with the credentials of the stack's service role
//! (`CreateStackInput$RoleARN`: "CloudFormation uses the role's credentials to make calls on
//! your behalf"), authorized under IAM's own default deny against the ARN that token names and
//! for the action reaching the provider over that transport performs — `lambda:InvokeFunction`
//! for a function, `sns:Publish` for a topic. A role whose identity policies do not allow it
//! reaches no provider at all: the resource takes `CREATE_FAILED` with the sentence AWS words
//! an unauthorized call with, which NAMES the action, so a client can tell a permissions
//! problem on the `ServiceToken` from a provider that answered `FAILED` — and the failed create
//! is rolled back like any other.**
//!
//! Three crates have to agree for that to hold, which is why the suite lives here:
//! CloudFormation decides the call and sends the request, IAM holds the service role whose
//! policies decide it, and AWS Lambda (or Amazon SNS) is what the token names.
//!
//! The denied table's rows differ along what the rule generalises over: which transport the
//! token names, and so which action is asked for; which of IAM's rules withheld the call (no
//! statement about it at all, an explicit `Deny`); and what the statements name as their
//! `Resource` (every resource, the provider's exact ARN, another provider's ARN). Each row's
//! role is also allowed one NON-invoking action of the provider's own service, because the
//! provider itself is a resource the same stack creates there and a deployment's calls to a
//! service its role may act in no way at all are refused for want of a grant — so a role
//! allowed nothing there would fail the provider instead of reaching the call under test.
//!
//! The controls are the second test: the same template under roles that DO allow the call —
//! scoped to the provider's exact ARN, by a wildcard action and resource, by patterns the call
//! matches — and a stack that names no service role at all reach `CREATE_COMPLETE` on their
//! provider's own answer, so no row passes against a gate that refuses every provider call.
//!
//! Nothing here sleeps on a service's behalf: every wait is a poll for the CONDITION, under the
//! shared waiter's generous deadline.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_failed_custom_resource_rollback_deletes::resource_events;
use crate::query_wire::{element, opt_element, outputs_of};
use crate::sns_lambda_subscription_permission_boundary::{
    build_router, cloudformation, iam, trust_policy, ACCOUNT, CREATE_COMPLETE,
};

/// The Region every stack here is deployed in.
const REGION: &str = "us-east-1";

/// How long a status the clock owes is waited for, and how often it is looked at. The wait is
/// on the CONDITION — a status no operation is still in flight in — and the deadline is far
/// longer than the spans this emulator gives these transitions, so a loaded host cannot decide
/// a row; nothing here sleeps a span on a service's behalf and nothing asserts on how long one
/// took.
const SETTLE_DEADLINE: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(100);

/// The terminal status a failed create rolls back to.
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The resource status a resource whose provider was never reached takes.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The logical ids every template declares its custom resource and its provider under.
const CUSTOM: &str = "Provisioned";
const PROVIDER: &str = "Provider";

/// The execution role the provider functions run as, created in IAM before any stack.
const EXECUTION_ROLE: &str = "custom-resource-provider-exec";

/// The marker a declaration carries, which the provider echoes back as the `Data` the template
/// reads — so a completed row proves the provider really ran.
const MARKER: &str = "reached the provider";

/// The role session CloudFormation's own calls are made under, which is what an authorization
/// refusal names the caller by: `arn:<partition>:sts::<account>:assumed-role/<role>/<session>`.
const SESSION: &str = "AWSCloudFormation";

/// The `HandlerErrorCode` class an unauthorized call is reported under.
const ACCESS_DENIED: &str = "AccessDenied";

/// A provider that answers through the response module CloudFormation supplies to inline code:
/// it echoes the declaration's marker, which is what the stack then publishes.
const PROVIDER_SOURCE: &str = r#"
import cfnresponse


def handler(event, context):
    cfnresponse.send(
        event,
        context,
        cfnresponse.SUCCESS,
        {"Echo": event["ResourceProperties"].get("Marker", "")},
        physicalResourceId="provisioned-by-the-provider",
    )
"#;

/// Which transport a row's `ServiceToken` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    /// An AWS Lambda function, invoked with the request as its event.
    Function,
    /// An Amazon SNS topic, which the request is published to.
    Topic,
}

impl Transport {
    /// The IAM action reaching a provider over this transport performs: `Invoke` "requires
    /// permission for the `lambda:InvokeFunction` action", and publishing a message to a topic
    /// requires `sns:Publish`.
    fn action(self) -> &'static str {
        match self {
            Self::Function => "lambda:InvokeFunction",
            Self::Topic => "sns:Publish",
        }
    }

    /// The ARN the provider of `stack` is reached at: what that stack's `ServiceToken` resolves
    /// to, and what a policy statement names the provider by.
    fn provider_arn(self, stack: &str) -> String {
        match self {
            Self::Function => format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{stack}-fn"),
            Self::Topic => format!("arn:aws:sns:{REGION}:{ACCOUNT}:{stack}-topic"),
        }
    }
}

/// The trust policy a stack's service role must carry: CloudFormation has to be able to assume
/// it at all before its permissions decide anything.
fn trusts_cloudformation() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "cloudformation.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// A service role named `name`, trusting CloudFormation, carrying `statements` as its one
/// inline permissions policy where the row states any and the managed policy `attached` where
/// it names one; answered with the ARN IAM persisted for it.
async fn service_role(
    router: &MultiServiceRouter,
    name: &str,
    statements: Option<Value>,
    attached: Option<&str>,
) -> String {
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trusts_cloudformation()),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    assert!(
        arn.starts_with("arn:"),
        "setup: CreateRole({name:?}) must answer an ARN, got {created}"
    );
    if let Some(statements) = statements {
        let document = json!({"Version": "2012-10-17", "Statement": statements}).to_string();
        iam(
            router,
            "PutRolePolicy",
            &[
                ("RoleName", name),
                ("PolicyName", "what-the-stack-may-do"),
                ("PolicyDocument", &document),
            ],
        )
        .await;
    }
    if let Some(policy_arn) = attached {
        let attachment = iam(
            router,
            "AttachRolePolicy",
            &[("RoleName", name), ("PolicyArn", policy_arn)],
        )
        .await;
        let listed = iam(router, "ListAttachedRolePolicies", &[("RoleName", name)]).await;
        assert!(
            listed.contains(policy_arn),
            "setup: {policy_arn:?} must be attached to {name:?}, got {attachment} then {listed}"
        );
    }
    arn
}

/// The execution role the provider functions run as, answered by its ARN.
async fn execution_role(router: &MultiServiceRouter) -> String {
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    assert!(
        arn.starts_with("arn:"),
        "setup: CreateRole({EXECUTION_ROLE:?}) must answer an ARN, got {created}"
    );
    arn
}

/// The template one row deploys: the provider its transport names, and the custom resource that
/// provider backs, publishing the provider's own answer as an output.
fn template(transport: Transport, stack: &str, execution_role_arn: &str) -> String {
    let (provider, service_token) = match transport {
        Transport::Function => (
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": format!("{stack}-fn"),
                    "Code": { "ZipFile": PROVIDER_SOURCE },
                    "Handler": "index.handler",
                    "Role": execution_role_arn,
                    "Runtime": "python3.12",
                },
            }),
            json!({ "Fn::GetAtt": [PROVIDER, "Arn"] }),
        ),
        // "Ref returns the topic ARN" for an `AWS::SNS::Topic`.
        Transport::Topic => (
            json!({
                "Type": "AWS::SNS::Topic",
                "Properties": { "TopicName": format!("{stack}-topic") },
            }),
            json!({ "Ref": PROVIDER }),
        ),
    };
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            PROVIDER: provider,
            CUSTOM: {
                "Type": "Custom::Provisioned",
                "DependsOn": [PROVIDER],
                "Properties": {
                    "ServiceToken": service_token,
                    "Marker": MARKER,
                },
            },
        },
        "Outputs": {
            "Echo": { "Value": { "Fn::GetAtt": [CUSTOM, "Echo"] } },
        },
    })
    .to_string()
}

/// The sentence AWS refuses an unauthorized call with: the session of the stack's service role,
/// the action it asked for, the resource it asked for, and which of IAM's rules withheld the
/// grant.
fn not_authorized(role: &str, action: &str, resource: &str, clause: &str) -> String {
    format!(
        "User: arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{SESSION} is not authorized to \
         perform: {action} on resource: {resource} {clause}"
    )
}

/// The clause for a call no statement of the role's is about at all: IAM's default deny.
fn no_grant(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The clause for a call an explicit `Deny` refuses.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// Poll `stack` until it reports a status no operation is still in flight in, and answer that
/// status with the `DescribeStacks` answer it was read from.
///
/// The waiter of this suite: every status a row asserts is owed at an instant CloudFormation
/// records, so what is waited FOR is that status appearing, under [`SETTLE_DEADLINE`].
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let body = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with("_IN_PROGRESS") {
            return (reported, body);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {stack:?} to settle, last reported {reported:?} with {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// One row of the denied table: the transport its `ServiceToken` names, the statements its
/// service role carries, and which of IAM's rules withheld the call.
struct Denied {
    case: &'static str,
    stack: &'static str,
    transport: Transport,
    /// The statements of the role's one inline policy, built from the provider's own ARN so a
    /// row can scope a statement to it.
    statements: fn(&str) -> Value,
    /// Whether an explicit `Deny` is what refuses the call, rather than the absence of a grant.
    explicit: bool,
}

const DENIED: &[Denied] = &[
    Denied {
        case: "a role whose statements say nothing about invoking the provider",
        stack: "cr-grant-silent",
        transport: Transport::Function,
        statements: |_| {
            json!([
                { "Effect": "Allow", "Action": "sns:*", "Resource": "*" },
                { "Effect": "Allow", "Action": "lambda:CreateFunction", "Resource": "*" },
            ])
        },
        explicit: false,
    },
    Denied {
        case: "a role allowed everything and explicitly denied the invoke",
        stack: "cr-grant-denied",
        transport: Transport::Function,
        statements: |_| {
            json!([
                { "Effect": "Allow", "Action": "*", "Resource": "*" },
                { "Effect": "Deny", "Action": "lambda:InvokeFunction", "Resource": "*" },
            ])
        },
        explicit: true,
    },
    Denied {
        case: "a role allowed the invoke on ANOTHER function",
        stack: "cr-grant-elsewhere",
        transport: Transport::Function,
        statements: |_| {
            json!([{
                "Effect": "Allow",
                "Action": "lambda:InvokeFunction",
                "Resource": format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:someone-else"),
            }])
        },
        explicit: false,
    },
    // The other transport, where the action the call performs is Amazon SNS's: the rule is the
    // same, and so is the sentence.
    Denied {
        case: "a topic provider the role carries no publish grant for",
        stack: "cr-grant-topic-silent",
        transport: Transport::Topic,
        statements: |_| {
            json!([
                { "Effect": "Allow", "Action": "lambda:*", "Resource": "*" },
                { "Effect": "Allow", "Action": "sns:CreateTopic", "Resource": "*" },
            ])
        },
        explicit: false,
    },
    Denied {
        case: "a topic provider the role is denied on that topic's own ARN",
        stack: "cr-grant-topic-denied",
        transport: Transport::Topic,
        statements: |topic| {
            json!([
                { "Effect": "Allow", "Action": "*", "Resource": "*" },
                { "Effect": "Deny", "Action": "sns:Publish", "Resource": topic },
            ])
        },
        explicit: true,
    },
];

/// What one row of the allowed table hands the stack: the shape of the grant its service role
/// carries, or no service role at all.
enum Grant {
    /// No `RoleARN`: there is no assumed role for this gate to decide the call on.
    NoRole,
    /// An inline permissions policy carrying these statements, built from the provider's own
    /// ARN so a row can scope a statement to it.
    Inline(fn(&str) -> Value),
    /// An attached AWS-managed policy whose document this emulator holds nothing for — a grant
    /// that cannot be read, so the absence of a readable one is no evidence against the call.
    UnreadableAttachment(&'static str),
}

/// One row of the allowed table: how the role's grant is spelled.
struct Allowed {
    case: &'static str,
    stack: &'static str,
    grant: Grant,
}

/// An AWS-managed policy that grants `lambda:InvokeFunction` and whose document this emulator's
/// managed-policy catalog does not carry — the catalog is a subset of AWS's, so an ARN under a
/// name it does not hold is taken as published and attaches, and what it grants cannot be read.
///
/// SOURCE: the AWS Managed Policy Reference, which publishes `AWSLambda_FullAccess` at the
/// account root and includes `lambda:InvokeFunction` in it.
const UNCARRIED_INVOKE_POLICY: &str = "arn:aws:iam::aws:policy/AWSLambda_FullAccess";

const ALLOWED: &[Allowed] = &[
    Allowed {
        case: "an allow scoped to the provider's exact ARN",
        stack: "cr-grant-exact",
        grant: Grant::Inline(|function| {
            json!([{
                "Effect": "Allow",
                "Action": "lambda:InvokeFunction",
                "Resource": function,
            }])
        }),
    },
    Allowed {
        case: "an allow of every action on every resource",
        stack: "cr-grant-wildcard",
        grant: Grant::Inline(|_| json!([{ "Effect": "Allow", "Action": "*", "Resource": "*" }])),
    },
    Allowed {
        case: "an allow whose action and ARN are patterns the call matches",
        stack: "cr-grant-pattern",
        grant: Grant::Inline(|_| {
            json!([{
                "Effect": "Allow",
                "Action": "lambda:Invoke*",
                "Resource": format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:*"),
            }])
        }),
    },
    // A role whose only grant is an attachment this emulator holds no document for: refusing
    // for want of a grant it cannot read would refuse a call AWS permits.
    Allowed {
        case: "a grant that comes from a managed policy this emulator cannot read",
        stack: "cr-grant-unreadable",
        grant: Grant::UnreadableAttachment(UNCARRIED_INVOKE_POLICY),
    },
    // The control for the gate itself: a stack that names no service role has no assumed role
    // to decide the call on, so nothing here judges it.
    Allowed {
        case: "a stack that names no service role at all",
        stack: "cr-grant-roleless",
        grant: Grant::NoRole,
    },
];

/// Deploy one stack, under the service role `role_arn` where the row names one.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    body: &str,
    role_arn: Option<&str>,
) {
    let mut members = vec![("StackName", stack), ("TemplateBody", body)];
    if let Some(role_arn) = role_arn {
        members.push(("RoleARN", role_arn));
    }
    let accepted = cloudformation(router, "CreateStack", &members).await;
    assert!(
        accepted.contains("<StackId>"),
        "case {case:?}: expected CreateStack to be accepted with a stack id, got {accepted}"
    );
}

#[tokio::test]
async fn a_provider_the_service_role_may_not_reach_fails_the_resource_naming_the_action() {
    for row in DENIED {
        let case = row.case;
        let router = build_router();
        let execution_role_arn = execution_role(&router).await;
        let provider_arn = row.transport.provider_arn(row.stack);
        let role_arn = service_role(
            &router,
            row.stack,
            Some((row.statements)(&provider_arn)),
            None,
        )
        .await;
        let body = template(row.transport, row.stack, &execution_role_arn);
        deploy(&router, case, row.stack, &body, Some(&role_arn)).await;

        // The failed create is rolled back like any other, so the stack settles in the clean
        // terminal status a client retries from.
        let (reported, described) = settled(&router, case, row.stack).await;
        assert_eq!(
            reported, ROLLBACK_COMPLETE,
            "case {case:?}: expected the stack to settle at {ROLLBACK_COMPLETE}, got {reported:?} \
             with {described}"
        );

        // ...and what the resource's own event reports is the unauthorized call: the session
        // the request would have been made as, the action reaching the provider performs, the
        // ARN the `ServiceToken` named, and why it was withheld.
        let action = row.transport.action();
        let clause = if row.explicit {
            EXPLICIT_DENY.to_string()
        } else {
            no_grant(action)
        };
        let expected = not_authorized(row.stack, action, &provider_arn, &clause);
        let events = resource_events(&router, row.stack, CUSTOM).await;
        assert!(
            events.iter().any(|(status, reason)| status == CREATE_FAILED
                && reason.contains(&expected)
                && reason.contains(ACCESS_DENIED)),
            "case {case:?}: expected a {CREATE_FAILED} event reporting {expected:?} under \
             {ACCESS_DENIED}, got {events:?}"
        );
    }
}

#[tokio::test]
async fn a_provider_the_service_role_is_allowed_is_reached_and_the_stack_settles_on_its_answer() {
    for row in ALLOWED {
        let case = row.case;
        let router = build_router();
        let execution_role_arn = execution_role(&router).await;
        let provider_arn = Transport::Function.provider_arn(row.stack);
        let role_arn = match &row.grant {
            Grant::NoRole => None,
            Grant::Inline(statements) => {
                Some(service_role(&router, row.stack, Some(statements(&provider_arn)), None).await)
            }
            Grant::UnreadableAttachment(policy_arn) => {
                Some(service_role(&router, row.stack, None, Some(policy_arn)).await)
            }
        };
        let body = template(Transport::Function, row.stack, &execution_role_arn);
        deploy(&router, case, row.stack, &body, role_arn.as_deref()).await;

        let (reported, described) = settled(&router, case, row.stack).await;
        if reported != CREATE_COMPLETE {
            let events = resource_events(&router, row.stack, CUSTOM).await;
            panic!(
                "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {reported:?}; \
                 the resource recorded {events:?}; last answer {described}"
            );
        }
        // The provider really ran: what the stack publishes is the `Data` its answer carried.
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get("Echo").map(String::as_str),
            Some(MARKER),
            "case {case:?}: expected Fn::GetAtt to resolve to the answered Data, got {outputs:?}"
        );
    }
}
