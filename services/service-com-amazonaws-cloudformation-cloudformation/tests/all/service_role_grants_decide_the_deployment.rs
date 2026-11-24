// SPDX-License-Identifier: CC0-1.0

//! What a stack's SERVICE ROLE lets its deployment do, end to end over the wire.
//!
//! RULE under test: **a stack's resource changes are carried out with the credentials of the
//! stack's service role — "CloudFormation uses the role's credentials to make calls on your
//! behalf" (`CreateStackInput$RoleARN`) — and the role keeps acting for every later operation
//! that names no other one ("CloudFormation always uses this role for all future operations on
//! the stack"), so a call that role is not permitted to make is not made: under IAM's default
//! deny, a role whose readable identity documents allow it NO action of the service that owns
//! a declared resource cannot create, change or delete that resource, and the resource fails
//! with that service's own 403 authorization answer — the sentence naming the role session,
//! the action and the resource, inside the diagnostic its SDK appends and the envelope
//! CloudFormation quotes a provider's answer in, classified `AccessDenied`.** A role the
//! documents DO allow an action of that service deploys the resource, whichever action and
//! whichever resource that allow is scoped to, because the calls a deployment makes are this
//! emulator's approximation of a provider's call set; an explicit `Deny` of the call refuses
//! it whatever else is in force.
//!
//! The rows differ along what the rule generalises over: which service owns the declared
//! resource and which wire it speaks (Amazon SQS and AWS Systems Manager on awsJson, Amazon
//! SNS on awsQuery), and what the role's one inline policy says about that service — the
//! called action named in full, a wildcard over the service, another action of the same
//! service, an allow of the called action scoped to another resource, a `NotAction` excluding
//! another action of the service and one excluding all of them, only another service's
//! actions, an unrelated service's action alone, and an allow of everything beside an explicit
//! deny. Two rows are controls that no gate refusing every call could pass: a role carrying no
//! readable document at all, and a stack naming no role.
//!
//! The second case is the ONE the whole rule exists for: a stack created under a role that may
//! act only in one service takes a later update — which names no role of its own — under that
//! same recorded role, so a resource the update adds in a service the role may not act in
//! fails and the update is rolled back, leaving the role and the resources the first
//! deployment created untouched.
//!
//! Nothing here sleeps for a span the emulator owes: every wait polls for the CONDITION it is
//! about under a generous deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

use crate::declared_topic_policy_decides_the_alarm_action::sqs;
use crate::harness::{
    assert_sdk_diagnostic_with_attempts, call, create_stack, envelope_parts, is_a_request_id,
    member, xml_blocks, xml_text, REGION,
};
use crate::service_role_permissions::{put_role_policy, trusts_cloudformation};
use crate::stack_lifecycle::{iam, sns, ACCOUNT_ID};
use crate::update_rollback_recovery::{parameter, parameter_value, template};

/// The role session CloudFormation makes a stack's calls under, which is what an authorization
/// refusal of one of them names the caller by: `RoleARN` is the role "that CloudFormation
/// assumes", and a refusal names the SESSION of an assumed role rather than the role.
const SESSION: &str = "AWSCloudFormation";

/// The `HandlerErrorCode` class a call that was not permitted is reported under, which is what
/// tells a client whether the failed resource is worth retrying.
const ACCESS_DENIED_CLASS: &str = "AccessDenied";

/// The name Amazon SQS's own SDK client reports that service under, and the status a service
/// answers an unauthorized call with — the two values its diagnostic states.
const SQS_SDK_NAME: &str = "Sqs";
const UNAUTHORIZED_STATUS: u16 = 403;

