// SPDX-License-Identifier: CC0-1.0

//! WHICH GRANT admits an Application Load Balancer's invocation of a `lambda` target group's
//! function, over the address the load balancer publishes.
//!
//! The rule under test: **a load balancer invokes a `lambda` target group's function as its
//! own service principal, so the invocation is admitted only by the FUNCTION's resource-based
//! policy, read on EVERY request: a statement that allows `lambda:InvokeFunction` to
//! `elasticloadbalancing.amazonaws.com` and whose conditions hold for the forwarded request —
//! the TARGET GROUP's ARN as `aws:SourceArn` and the invoking account as `aws:SourceAccount`.
//! A request no such statement admits is answered with the load balancer's own HTTP 502 and
//! the function is NOT invoked; a request one admits is served from the function's own
//! response, with no re-registration of the target.**
//!
//! `AddPermission$Principal` is what names the grantee — "for Amazon Web Services services,
//! the principal is a domain-style identifier that the service defines… If you specify a
//! service, use `SourceArn` or `SourceAccount` to limit who can invoke the function" — and
//! `AddPermission$SourceArn` is the invoking resource the grant is narrowed to, compared as an
//! ARN ("Lambda configures the comparison using the `StringLike` operator"), which is why a
//! wildcard in it matches. `lambda:InvokeFunction` is the action an invocation performs
//! (`Invoke` "requires permission for the `lambda:InvokeFunction` action"), and
//! `TargetGroup$TargetType`'s `lambda` ("Register a single Lambda function as a target") with
//! `TargetDescription$Id` ("the ARN of the Lambda function") is the registration the forward
//! resolves to. The 502 a load balancer answers for an invocation it may not make is recorded
//! behaviour.
//!
//! What this suite drives that its siblings do not: they span the STATE of one documented
//! grant (in place, withdrawn, restored — [`crate::alb_lambda_target`],
//! [`crate::alb_lambda_target_access_log`]); the rows here span the SHAPE of the grant itself,
//! which is what the rule generalises over — the principal it is written for, the action it
//! allows, and what it is narrowed by (this target group, another target group, a wildcard
//! over them, the invoking account, another account, an organization, nothing at all) —
//! because every one of those decides the same request.
//!
//! Every row drives the real wire path: a connection to the authority the load balancer
//! published, speaking HTTP/1.1 with no AWS credential, as a client of that address does. The
//! function is a real deployment package created through Lambda's OWN API and run by the
//! Lambda emulator mounted on this suite's engine, and each grant is written and withdrawn
//! through Lambda's own `AddPermission` / `RemovePermission`. That the function ran — or did
//! not — is asserted where nothing of this service can fake it: the handler reports every
//! invocation to an Amazon SQS queue through its own SDK before it decides what to return, so
//! a refused row leaves the queue empty and a served row leaves exactly one report naming the
//! row that caused it.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::alb_data_plane::{create_load_balancer, settle};
use crate::alb_lambda_target::emulator;
use crate::alb_lambda_target_published_address::{
    activate_functions, exchange, lambda, package, reports, sqs, HANDLER, RUNTIME, TARGET_BODY,
};
use crate::provisioning_loop::{account, action, call, p};
use crate::target_group_attributes_and_modification::create_target_group;

/// The function every row forwards to, and the queue its handler reports each invocation to.
const FUNCTION: &str = "alb-lambda-invoke-grant";
const SIDE_CHANNEL: &str = "alb-lambda-invoke-grant-side-channel";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern. No IAM is
/// mounted here, so nothing is known about it and nothing is refused for it: what admits the
/// handler's OWN call to the side channel is not what this suite is about, and the rows'
/// verdicts say nothing about it. What they are about is the grant on the FUNCTION, which is
/// what admits the LOAD BALANCER's invocation of it.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The load balancer, the target group its listener forwards to, and the SECOND group — a
/// group of this same account the function is NOT reached through — whose ARN the
/// wrongly-narrowed row grants to.
const LOAD_BALANCER: &str = "lambda-grant-alb";
const TARGET_GROUP: &str = "lambda-grant-tg";
const OTHER_TARGET_GROUP: &str = "lambda-grant-other-tg";

/// The service principal a load balancer invokes a function as, and the action an invocation
/// performs.
const SERVICE_PRINCIPAL: &str = "elasticloadbalancing.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The status a load balancer answers a request whose invocation the function's policy admits
/// no load balancer to make.
const BAD_GATEWAY: u16 = 502;

/// How long a row waits for the report of an invocation that was made. Generous on purpose:
/// everything waited for happens in this process, so neither a loaded host nor a run that
/// drives every suite of the workspace at once decides a verdict. It is a deadline to FAIL at,
/// not a latency this suite asserts.
const BUDGET: Duration = Duration::from_secs(120);

