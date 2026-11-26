// SPDX-License-Identifier: CC0-1.0

//! The PERMISSIONS BOUNDARY attached to a stack's service role, end to end over the wire.
//!
//! RULE under test: **the permissions boundary attached to the IAM role a stack operation acts
//! as caps every permission that role's identity policies grant — "a permissions boundary sets
//! the maximum permissions that an identity-based policy can grant" — so a call the boundary
//! does not admit is not made however wide the role's own policies are: the resource fails with
//! the owning service's own 403 authorization answer, naming the role session, the action, the
//! resource and which rule withheld the call, and the boundary that explicitly denied it is
//! NAMED in that sentence.** The ceiling is read from IAM at the instant of the deployment, so
//! removing it lets the same deployment through.
//!
//! The rows differ along what the rule generalises over: what the ceiling says about the call
//! (an explicit deny of the create's own action, of every action of the called service, of the
//! resource's own ARN; an allow naming only another service; an allow of every action but that
//! service's; an allow of another action of the same service; an allow of the create scoped to
//! another resource), which service owns the declared resource and which wire it speaks (AWS
//! Systems Manager on awsJson, Amazon SNS on awsQuery), and the role's identity policy
//! underneath, which allows everything in every row so that only the ceiling can decide. Three
//! rows are controls no gate refusing every call could pass: a ceiling over every action, a
//! ceiling over the create's own action on the resource itself, and a role carrying no boundary
//! at all.
//!
//! Nothing here sleeps for a span the emulator owes: every wait polls for the CONDITION it is
//! about under a generous deadline.
//!
//! The last test is the same ceiling over the principal that SIGNED the request — a stack
//! operation with no `RoleARN` runs with the caller's own permissions — where the clause is read
//! off the caller's own boundary in exactly the same way.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

use crate::harness::{
    call, create_stack, envelope_parts, is_a_request_id, member, xml_blocks, xml_text, REGION,
};
use crate::service_role_permissions::{put_role_policy, trusts_cloudformation};
use crate::stack_lifecycle::{iam, sns, ACCOUNT_ID};
use crate::update_rollback_recovery::{parameter, parameter_value, template};

/// The role session CloudFormation makes a stack's calls under, which is what an authorization
/// refusal of one of them names the caller by: a refusal names the SESSION of an assumed role.
const SESSION: &str = "AWSCloudFormation";

/// The `HandlerErrorCode` class a call that was not permitted is reported under.
const ACCESS_DENIED_CLASS: &str = "AccessDenied";

/// How long a case looks again for a condition the emulator owes, and how often.
///
/// Generous: every wait here is for a CONDITION (a stack reporting a status, an event being
/// recorded), never for a span of the clock, so a loaded host cannot decide a case.
const DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// A router carrying CloudFormation, the two services these templates' resources live in and
/// IAM — which holds the role and the managed policy that is its ceiling — all on ONE engine,
/// the shape `core/router/src/lib.rs` mounts every service in.
fn bounded_deployment_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router
}

/// One statement, as IAM's own grammar writes it.
fn statement(effect: &str, action: &str, resource: &str) -> Value {
    json!({"Effect": effect, "Action": action, "Resource": resource})
}

/// The same statement with its action element written as the `NotAction` the grammar also
/// admits: it is about every action EXCEPT the ones `not_action` names.
fn excluding(effect: &str, not_action: &str, resource: &str) -> Value {
    json!({"Effect": effect, "NotAction": not_action, "Resource": resource})
}

/// Everything the role is allowed, which every row's identity policy grants: the rows are about
/// what the CEILING takes back out of it.
fn allows_everything() -> Value {
    json!([statement("Allow", "*", "*")])
}