/// How long a case looks again for a condition the emulator owes, and how often.
///
/// Generous: every wait here is for a CONDITION (a stack reporting a status, an event being
/// recorded), never for a span of the clock, so a loaded host cannot decide a case.
const DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// A router carrying CloudFormation and the three services these templates' resources live in,
/// all on ONE engine — the shape `core/router/src/lib.rs` mounts every service in — plus IAM,
/// which holds the role whose documents decide the deployment's calls.
///
/// Its own router rather than the shared harness's, for the reason
/// `declared_topic_policy_decides_the_alarm_action` records: mounting Amazon SQS for every
/// suite of this target would change what the accounts those suites build hold.
fn deployed_under_a_role_router() -> MultiServiceRouter {
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
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
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

/// A role trusting CloudFormation, carrying `statements` as its one inline permissions policy
/// where a case states any; answered with the ARN IAM persisted for it.
async fn given_role(router: &MultiServiceRouter, name: &str, statements: Option<Value>) -> String {
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
    if let Some(statements) = statements {
        put_role_policy(router, name, statements).await;
    }
    arn
}

/// One statement of an identity policy, as IAM's own grammar writes it.
fn statement(effect: &str, action: &str, resource: &str) -> Value {
    json!({"Effect": effect, "Action": action, "Resource": resource})
}

/// The same statement with its action element written as the `NotAction` the grammar also
/// admits: it is about every action EXCEPT the one `not_action` names.
fn excluding(effect: &str, not_action: &str, resource: &str) -> Value {
    json!({"Effect": effect, "NotAction": not_action, "Resource": resource})
}

/// The service that OWNS the resource a row declares, which is the service the deployment's
/// calls about it are made to — so it is also the action namespace the role's documents are
/// asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Queue,
    Parameter,
    Topic,
}

impl Owner {
    /// The action namespace of that service, which is its signing name.
    fn namespace(self) -> &'static str {
        match self {
            Self::Queue => "sqs",
            Self::Parameter => "ssm",
            Self::Topic => "sns",
        }
    }

    /// The operation a CREATE of that resource calls on its owning service, as that service's
    /// own model names it: a queue is created by `CreateQueue`, a Parameter Store parameter is
    /// written by `PutParameter`, a topic is created by `CreateTopic`.
    fn created_by(self) -> &'static str {
        match self {
            Self::Queue => "CreateQueue",
            Self::Parameter => "PutParameter",
            Self::Topic => "CreateTopic",
        }
    }

    /// The logical id the template declares that resource under.
    fn logical_id(self) -> &'static str {
        match self {
            Self::Queue => "Queue",
            Self::Parameter => "Param",
            Self::Topic => "Topic",
        }
    }
}

/// The IAM action reaching that service's create asks for, spelled as the service that refuses
/// it names an action in its OWN refusal.
///
/// The case is the refusing service's wording rather than part of the decision — IAM compares
/// action names case-insensitively — and the workspace states it once
/// (`::service_common::iam_authorization::reported_action`, whose table carries the namespaces
/// worded case-folded), so this suite asks the same question the gate answers rather than
/// spelling a third copy of it.
fn reported_action(owner: Owner) -> String {
    ::service_common::iam_authorization::reported_action(
        &::service_common::iam_authorization::iam_action(owner.namespace(), owner.created_by()),
    )
}

/// The name a row's resources are created under, and the identifier a policy statement — and
/// an authorization refusal — names that resource by.
struct Named {
    stack: String,
    resource: String,
    /// What the deployment's calls about the resource are authorized against: the ARN the
    /// resource carries, which is what a statement names it by.
    target: String,
}