/// One row: the grant the function's resource policy carries when the request is made, and
/// what the load balancer must answer for it.
struct Row {
    case: &'static str,
    /// The statement `AddPermission` writes, as the body a client sends it with, minus the
    /// `StatementId` the row is given. `None` for the row whose function carries no
    /// resource-based policy at all.
    grant: Option<Value>,
    /// The status the load balancer must answer.
    status: u16,
    /// Whether the function must have RUN for this row.
    invoked: bool,
}

/// The rows: one per SHAPE of grant, spanning the principal it is written for, the action it
/// allows and what it is narrowed by. `group` is the target group the listener forwards to,
/// `other` a second group of the same account, and `wildcard` an ARN pattern covering the
/// first group — the form `AddPermission$SourceArn`'s documented `StringLike` comparison
/// admits.
fn rows(group: &str, other: &str, wildcard: &str) -> Vec<Row> {
    vec![
        Row {
            case: "no resource-based policy at all",
            grant: None,
            status: BAD_GATEWAY,
            invoked: false,
        },
        Row {
            case: "the documented grant, narrowed to the group the request is forwarded to",
            grant: Some(json!({
                "Action": INVOKE_ACTION,
                "Principal": SERVICE_PRINCIPAL,
                "SourceArn": group,
            })),
            status: 200,
            invoked: true,
        },
        Row {
            case: "a grant narrowed to another target group of the same account",
            grant: Some(json!({
                "Action": INVOKE_ACTION,
                "Principal": SERVICE_PRINCIPAL,
                "SourceArn": other,
            })),
            status: BAD_GATEWAY,
            invoked: false,
        },
        Row {
            case: "a grant narrowed by an ARN pattern that covers the group",
            grant: Some(json!({
                "Action": INVOKE_ACTION,
                "Principal": SERVICE_PRINCIPAL,
                "SourceArn": wildcard,
            })),
            status: 200,
            invoked: true,
        },
        Row {
            case: "a grant narrowed to no invoking resource at all",
            grant: Some(json!({
                "Action": INVOKE_ACTION,
                "Principal": SERVICE_PRINCIPAL,
            })),
            status: 200,
            invoked: true,
        },
        Row {
            case: "a grant written for another service principal",
            grant: Some(json!({
                "Action": INVOKE_ACTION,
                "Principal": "apigateway.amazonaws.com",
                "SourceArn": group,
            })),
            status: BAD_GATEWAY,
            invoked: false,
        },
        Row {
            case: "a grant of another action of the same service",
            grant: Some(json!({
                "Action": "lambda:GetFunction",
                "Principal": SERVICE_PRINCIPAL,
                "SourceArn": group,
            })),
            status: BAD_GATEWAY,
            invoked: false,
        },
        Row {
            case: "a grant of every action of the service",
            grant: Some(json!({
                "Action": "lambda:*",
                "Principal": SERVICE_PRINCIPAL,
                "SourceArn": group,
            })),
            status: 200,
            invoked: true,
        },
        Row {
            case: "a grant narrowed to the account the load balancer belongs to",
            grant: Some(json!({
                "Action": INVOKE_ACTION,
                "Principal": SERVICE_PRINCIPAL,
                "SourceArn": group,
                "SourceAccount": account(),
            })),
            status: 200,
            invoked: true,
        },
        Row {
            case: "a grant narrowed to another account",
            grant: Some(json!({
                "Action": INVOKE_ACTION,
                "Principal": SERVICE_PRINCIPAL,
                "SourceArn": group,
                // A well-formed account id of twelve digits (`SourceOwner`, pattern
                // `^\d{12}$`) that is not the one this emulator's resources belong to.
                "SourceAccount": "210987654321",
            })),
            status: BAD_GATEWAY,
            invoked: false,
        },
        Row {
            case: "a grant narrowed to an organization",
            grant: Some(json!({
                "Action": INVOKE_ACTION,
                "Principal": SERVICE_PRINCIPAL,
                "SourceArn": group,
                // `PrincipalOrgID`, pattern `^o-[a-z0-9]{10,32}$`.
                "PrincipalOrgID": "o-abcdefghij",
            })),
            status: BAD_GATEWAY,
            invoked: false,
        },
        Row {
            case: "the documented grant again, after every refused shape: served as before",
            grant: Some(json!({
                "Action": INVOKE_ACTION,
                "Principal": SERVICE_PRINCIPAL,
                "SourceArn": group,
            })),
            status: 200,
            invoked: true,
        },
    ]
}