/// A role trusting CloudFormation and allowed everything by its one inline policy, answered
/// with the ARN IAM persisted for it.
async fn unbounded_role(router: &MultiServiceRouter, name: &str) -> String {
    let (http_status, created) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trusts_cloudformation()),
        ],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "role {name:?}: expected CreateRole to succeed, got {http_status} {created}"
    );
    let arn = xml_text(&created, "Arn")
        .unwrap_or_else(|| panic!("role {name:?}: CreateRole reported no Arn: {created}"));
    put_role_policy(router, name, allows_everything()).await;
    arn
}

/// Create the managed policy `statements` describes and answer the ARN IAM persisted for it: a
/// permissions boundary is attached as a managed policy ("the ARN of the managed policy that is
/// used to set the permissions boundary").
async fn managed_policy(router: &MultiServiceRouter, name: &str, statements: Value) -> String {
    let document = json!({"Version": "2012-10-17", "Statement": statements}).to_string();
    let (http_status, created) = iam(
        router,
        "CreatePolicy",
        &[("PolicyName", name), ("PolicyDocument", &document)],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "policy {name:?}: expected CreatePolicy to succeed, got {http_status} {created}"
    );
    xml_text(&created, "Arn")
        .unwrap_or_else(|| panic!("policy {name:?}: CreatePolicy reported no Arn: {created}"))
}

/// Attach `boundary_arn` to the role as its permissions boundary, and read it back off the role
/// — the ceiling has to be IAM's own record before a deployment can be judged against it.
async fn bound_by(router: &MultiServiceRouter, role_name: &str, boundary_arn: &str) {
    let (http_status, body) = iam(
        router,
        "PutRolePermissionsBoundary",
        &[
            ("RoleName", role_name),
            ("PermissionsBoundary", boundary_arn),
        ],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "role {role_name:?}: expected PutRolePermissionsBoundary to succeed, got {http_status} \
         {body}"
    );
    assert_eq!(
        reported_boundary(router, role_name).await.as_deref(),
        Some(boundary_arn),
        "role {role_name:?}: expected GetRole to report the boundary just attached"
    );
}

/// The `PermissionsBoundary.PermissionsBoundaryArn` IAM reports for the role, or `None` when it
/// reports none.
async fn reported_boundary(router: &MultiServiceRouter, role_name: &str) -> Option<String> {
    let (http_status, body) = iam(router, "GetRole", &[("RoleName", role_name)]).await;
    assert_eq!(
        http_status, 200,
        "role {role_name:?}: expected GetRole to succeed, got {http_status} {body}"
    );
    xml_text(&body, "PermissionsBoundaryArn")
}

/// The service that OWNS the resource a row declares, which is the service the deployment's
/// calls about it are made to — so it is also the action namespace the ceiling is asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Parameter,
    Topic,
}

impl Owner {
    /// The action namespace of that service, which is its signing name.
    fn namespace(self) -> &'static str {
        match self {
            Self::Parameter => "ssm",
            Self::Topic => "sns",
        }
    }

    /// The operation a CREATE of that resource calls on its owning service, as that service's
    /// own model names it.
    fn created_by(self) -> &'static str {
        match self {
            Self::Parameter => "PutParameter",
            Self::Topic => "CreateTopic",
        }
    }

    /// The logical id the template declares that resource under.
    fn logical_id(self) -> &'static str {
        match self {
            Self::Parameter => "Param",
            Self::Topic => "Topic",
        }
    }
}

/// The IAM action reaching that service's create asks for, spelled as the service that refuses
/// it names an action in its OWN refusal — the workspace's one answer for that spelling
/// (`::service_common::iam_authorization::reported_action`), so this suite asks the same
/// question the gate answers.
fn reported_action(owner: Owner) -> String {
    ::service_common::iam_authorization::reported_action(
        &::service_common::iam_authorization::iam_action(owner.namespace(), owner.created_by()),
    )
}

/// The name a row's resource is created under, and the ARN a policy statement — and an
/// authorization refusal — names that resource by.
struct Named {
    stack: String,
    resource: String,
    target: String,
}