impl Named {
    fn of(owner: Owner, stack: &str) -> Self {
        let resource = format!("{stack}-resource");
        let target = match owner {
            Owner::Queue => format!("arn:aws:sqs:{REGION}:{ACCOUNT_ID}:{resource}"),
            Owner::Topic => format!("arn:aws:sns:{REGION}:{ACCOUNT_ID}:{resource}"),
            // A Parameter Store parameter's own identifier is its NAME, so what a statement
            // scoped to it names is the ARN of a parameter of that name
            // (`arn:<partition>:ssm:<region>:<account>:parameter/<name>`, the form AWS
            // documents for a Systems Manager parameter).
            Owner::Parameter => {
                format!("arn:aws:ssm:{REGION}:{ACCOUNT_ID}:parameter/{resource}")
            }
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
        Owner::Queue => json!({
            "Queue": {
                "Type": "AWS::SQS::Queue",
                "Properties": {"QueueName": named.resource},
            },
        }),
        Owner::Parameter => json!({"Param": parameter(&named.resource, "made-by-role")}),
        Owner::Topic => json!({
            "Topic": {
                "Type": "AWS::SNS::Topic",
                "Properties": {"TopicName": named.resource},
            },
        }),
    }
}

/// What one row expects of the deployment, and — where the role may not make the call — which
/// of IAM's rules withheld it, as the refusal's own reason clause.
enum Expected {
    Created,
    RefusedForWantOfAGrant,
    RefusedByAnExplicitDeny,
}

/// One row: what the role's one inline policy says, which resource the stack declares, and
/// whether the deployment is carried out.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The statements of the role's inline policy; `None` for a role carrying no document at
    /// all, and the row with no role at all is [`Row::names_a_role`] = false.
    statements: Option<fn(&Named) -> Value>,
    names_a_role: bool,
    owner: Owner,
    expected: Expected,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the called service's own create action, named in full",
            stack: "role-grant-named",
            statements: Some(|_| json!([statement("Allow", "ssm:PutParameter", "*")])),
            names_a_role: true,
            owner: Owner::Parameter,
            expected: Expected::Created,
        },
        Row {
            case: "a wildcard over every action of the called service",
            stack: "role-grant-wildcard",
            statements: Some(|_| json!([statement("Allow", "sqs:*", "*")])),
            names_a_role: true,
            owner: Owner::Queue,
            expected: Expected::Created,
        },
        // The deployment's own calls are this emulator's approximation of a provider's call
        // set, so what the documents are asked is whether the role may act in the SERVICE at
        // all: a grant of another of its actions is not evidence against the create.
        Row {
            case: "another action of the called service",
            stack: "role-grant-other-action",
            statements: Some(|_| json!([statement("Allow", "sqs:DeleteQueue", "*")])),
            names_a_role: true,
            owner: Owner::Queue,
            expected: Expected::Created,
        },
        Row {
            case: "the create action scoped to another resource of the called service",
            stack: "role-grant-other-resource",
            statements: Some(|_| {
                json!([statement(
                    "Allow",
                    "sns:CreateTopic",
                    &format!("arn:aws:sns:{REGION}:{ACCOUNT_ID}:someone-else"),
                )])
            }),
            names_a_role: true,
            owner: Owner::Topic,
            expected: Expected::Created,
        },
        // A `NotAction` element is about every action except the ones it names, so one that
        // excludes another action of the called service leaves this call's own allowed.
        Row {
            case: "every action but another one of the called service",
            stack: "role-grant-not-action",
            statements: Some(|_| json!([excluding("Allow", "sqs:DeleteQueue", "*")])),
            names_a_role: true,
            owner: Owner::Queue,
            expected: Expected::Created,
        },
        // The shape the rule exists for: a role scoped to ONE service, deploying a resource of
        // another.
        Row {
            case: "every action of another service and none of the called one",
            stack: "role-grant-elsewhere",
            statements: Some(|_| json!([statement("Allow", "ssm:*", "*")])),
            names_a_role: true,
            owner: Owner::Queue,
            expected: Expected::RefusedForWantOfAGrant,
        },
        Row {
            case: "one unrelated action and nothing of the called service",
            stack: "role-grant-unrelated",
            statements: Some(|_| json!([statement("Allow", "s3:GetObject", "*")])),
            names_a_role: true,
            owner: Owner::Parameter,
            expected: Expected::RefusedForWantOfAGrant,
        },
        Row {
            case: "a role allowed no action of the service owning a topic",
            stack: "role-grant-no-sns",
            statements: Some(|_| json!([statement("Allow", "sqs:*", "*")])),
            names_a_role: true,
            owner: Owner::Topic,
            expected: Expected::RefusedForWantOfAGrant,
        },
        Row {
            case: "every action but the called service's own",
            stack: "role-grant-not-this-service",
            statements: Some(|_| json!([excluding("Allow", "sqs:*", "*")])),
            names_a_role: true,
            owner: Owner::Queue,
            expected: Expected::RefusedForWantOfAGrant,
        },
        // "An explicit deny in any policy overrides any allows", whatever else the role may do
        // in that service.
        Row {
            case: "everything allowed beside an explicit deny of the create",
            stack: "role-grant-denied",
            statements: Some(|named| {
                json!([
                    statement("Allow", "*", "*"),
                    statement("Deny", "sqs:CreateQueue", &named.target),
                ])
            }),
            names_a_role: true,
            owner: Owner::Queue,
            expected: Expected::RefusedByAnExplicitDeny,
        },
        // A CONTROL: a role carrying no readable document at all is one whose permissions this
        // emulator cannot establish, so nothing here refuses its calls.
        Row {
            case: "a role carrying no readable document at all",
            stack: "role-grant-silent",
            statements: None,
            names_a_role: true,
            owner: Owner::Queue,
            expected: Expected::Created,
        },
        // A CONTROL: a stack that names no role runs with the caller's own permissions, so
        // there is no assumed role for this gate to judge a call on.
        Row {
            case: "a stack naming no role at all",
            stack: "role-grant-roleless",
            statements: None,
            names_a_role: false,
            owner: Owner::Queue,
            expected: Expected::Created,
        },
    ]
}