/// RULE: the grant that admits a load balancer's invocation of a `lambda` target group's
/// function is the function's own resource policy, read on every request — allowing
/// `lambda:InvokeFunction` to the load balancer's service principal, for the target group the
/// request was forwarded to and the account it belongs to. Anything else earns the load
/// balancer's own 502 with nothing invoked.
#[tokio::test]
async fn only_the_functions_own_grant_admits_a_load_balancers_invocation() {
    let (router, engine) = emulator();

    // The side channel the handler reports every invocation to.
    let (status, queue) = sqs(&router, "CreateQueue", &json!({ "QueueName": SIDE_CHANNEL })).await;
    assert_eq!(
        status, 200,
        "setup CreateQueue: expected 200, got {status} {queue}"
    );
    let queue_url = queue["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateQueue: no QueueUrl in {queue}"))
        .to_string();

    // The function the load balancer forwards to, created through Lambda's own API.
    let (status, created) = lambda(
        &router,
        "POST",
        "/2015-03-31/functions",
        json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 30,
            "Environment": { "Variables": { "QUEUE_URL": queue_url } },
            "Code": { "ZipFile": aws_smithy_types::base64::encode(package()) },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateFunction: expected 201, got {status} {created}"
    );
    let function_arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateFunction: no FunctionArn in {created}"))
        .to_string();
    // The activation span Lambda owes the function, placed rather than slept for — the one
    // reading of it every suite that invokes a freshly created function shares.
    activate_functions(&engine);

    // The load balancer and the address it publishes, the two `lambda` target groups, the
    // registration of the function with the one the listener forwards to, and that listener —
    // each through this service's own API.
    let (lb_arn, published) = create_load_balancer(&router, LOAD_BALANCER).await;
    let group = create_target_group(&router, TARGET_GROUP, &[("TargetType", "lambda")]).await;
    let other =
        create_target_group(&router, OTHER_TARGET_GROUP, &[("TargetType", "lambda")]).await;

    let mut form = action("RegisterTargets");
    form.push(p("TargetGroupArn", &group));
    form.push(p("Targets.member.1.Id", &function_arn));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup RegisterTargets: expected 200, got {} body {}",
        reply.status, reply.body
    );

    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", &lb_arn));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", "80"));
    form.push(p("DefaultActions.member.1.Type", "forward"));
    form.push(p("DefaultActions.member.1.TargetGroupArn", &group));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateListener: expected 200, got {} body {}",
        reply.status, reply.body
    );

    // The move to `active` the clock owes the load balancer, placed rather than slept for.
    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));

    // An ARN pattern over the group, which is the form the documented `StringLike` comparison
    // of `SourceArn` admits: the group's own ARN with its last element replaced by a wildcard.
    let wildcard = group
        .rsplit_once('/')
        .map(|(prefix, _id)| format!("{prefix}/*"))
        .unwrap_or_else(|| panic!("setup: the target group ARN {group:?} names no resource path"));

    for (index, row) in rows(&group, &other, &wildcard).into_iter().enumerate() {
        // The statement this row's policy carries, written as a client writes one.
        let statement = format!("grant-{index}");
        if let Some(grant) = &row.grant {
            let mut body = grant.clone();
            body["StatementId"] = Value::from(statement.clone());
            let (status, written) = lambda(
                &router,
                "POST",
                &format!("/2015-03-31/functions/{FUNCTION}/policy"),
                body,
            )
            .await;
            assert_eq!(
                status, 201,
                "case {:?}: expected AddPermission to answer 201, got {status} {written}",
                row.case
            );
        }

        // The request itself, on the address the load balancer published. The path's last
        // segment is what the handler reports, so a report names the row that caused it.
        let target = format!("/grant/{index}");
        let answered = exchange(row.case, &published, &target).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {}, got {} in {:?}",
            row.case, row.status, answered.status, answered.raw
        );
        // A refused invocation is the LOAD BALANCER's own answer: the function's body is
        // never what such a request is answered with.
        assert_eq!(
            answered.raw.contains(TARGET_BODY),
            row.status == 200,
            "case {:?}: expected the function's own body to be served={}, got {:?}",
            row.case,
            row.status == 200,
            answered.raw
        );
        // And the function RAN exactly once where the grant admitted the invocation, and not
        // at all where it did not.
        let expected: Vec<String> = if row.invoked {
            vec![index.to_string()]
        } else {
            Vec::new()
        };
        let seen = reports(&router, row.case, &queue_url, BUDGET, expected.len()).await;
        assert_eq!(
            seen, expected,
            "case {:?}: expected the invocations {expected:?} to be reported, got {seen:?}",
            row.case
        );

        if row.grant.is_some() {
            let (status, removed) = lambda(
                &router,
                "DELETE",
                &format!("/2015-03-31/functions/{FUNCTION}/policy/{statement}"),
                Value::Null,
            )
            .await;
            assert_eq!(
                status, 204,
                "case {:?}: expected RemovePermission to answer 204, got {status} {removed}",
                row.case
            );
        }
    }

    // What the refused rows cost the registration: nothing. The group still registers the one
    // Lambda target, which is why the last row is served without re-registering anything.
    let mut form = action("DescribeTargetHealth");
    form.push(p("TargetGroupArn", &group));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case the registration after every row: expected 200, got {} body {}",
        reply.status, reply.body
    );
    assert!(
        reply.body.contains(&function_arn),
        "case the registration after every row: expected the function ARN {function_arn:?}, got \
         {}",
        reply.body
    );
}