impl Named {
    fn of(owner: Owner, stack: &str) -> Self {
        let resource = format!("{stack}-resource");
        let target = match owner {
            Owner::Topic => format!("arn:aws:sns:{REGION}:{ACCOUNT_ID}:{resource}"),
            // A Parameter Store parameter's own identifier is its NAME, so what a statement
            // scoped to it names is the ARN of a parameter of that name
            // (`arn:<partition>:ssm:<region>:<account>:parameter/<name>`).
            Owner::Parameter => format!("arn:aws:ssm:{REGION}:{ACCOUNT_ID}:parameter/{resource}"),
        };
        Self {
            stack: stack.to_string(),
            resource,
            target,
        }
    }
}

/// The `Resources` section a row deploys: the one resource its owner names.
fn declared(owner: Owner, named: &Named) -> Value {
    match owner {
        Owner::Parameter => json!({"Param": parameter(&named.resource, "inside-the-boundary")}),
        Owner::Topic => json!({
            "Topic": {
                "Type": "AWS::SNS::Topic",
                "Properties": {"TopicName": named.resource},
            },
        }),
    }
}

/// Whether the owning service holds the row's resource right now, read through that service's
/// OWN wire — which is what says whether the call was made at all.
async fn owning_service_holds(
    router: &MultiServiceRouter,
    case: &str,
    owner: Owner,
    named: &Named,
) -> bool {
    match owner {
        Owner::Parameter => parameter_value(router, &named.resource).await.is_some(),
        Owner::Topic => {
            let (http_status, body) =
                sns(router, "GetTopicAttributes", &[("TopicArn", &named.target)]).await;
            assert!(
                matches!(http_status, 200 | 400 | 404),
                "case {case:?}: GetTopicAttributes answered {http_status} {body}"
            );
            http_status == 200
        }
    }
}

/// What one row expects of the deployment, and — where the ceiling withheld the call — which of
/// IAM's rules withheld it.
enum Expected {
    Created,
    /// An explicit `Deny` in the boundary, whose refusal NAMES the boundary.
    RefusedByTheCeilingsDeny,
    /// A ceiling that admits no action of the called service at all.
    RefusedForWantOfTheCeilingsAllow,
}