/// Poll `stack` until it reports `wanted`, or [`DEADLINE`] passes.
async fn until_status(router: &MultiServiceRouter, case: &str, stack: &str, wanted: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (http_status, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
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

/// Every `ResourceStatusReason` the stack recorded for `logical_id` in `resource_status`, in
/// the order the events report them.
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

/// Whether the owning service holds the resource `named` addresses right now, read through
/// that service's OWN wire — which is what says whether a call was made at all.
async fn owning_service_holds(
    router: &MultiServiceRouter,
    case: &str,
    owner: Owner,
    named: &Named,
) -> bool {
    match owner {
        Owner::Queue => {
            let (http_status, body) =
                sqs(router, "GetQueueUrl", &json!({"QueueName": named.resource})).await;
            assert!(
                matches!(http_status, 200 | 400),
                "case {case:?}: GetQueueUrl answered {http_status} {body}"
            );
            http_status == 200
        }
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

/// The sentence AWS refuses an unauthorized call with, worded by the workspace's one
/// authorization refusal: the principal, the action, the resource and which of IAM's rules
/// withheld the grant.
fn expected_sentence(role_name: &str, owner: Owner, named: &Named, clause: &str) -> String {
    let principal = format!("arn:aws:sts::{ACCOUNT_ID}:assumed-role/{role_name}/{SESSION}");
    ::service_common::iam_authorization::not_authorized_by_identity_policy(
        &principal,
        &reported_action(owner),
        &named.target,
        clause,
    )
}

/// RULE: a deployment's call to a service the stack's service role is allowed no action of is
/// refused, failing that resource with that service's own authorization answer; a role the
/// documents allow an action of that service deploys the resource.
#[tokio::test]
async fn a_deployment_call_the_service_role_may_not_make_fails_that_resource() {
    for row in rows() {
        let case = row.case;
        let router = deployed_under_a_role_router();
        let named = Named::of(row.owner, row.stack);
        let role_name = format!("{}-deployer", row.stack);
        let mut extra = vec![member("DisableRollback", "true")];
        if row.names_a_role {
            let statements = row.statements.map(|build| build(&named));
            let role_arn = given_role(&router, &role_name, statements).await;
            extra.push(member("RoleARN", &role_arn));
        }
        let body = template(declared(row.owner, &named));
        let (http_status, created) = create_stack(&router, &named.stack, &body, &extra).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: CreateStack must be accepted — the decision is the RESOURCE's, not \
             the request's — got {http_status} {created}"
        );

        let (wanted, refused) = match row.expected {
            Expected::Created => (status::CREATE_COMPLETE, None),
            Expected::RefusedForWantOfAGrant => (
                status::CREATE_FAILED,
                Some(::service_common::iam_authorization::no_identity_policy_allows(
                    &reported_action(row.owner),
                )),
            ),
            Expected::RefusedByAnExplicitDeny => (
                status::CREATE_FAILED,
                Some(
                    ::service_common::iam_authorization::IDENTITY_POLICY_EXPLICIT_DENY
                        .to_string(),
                ),
            ),
        };
        until_status(&router, case, &named.stack, wanted).await;

        // What the owning service really holds, which is the point of the rule: a call that
        // was not made created nothing, and one that was made created the resource.
        let held = owning_service_holds(&router, case, row.owner, &named).await;
        assert_eq!(
            held,
            refused.is_none(),
            "case {case:?}: expected the owning service to hold {:?} = {}, got {held}",
            named.resource,
            refused.is_none()
        );

        let Some(clause) = refused else {
            continue;
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
            "case {case:?}: expected the failure classified as a call that was not permitted, \
             got {class:?} out of {:?}",
            recorded[0]
        );
        assert!(
            is_a_request_id(&token),
            "case {case:?}: expected the token of the resource request, got {token:?}"
        );
        let sentence = expected_sentence(&role_name, row.owner, &named, &clause);
        // The refusal is QUOTED as the resource's provider reported it, and a provider that
        // frames an answer in wording of its own carries that sentence inside its frame — so
        // what every row is held to is that the sentence is there, exactly as AWS words it.
        assert!(
            quoted.contains(&sentence),
            "case {case:?}: expected the refusal {sentence:?} in the reason, got {quoted:?}"
        );
        // ...and for a service whose SDK's diagnostic this emulator words, the call's own
        // diagnostic closes it: the service as its SDK names it, the 403 it answered, the id of
        // the call and the attempts the SDK made.
        if row.owner == Owner::Queue {
            assert_sdk_diagnostic_with_attempts(case, &quoted, SQS_SDK_NAME, UNAUTHORIZED_STATUS);
        }
    }
}

/// The sentence Amazon SQS answers a read of a queue it does not hold with, which is that
/// service's own wording (its crate's `state::QUEUE_DOES_NOT_EXIST_MESSAGE`).
const NO_SUCH_QUEUE: &str = "The specified queue does not exist.";

/// RULE (the later operation): the role a stack RECORDS keeps acting for an operation that
/// names no other one, so an update that adds a resource in a service that role may not act in
/// fails that resource and is rolled back — the stack settling at `UPDATE_ROLLBACK_COMPLETE`
/// with the role, and the resources the first deployment created, exactly as they were.
#[tokio::test]
async fn an_update_naming_no_role_runs_under_the_one_the_stack_recorded() {
    let case = "a later update under the stack's recorded role";
    let router = deployed_under_a_role_router();
    let stack = "role-kept-for-later-deploys";
    let role_name = format!("{stack}-deployer");
    // The role may act in AWS Systems Manager and nowhere else, which is what the stack's first
    // deployment needs and its second does not have.
    let role_arn = given_role(
        &router,
        &role_name,
        Some(json!([statement("Allow", "ssm:*", "*")])),
    )
    .await;
    let parameter_name = format!("/{stack}/p");
    let queue_name = format!("{stack}-queue");
    let queue_arn = format!("arn:aws:sqs:{REGION}:{ACCOUNT_ID}:{queue_name}");
    let first = json!({"Param": parameter(&parameter_name, "made-by-role")});

    let (http_status, created) = create_stack(
        &router,
        stack,
        &template(first.clone()),
        &[member("RoleARN", &role_arn)],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected CreateStack to succeed, got {http_status} {created}"
    );
    until_status(&router, case, stack, status::CREATE_COMPLETE).await;
    let written = parameter_value(&router, &parameter_name).await;
    assert_eq!(
        written.as_deref(),
        Some("made-by-role"),
        "case {case:?}: expected the role's own deployment to write the parameter, got {written:?}"
    );

    // The second deployment names NO role of its own and adds a queue: the role the stack
    // recorded is what its calls are made under, and that role may not act in Amazon SQS.
    let second = json!({
        "Param": parameter(&parameter_name, "made-by-role"),
        "Queue": {
            "Type": "AWS::SQS::Queue",
            "Properties": {"QueueName": queue_name},
        },
    });
    let (http_status, updated) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack),
            member("TemplateBody", &template(second)),
        ],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected UpdateStack to be accepted, got {http_status} {updated}"
    );
    until_status(&router, case, stack, status::UPDATE_ROLLBACK_COMPLETE).await;

    // The role is still the stack's, and the resource the role DID create is untouched.
    let (http_status, described) =
        call(&router, "DescribeStacks", &[member("StackName", stack)]).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: DescribeStacks must answer 200, got {http_status} {described}"
    );
    assert_eq!(
        xml_text(&described, "RoleARN").as_deref(),
        Some(role_arn.as_str()),
        "case {case:?}: expected the rolled-back stack to keep its role, got {described}"
    );
    let kept = parameter_value(&router, &parameter_name).await;
    assert_eq!(
        kept.as_deref(),
        Some("made-by-role"),
        "case {case:?}: expected the role-created parameter to survive the rollback, got {kept:?}"
    );

    // Amazon SQS holds no such queue: the call the role was not permitted to make was not made.
    let (http_status, answered) = sqs(&router, "GetQueueUrl", &json!({"QueueName": queue_name})).await;
    assert_eq!(
        http_status, 400,
        "case {case:?}: expected Amazon SQS to refuse a read of the queue, got {http_status} \
         {answered}"
    );
    assert_eq!(
        answered.get("message").and_then(Value::as_str),
        Some(NO_SUCH_QUEUE),
        "case {case:?}: expected that service's own not-found sentence, got {answered}"
    );

    // ...and the queue reported exactly one CREATE_FAILED event, carrying the refusal.
    let recorded = until_reasons_for(
        &router,
        case,
        stack,
        "Queue",
        resource_status::CREATE_FAILED,
    )
    .await;
    assert_eq!(
        recorded.len(),
        1,
        "case {case:?}: expected exactly one CREATE_FAILED event for the queue, got {recorded:?}"
    );
    let (quoted, _, class) = envelope_parts(case, &recorded[0]);
    assert_eq!(
        class, ACCESS_DENIED_CLASS,
        "case {case:?}: expected the AccessDenied class, got {class:?} out of {:?}",
        recorded[0]
    );
    let sentence = {
        let principal = format!("arn:aws:sts::{ACCOUNT_ID}:assumed-role/{role_name}/{SESSION}");
        ::service_common::iam_authorization::not_authorized_by_identity_policy(
            &principal,
            &reported_action(Owner::Queue),
            &queue_arn,
            &::service_common::iam_authorization::no_identity_policy_allows(&reported_action(
                Owner::Queue,
            )),
        )
    };
    assert!(
        quoted.contains(&sentence),
        "case {case:?}: expected the refusal {sentence:?} in the reason, got {quoted:?}"
    );
    assert_sdk_diagnostic_with_attempts(case, &quoted, SQS_SDK_NAME, UNAUTHORIZED_STATUS);
}