/// One row: what the role's ceiling says, which resource the stack declares, and whether the
/// deployment is carried out.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The statements of the managed policy attached as the role's permissions boundary; `None`
    /// for a role carrying no boundary at all.
    ceiling: Option<fn(&Named) -> Value>,
    owner: Owner,
    expected: Expected,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a ceiling explicitly denying the create's own action",
            stack: "ceiling-deny-named",
            ceiling: Some(|_| {
                json!([
                    statement("Allow", "*", "*"),
                    statement("Deny", "ssm:PutParameter", "*"),
                ])
            }),
            owner: Owner::Parameter,
            expected: Expected::RefusedByTheCeilingsDeny,
        },
        // Amazon SNS speaks awsQuery, where the operation is the request's `Action` parameter
        // rather than an `X-Amz-Target` header: the rule is the same either way.
        Row {
            case: "a ceiling explicitly denying every action of the called service",
            stack: "ceiling-deny-service",
            ceiling: Some(|_| {
                json!([
                    statement("Allow", "*", "*"),
                    statement("Deny", "sns:*", "*"),
                ])
            }),
            owner: Owner::Topic,
            expected: Expected::RefusedByTheCeilingsDeny,
        },
        Row {
            case: "a ceiling explicitly denying the create on the resource's own ARN",
            stack: "ceiling-deny-arn",
            ceiling: Some(|named| {
                json!([
                    statement("Allow", "*", "*"),
                    statement("Deny", "ssm:*", &named.target),
                ])
            }),
            owner: Owner::Parameter,
            expected: Expected::RefusedByTheCeilingsDeny,
        },
        // The shape the rule exists for: a ceiling scoped to ONE service, over a deployment of a
        // resource of another.
        Row {
            case: "a ceiling naming only another service",
            stack: "ceiling-elsewhere",
            ceiling: Some(|_| json!([statement("Allow", "ssm:*", "*")])),
            owner: Owner::Topic,
            expected: Expected::RefusedForWantOfTheCeilingsAllow,
        },
        Row {
            case: "a ceiling over every action but the called service's",
            stack: "ceiling-not-this-service",
            ceiling: Some(|_| json!([excluding("Allow", "sns:*", "*")])),
            owner: Owner::Topic,
            expected: Expected::RefusedForWantOfTheCeilingsAllow,
        },
        // A CONTROL: a ceiling over everything takes nothing back out of the role's grants.
        Row {
            case: "a ceiling over every action",
            stack: "ceiling-everything",
            ceiling: Some(|_| json!([statement("Allow", "*", "*")])),
            owner: Owner::Parameter,
            expected: Expected::Created,
        },
        // A ceiling is read over the same action and the same resource the grant beneath it is,
        // so one admitting only ANOTHER action of the called service admits no call of this one.
        Row {
            case: "a ceiling over another action of the called service",
            stack: "ceiling-other-action",
            ceiling: Some(|_| json!([statement("Allow", "ssm:DeleteParameter", "*")])),
            owner: Owner::Parameter,
            expected: Expected::RefusedForWantOfTheCeilingsAllow,
        },
        Row {
            case: "a ceiling over the create scoped to another resource of that service",
            stack: "ceiling-other-resource",
            ceiling: Some(|_| {
                json!([statement(
                    "Allow",
                    "sns:CreateTopic",
                    &format!("arn:aws:sns:{REGION}:{ACCOUNT_ID}:someone-else"),
                )])
            }),
            owner: Owner::Topic,
            expected: Expected::RefusedForWantOfTheCeilingsAllow,
        },
        // A CONTROL: a ceiling over the create's own action, scoped to the resource the stack
        // declares, takes nothing back out of the role's grants.
        Row {
            case: "a ceiling over the create's own action on the resource itself",
            stack: "ceiling-the-action",
            ceiling: Some(|named| {
                json!([statement("Allow", "ssm:PutParameter", &named.target)])
            }),
            owner: Owner::Parameter,
            expected: Expected::Created,
        },
        // A CONTROL: a role carrying no boundary is capped by nothing.
        Row {
            case: "no boundary at all",
            stack: "ceiling-none",
            ceiling: None,
            owner: Owner::Parameter,
            expected: Expected::Created,
        },
    ]
}

/// Poll `stack` until it reports `wanted`, or [`DEADLINE`] passes.
async fn until_status(router: &MultiServiceRouter, case: &str, stack: &str, wanted: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (http_status, body) =
            call(router, "DescribeStacks", &[member("StackName", stack)]).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: DescribeStacks({stack:?}) must answer 200, got {http_status} {body}"
        );
        let reported = xml_text(&body, "StackStatus").unwrap_or_default();
        if reported == wanted {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {stack:?} to reach {wanted}, last reported {reported:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every `ResourceStatusReason` the stack recorded for `logical_id` in `resource_status`.
async fn reasons_for(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    resource_status: &str,
) -> Vec<String> {
    let (http_status, body) = call(
        router,
        "DescribeStackEvents",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: DescribeStackEvents({stack:?}) must answer 200, got {http_status} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|entry| {
            xml_text(entry, "LogicalResourceId").as_deref() == Some(logical_id)
                && xml_text(entry, "ResourceStatus").as_deref() == Some(resource_status)
        })
        .map(|entry| xml_text(entry, "ResourceStatusReason").unwrap_or_default())
        .collect()
}

/// The same read, looked for again until the stack has recorded at least one such event or
/// [`DEADLINE`] passes: a resource's failure is recorded at an instant the deployment reaches.
async fn until_reasons_for(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    resource_status: &str,
) -> Vec<String> {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let recorded = reasons_for(router, case, stack, logical_id, resource_status).await;
        if !recorded.is_empty() {
            return recorded;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected a {resource_status} event for {logical_id:?} of {stack:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The sentence AWS refuses an unauthorized call with, worded by the workspace's one
/// authorization refusal: the principal, the action, the resource and which of IAM's rules
/// withheld the call.
fn expected_sentence(role_name: &str, owner: Owner, named: &Named, clause: &str) -> String {
    let principal = format!("arn:aws:sts::{ACCOUNT_ID}:assumed-role/{role_name}/{SESSION}");
    ::service_common::iam_authorization::not_authorized_by_identity_policy(
        &principal,
        &reported_action(owner),
        &named.target,
        clause,
    )
}

/// Create the row's stack under a role allowed everything and capped by the row's ceiling, and
/// hold the deployment to what the row expects.
///
/// `DisableRollback` keeps a failed create exactly as it failed, so the case reads the status
/// the refused call left rather than the rollback that would follow it.
async fn assert_row(row: &Row) {
    let case = row.case;
    let router = bounded_deployment_router();
    let named = Named::of(row.owner, row.stack);
    let role_name = format!("{}-deployer", row.stack);
    let role_arn = unbounded_role(&router, &role_name).await;
    let boundary_arn = match row.ceiling {
        Some(build) => {
            let arn = managed_policy(
                &router,
                &format!("{}-ceiling", row.stack),
                build(&named),
            )
            .await;
            bound_by(&router, &role_name, &arn).await;
            Some(arn)
        }
        None => {
            assert_eq!(
                reported_boundary(&router, &role_name).await,
                None,
                "case {case:?}: the control row's role must carry no boundary"
            );
            None
        }
    };

    let body = template(declared(row.owner, &named));
    let (http_status, created) = create_stack(
        &router,
        &named.stack,
        &body,
        &[
            member("RoleARN", &role_arn),
            member("DisableRollback", "true"),
        ],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: CreateStack must be accepted — the decision is the RESOURCE's, not the \
         request's — got {http_status} {created}"
    );

    let (wanted, refused) = match row.expected {
        Expected::Created => (status::CREATE_COMPLETE, None),
        Expected::RefusedByTheCeilingsDeny => {
            let arn = boundary_arn
                .clone()
                .unwrap_or_else(|| panic!("case {case:?}: this row states a ceiling"));
            (
                status::CREATE_FAILED,
                Some(::service_common::iam_authorization::permissions_boundary_explicit_deny(
                    &arn,
                )),
            )
        }
        Expected::RefusedForWantOfTheCeilingsAllow => (
            status::CREATE_FAILED,
            Some(
                ::service_common::iam_authorization::no_permissions_boundary_allows(
                    &reported_action(row.owner),
                ),
            ),
        ),
    };
    until_status(&router, case, &named.stack, wanted).await;

    // What the owning service really holds, which is the point of the rule: a call that was not
    // made created nothing, and one that was made created the resource.
    let held = owning_service_holds(&router, case, row.owner, &named).await;
    assert_eq!(
        held,
        refused.is_none(),
        "case {case:?}: expected the owning service to hold {:?} = {}, got {held}",
        named.resource,
        refused.is_none()
    );

    let Some(clause) = refused else {
        return;
    };
    let logical_id = row.owner.logical_id();
    let recorded = until_reasons_for(
        &router,
        case,
        &named.stack,
        logical_id,
        resource_status::CREATE_FAILED,
    )
    .await;
    assert_eq!(
        recorded.len(),
        1,
        "case {case:?}: expected exactly one {} event for {logical_id:?}, got {recorded:?}",
        resource_status::CREATE_FAILED
    );
    let (quoted, token, class) = envelope_parts(case, &recorded[0]);
    assert_eq!(
        class, ACCESS_DENIED_CLASS,
        "case {case:?}: expected the failure classified as a call that was not permitted, got \
         {class:?} out of {:?}",
        recorded[0]
    );
    assert!(
        is_a_request_id(&token),
        "case {case:?}: expected the token of the resource request, got {token:?}"
    );
    let sentence = expected_sentence(&role_name, row.owner, &named, &clause);
    assert!(
        quoted.contains(&sentence),
        "case {case:?}: expected the refusal {sentence:?} in the reason, got {quoted:?}"
    );
}

/// RULE: a deployment's call the role's permissions boundary does not admit is not made, and the
/// resource fails with the refusal naming the rule — and the boundary — that withheld it; a
/// ceiling that admits the call takes nothing back out of the role's grants.
#[tokio::test]
async fn a_deployment_call_outside_the_roles_boundary_fails_that_resource() {
    for row in rows() {
        assert_row(&row).await;
    }
}

/// RULE (the ceiling is IAM's live record): the boundary is read from IAM at the instant of the
/// deployment, so a deployment the ceiling refuses is carried out once the ceiling is removed —
/// and the role's own grants were never what refused it.
#[tokio::test]
async fn removing_the_boundary_lets_the_same_deployment_through() {
    let case = "a ceiling removed between two deployments";
    let router = bounded_deployment_router();
    let role_name = "ceiling-lifted-deployer";
    let role_arn = unbounded_role(&router, role_name).await;
    let boundary_arn = managed_policy(
        &router,
        "ceiling-lifted-ceiling",
        json!([statement("Allow", "sns:*", "*")]),
    )
    .await;
    bound_by(&router, role_name, &boundary_arn).await;

    // Under the ceiling, the parameter is not written: Systems Manager is outside it.
    let refused = Named::of(Owner::Parameter, "ceiling-lifted-before");
    let (http_status, created) = create_stack(
        &router,
        &refused.stack,
        &template(declared(Owner::Parameter, &refused)),
        &[
            member("RoleARN", &role_arn),
            member("DisableRollback", "true"),
        ],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected CreateStack to be accepted, got {http_status} {created}"
    );
    until_status(&router, case, &refused.stack, status::CREATE_FAILED).await;
    let written = parameter_value(&router, &refused.resource).await;
    assert_eq!(
        written, None,
        "case {case:?}: expected no parameter under the ceiling, got {written:?}"
    );

    // The ceiling is removed — "DeleteRolePermissionsBoundary … deletes the permissions boundary
    // for the specified IAM role" — and nothing else about the role changes.
    let (http_status, body) = iam(
        &router,
        "DeleteRolePermissionsBoundary",
        &[("RoleName", role_name)],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected DeleteRolePermissionsBoundary to succeed, got {http_status} \
         {body}"
    );
    assert_eq!(
        reported_boundary(&router, role_name).await,
        None,
        "case {case:?}: expected the role to report no boundary once it is removed"
    );

    // The same deployment, under the same role, is now carried out.
    let allowed = Named::of(Owner::Parameter, "ceiling-lifted-after");
    let (http_status, created) = create_stack(
        &router,
        &allowed.stack,
        &template(declared(Owner::Parameter, &allowed)),
        &[member("RoleARN", &role_arn)],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected the later CreateStack to be accepted, got {http_status} {created}"
    );
    until_status(&router, case, &allowed.stack, status::CREATE_COMPLETE).await;
    let value = parameter_value(&router, &allowed.resource).await;
    assert_eq!(
        value.as_deref(),
        Some("inside-the-boundary"),
        "case {case:?}: expected the deployment to write the parameter once the ceiling is gone, \
         got {value:?}"
    );
}

/// What one row of the caller's own ceiling states, and what the request it signs is answered.
struct CallerRow {
    case: &'static str,
    user: &'static str,
    /// The statements of the managed policy attached as the user's permissions boundary.
    ceiling: Value,
    /// The reason clause the refusal must close with, or `None` for a call the ceiling admits.
    clause: Option<Clause>,
}

/// Which of IAM's rules a row expects to have withheld the call.
enum Clause {
    /// An explicit `Deny` in the boundary, whose sentence NAMES the boundary.
    TheCeilingsDeny,
    /// A ceiling that does not admit the action at all.
    WantOfTheCeilingsAllow,
}

/// The operation the caller rows sign, and the IAM action it performs: an operation about no
/// stack of its own, so the row turns on the ceiling alone.
const CALLER_OPERATION: &str = "ListStacks";

/// RULE (the same ceiling, for the principal that SIGNED the request): the boundary in force on
/// the caller caps its own grants too, and the refusal names which rule withheld the call — the
/// boundary itself where an explicit `Deny` in it decided.
#[tokio::test]
async fn a_request_outside_the_callers_boundary_is_refused_naming_the_boundary() {
    let rows = vec![
        CallerRow {
            case: "a ceiling explicitly denying the operation",
            user: "ceiling-caller-denied",
            ceiling: json!([
                statement("Allow", "*", "*"),
                statement("Deny", "cloudformation:ListStacks", "*"),
            ]),
            clause: Some(Clause::TheCeilingsDeny),
        },
        CallerRow {
            case: "a ceiling naming only another service",
            user: "ceiling-caller-elsewhere",
            ceiling: json!([statement("Allow", "ssm:*", "*")]),
            clause: Some(Clause::WantOfTheCeilingsAllow),
        },
        // A CONTROL: a ceiling that admits the call takes nothing back out of the caller's own
        // grant.
        CallerRow {
            case: "a ceiling over the operation",
            user: "ceiling-caller-allowed",
            ceiling: json!([statement("Allow", "cloudformation:*", "*")]),
            clause: None,
        },
    ];
    let action = ::service_common::iam_authorization::iam_action(
        "cloudformation",
        CALLER_OPERATION,
    );
    for row in rows {
        let case = row.case;
        let router = bounded_deployment_router();
        // The caller's own policy allows everything, so only the ceiling can decide.
        let access_key_id = crate::stack_lifecycle::user_with_inline_policy(
            &router,
            case,
            row.user,
            "what-the-caller-may-do",
            &json!({"Version": "2012-10-17", "Statement": allows_everything()}).to_string(),
        )
        .await;
        let boundary_arn =
            managed_policy(&router, &format!("{}-ceiling", row.user), row.ceiling).await;
        let (http_status, bounded) = iam(
            &router,
            "PutUserPermissionsBoundary",
            &[
                ("UserName", row.user),
                ("PermissionsBoundary", &boundary_arn),
            ],
        )
        .await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected PutUserPermissionsBoundary to succeed, got {http_status} \
             {bounded}"
        );

        let (http_status, body) =
            crate::harness::call_as(&router, &access_key_id, CALLER_OPERATION, &[]).await;
        let Some(clause) = row.clause else {
            assert_eq!(
                http_status, 200,
                "case {case:?}: expected the call the ceiling admits to be served, got \
                 {http_status} {body}"
            );
            continue;
        };
        assert_eq!(
            http_status, 403,
            "case {case:?}: expected the call outside the ceiling to be refused, got \
             {http_status} {body}"
        );
        let reason = match clause {
            Clause::TheCeilingsDeny => {
                ::service_common::iam_authorization::permissions_boundary_explicit_deny(
                    &boundary_arn,
                )
            }
            Clause::WantOfTheCeilingsAllow => {
                ::service_common::iam_authorization::no_permissions_boundary_allows(&action)
            }
        };
        let message = xml_text(&body, "Message").unwrap_or_default();
        assert!(
            message.contains(&format!("is not authorized to perform: {action}")),
            "case {case:?}: expected the refusal to name the action, got {message:?}"
        );
        assert!(
            message.ends_with(&reason),
            "case {case:?}: expected the refusal to close with {reason:?}, got {message:?}"
        );
    }
}
