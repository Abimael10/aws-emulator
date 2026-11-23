// SPDX-License-Identifier: CC0-1.0

//! What a stack create a DEPLOY-TIME GATE stops leaves behind — nothing — read back through the
//! services that OWN the resources its template declared.
//!
//! RULE: a resource CloudFormation cannot carry out is failed with the refusal's own words
//! (`CREATE_FAILED`, that refusal recorded as the resource's `ResourceStatusReason`), and under
//! the default rollback behaviour the whole create is rolled back to `ROLLBACK_COMPLETE` — so
//! every resource that declaration would have produced is ABSENT from the service that owns it,
//! which answers its own not-found refusal for a read of it. The other half of the same rule:
//! a stack reports a resource created only where the owning service really holds it, so the
//! identical template nothing stops reaches `CREATE_COMPLETE` and what it produced is held
//! there AND usable — a declared function RUNS and answers what its handler computed.
//!
//! Why it matters: a client uses a deployment as a GATE. A stack that reports success for work
//! that never happened, or that rolls back while leaving the half-made resource in place, turns
//! the one signal a deploy pipeline has into the opposite of what the account holds.
//!
//! The rows span the gates a deployment is stopped by, each beside the positive control that
//! differs from it in ONE thing:
//!
//! 1. the `TemplateURL` of an `AWS::CloudFormation::Stack` that is not an https Amazon S3
//!    location (an S3 ARN, and the same object over plain http) — the control being the
//!    identical child addressed by its https URL, whose topic Amazon SNS then holds;
//! 2. an `AWS::Lambda::Function` whose `Role` the CALLING PRINCIPAL holds no `iam:PassRole`
//!    for — the control being the same template deployed by a principal that may pass it,
//!    whose function AWS Lambda then holds and runs, over a table of payloads;
//! 3. a `Custom::` resource whose Lambda provider answers `FAILED` because the AWS call its
//!    handler makes is refused to its execution role — the control being the same provider
//!    whose role grants that call, whose object Amazon S3 then holds.
//!
//! Five services have to agree for that to hold, and this crate's harness mounts them all on
//! one engine: CloudFormation deploys and rolls back, IAM holds the roles and decides what the
//! caller may pass, AWS Lambda holds and runs the functions, Amazon S3 holds the staged child
//! template and the object a provider writes, and Amazon SNS holds the child's topic.
//!
//! Nothing here sleeps for a span the emulator owes: every wait is a poll for the CONDITION
//! under this target's shared generous deadline, and no case asserts how long anything took.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

use crate::harness::{call_as, failed_resource_reason};
use crate::hotswapped_function_code::lambda;
// ONE reader of a restJson1 error envelope in this target, owned by the suite that first needed
// it: what AWS Lambda refuses a read of a function it does not hold with is read the same way
// here.
use crate::implicit_log_group_collision::refusal as json_refusal;
use crate::nested_stacks::{staged_url, STACK_RESOURCE_TYPE};
use crate::stack_declared_function_identity::{get_function, trust_policy};
use crate::stack_lifecycle::{
    acknowledging_iam, call, create_stack, iam, member, router, settled_stack, sns,
    user_with_inline_policy, xml_blocks, xml_text, ACCOUNT_ID, REGION,
};
use crate::stack_resource_provisioning::s3;
use crate::stack_resources::sns_arn;
use crate::template_by_reference::{create_staging_bucket, stage, STAGING_BUCKET};

/// The runtime and handler every function below is declared with, and so the language of the
/// inline source it carries.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// How AWS Lambda refuses a read of a function it does not hold: the modelled
/// `ResourceNotFoundException` (httpError 404) and the fixed text of its message, whose one
/// variable part is the ARN of the function the request named.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";
const FUNCTION_NOT_FOUND_PREFIX: &str = "Function not found: ";

/// The `ResourceStatus` the resource of one logical id is reported at by
/// `ListStackResources` — "the summary information for stacks whose status matches" — or the
/// empty string where the stack reports no such resource at all.
async fn listed_status(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (code, body) = call(
        router,
        "ListStackResources",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: ListStackResources({stack}) must answer 200, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .find(|entry| xml_text(entry, "LogicalResourceId").as_deref() == Some(logical_id))
        .and_then(|entry| xml_text(entry, "ResourceStatus"))
        .unwrap_or_default()
}

/// The `StackStatus` of the stack once its operation has settled.
async fn settled_status(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let described = settled_stack(router, case, stack).await;
    xml_text(&described, "StackStatus").unwrap_or_else(|| {
        panic!("case {case:?}: expected a settled {stack} to report a status, got {described}")
    })
}

/// Whether AWS Lambda holds `function`, and the refusal it answered where it does not.
async fn lambda_holds(router: &MultiServiceRouter, function: &str) -> (bool, (String, String)) {
    let (code, body) = get_function(router, function).await;
    (code == 200, json_refusal(&body))
}

/// The ARN AWS Lambda words a function's absence with, built from the name the request named
/// and the account and Region this emulator answers in.
fn function_arn(function: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT_ID}:function:{function}")
}

/// Assert AWS Lambda holds nothing at `function`, and refuses the read as the model declares.
async fn assert_no_function(router: &MultiServiceRouter, case: &str, function: &str) {
    let (code, body) = get_function(router, function).await;
    assert_eq!(
        code, 404,
        "case {case:?}: expected AWS Lambda to hold no function {function:?} after the \
         rollback, got {code} with {body}"
    );
    let expected = (
        RESOURCE_NOT_FOUND.to_string(),
        format!("{FUNCTION_NOT_FOUND_PREFIX}{}", function_arn(function)),
    );
    let answered = json_refusal(&body);
    assert_eq!(
        answered, expected,
        "case {case:?}: expected the modelled refusal {expected:?}, got {answered:?} ({body})"
    );
}

// ---------------------------------------------------------------------------------------------
// Gate 1: the location a nested stack's template is addressed by.
// ---------------------------------------------------------------------------------------------

/// The logical id every parent below declares its nested stack under.
const NESTED: &str = "Child";

/// The child template every row addresses — the SAME object, so a row differs only in HOW it
/// addresses it. Its one topic is named by a parameter the parent passes down, so each row's
/// child would create a topic of its own and the read that asks whether it exists cannot be
/// answered by another row's.
const CHILD_KEY: &str = "gated-child-template.json";

/// The sentence CloudFormation words a `TemplateURL` that is not an https Amazon S3 location
/// with. Source: `CreateStackInput$TemplateURL`
/// (`smithy_query(query="doc",shape="CreateStackInput",member="TemplateURL")`) — "the location
/// for an Amazon S3 bucket must start with `https://`". Entirely fixed text: nothing of the
/// declaration is interpolated into it.
const S3_LOCATION_SCHEME: &str = "The location for an Amazon S3 bucket must start with https://";

fn child_template() -> String {
    json!({
        "Parameters": {"TopicName": {"Type": "String"}},
        "Resources": {
            "Signals": {
                "Type": "AWS::SNS::Topic",
                "Properties": {"TopicName": {"Ref": "TopicName"}},
            },
        },
    })
    .to_string()
}

fn parent_template(template_url: &str, topic: &str) -> String {
    json!({
        "Resources": {
            NESTED: {
                "Type": STACK_RESOURCE_TYPE,
                "Properties": {
                    "TemplateURL": template_url,
                    "Parameters": {"TopicName": topic},
                },
            },
        },
    })
    .to_string()
}

/// One row: how the parent addresses the staged child, and whether CloudFormation reads it.
struct Location {
    case: &'static str,
    stack: &'static str,
    topic: &'static str,
    url: String,
    /// The `ResourceStatusReason` the nested stack's `CREATE_FAILED` must carry, exactly, or
    /// `None` for the location CloudFormation reads.
    refused_with: Option<&'static str>,
}

fn locations() -> Vec<Location> {
    vec![
        Location {
            // An ARN names the object but is not a location at all.
            case: "an Amazon S3 ARN rather than an https URL",
            stack: "gate-nested-arn",
            topic: "gate-nested-arn-topic",
            url: format!("arn:aws:s3:::{STAGING_BUCKET}/{CHILD_KEY}"),
            refused_with: Some(S3_LOCATION_SCHEME),
        },
        Location {
            // The same object, addressed over a scheme the rule does not admit.
            case: "the same object over plain http",
            stack: "gate-nested-http",
            topic: "gate-nested-http-topic",
            url: format!("http://s3.{REGION}.amazonaws.com/{STAGING_BUCKET}/{CHILD_KEY}"),
            refused_with: Some(S3_LOCATION_SCHEME),
        },
        Location {
            // The control: the identical child by its https URL.
            case: "the control: the same child by its https URL",
            stack: "gate-nested-https",
            topic: "gate-nested-https-topic",
            url: staged_url(CHILD_KEY),
            refused_with: None,
        },
    ]
}

#[tokio::test]
async fn a_nested_template_location_cloudformation_refuses_creates_no_child_resource() {
    for row in locations() {
        let case = row.case;
        let router = router();
        create_staging_bucket(&router).await;
        stage(&router, CHILD_KEY, &child_template()).await;

        let (code, body) = create_stack(
            &router,
            row.stack,
            &parent_template(&row.url, row.topic),
            &[],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the parent must be accepted — the decision is the RESOURCE's, not \
             the request's — got {code} {body}"
        );

        let Some(reason) = row.refused_with else {
            // The control: the child is read, so the parent completes and the topic the child
            // declared is one Amazon SNS really holds.
            let reported = settled_status(&router, case, row.stack).await;
            assert_eq!(
                reported,
                status::CREATE_COMPLETE,
                "case {case:?}: expected the parent to complete, got {reported:?}"
            );
            let listed = listed_status(&router, case, row.stack, NESTED).await;
            assert_eq!(
                listed,
                resource_status::CREATE_COMPLETE,
                "case {case:?}: expected {NESTED} to be reported created, got {listed:?}"
            );
            let (code, attributes) =
                sns(&router, "GetTopicAttributes", &[("TopicArn", &sns_arn(row.topic))]).await;
            assert_eq!(
                code, 200,
                "case {case:?}: expected Amazon SNS to hold the topic the child created, got \
                 {code} {attributes}"
            );
            continue;
        };

        // The gate: the nested stack fails with CloudFormation's own sentence for the location,
        // the parent rolls back, and the resource is reported gone.
        let recorded = failed_resource_reason(
            case,
            &router,
            REGION,
            row.stack,
            NESTED,
            resource_status::CREATE_FAILED,
        )
        .await;
        assert_eq!(
            recorded, reason,
            "case {case:?}: expected {NESTED} to record {reason:?}, got {recorded:?}"
        );
        let reported = settled_status(&router, case, row.stack).await;
        assert_eq!(
            reported,
            status::ROLLBACK_COMPLETE,
            "case {case:?}: expected the parent to roll back, got {reported:?}"
        );
        let listed = listed_status(&router, case, row.stack, NESTED).await;
        assert_eq!(
            listed,
            resource_status::DELETE_COMPLETE,
            "case {case:?}: expected {NESTED} to be reported removed by the rollback, got \
             {listed:?}"
        );

        // ...and nothing the child would have created exists: Amazon SNS holds no topic at the
        // name the child was to create it under.
        let (code, attributes) =
            sns(&router, "GetTopicAttributes", &[("TopicArn", &sns_arn(row.topic))]).await;
        assert_eq!(
            code, 404,
            "case {case:?}: expected Amazon SNS to hold no topic {:?}, got {code} {attributes}",
            row.topic
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Gate 2: the `iam:PassRole` the calling principal needs for a role the template hands over,
// and what the function a completed deployment created DOES.
// ---------------------------------------------------------------------------------------------

/// The logical ids every template of this gate declares its two resources under.
const EXEC_ROLE: &str = "ExecRole";
const WORKER: &str = "Worker";

/// The action a resource that hands an IAM role to its owning service needs of the caller.
const PASS_ROLE: &str = "iam:PassRole";

/// The handler the deployed function runs: it COMPUTES its answer from the event it is given,
/// so an answer carrying the total of the numbers sent can only have come from a run of this
/// code — it is in neither the request nor the deployment package's text.
const ADDS_UP: &str = "def handler(event, context):\n    numbers = event[\"numbers\"]\n    return \
                       {\"total\": sum(numbers), \"count\": len(numbers)}\n";

fn passing_template(role: &str, function: &str) -> String {
    json!({
        "Resources": {
            EXEC_ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": role,
                    "AssumeRolePolicyDocument": trust_policy(),
                },
            },
            WORKER: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": {"Fn::GetAtt": [EXEC_ROLE, "Arn"]},
                    "Code": {"ZipFile": ADDS_UP},
                },
            },
        },
    })
    .to_string()
}

/// One row: what the caller's own policy says about passing the role its template hands to AWS
/// Lambda.
struct Caller {
    case: &'static str,
    user: &'static str,
    stack: &'static str,
    role: &'static str,
    function: &'static str,
    /// Whether the caller's policy grants `iam:PassRole` over the role the template passes.
    may_pass: bool,
}

const CALLERS: &[Caller] = &[
    Caller {
        case: "a caller holding no iam:PassRole for the role its template passes",
        user: "gate-passrole-denied-user",
        stack: "gate-passrole-denied",
        role: "gate-passrole-denied-role",
        function: "gate-passrole-denied-worker",
        may_pass: false,
    },
    Caller {
        case: "the control: the same template under a grant of iam:PassRole on that role",
        user: "gate-passrole-granted-user",
        stack: "gate-passrole-granted",
        role: "gate-passrole-granted-role",
        function: "gate-passrole-granted-worker",
        may_pass: true,
    },
];

/// The policy one row's caller carries: the CloudFormation grant every row needs to reach the
/// operation at all, plus the `iam:PassRole` grant the row either holds or does not — scoped to
/// the role its own template passes, which is the resource the permission is about.
fn caller_policy(row: &Caller) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": "cloudformation:*",
        "Resource": "*",
    })];
    if row.may_pass {
        statements.push(json!({
            "Effect": "Allow",
            "Action": PASS_ROLE,
            "Resource": format!("arn:aws:iam::{ACCOUNT_ID}:role/{}", row.role),
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// One synchronous invocation of `function` carrying `payload`, which must RUN to completion:
/// a run that raised carries the `X-Amz-Function-Error` signal instead.
async fn invoked(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    payload: &Value,
) -> Value {
    let answered = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        payload.to_string().as_bytes(),
        &[("x-amz-invocation-type", "RequestResponse")],
    )
    .await;
    assert_eq!(
        (answered.status, answered.function_error()),
        (200, None),
        "case {case:?}: expected the deployed function to run and answer, got {} with {}",
        answered.status,
        answered.body
    );
    answered.body
}

/// The payloads the deployed function is invoked with, and what its handler computes for each —
/// the rows differ in the COUNT of numbers sent, their signs and their magnitudes, so no single
/// memorised answer satisfies them.
fn payloads() -> Vec<(Vec<i64>, i64)> {
    vec![
        (vec![1, 2, 3], 6),
        (vec![], 0),
        (vec![-5, 10], 5),
        (vec![7], 7),
        (vec![1_000_000, 2_000_000, 3_000_000, 4_000_000], 10_000_000),
    ]
}

#[tokio::test]
async fn a_role_the_caller_may_not_pass_fails_the_function_and_lambda_holds_none() {
    for row in CALLERS {
        let case = row.case;
        let router = router();
        let access_key_id = user_with_inline_policy(
            &router,
            case,
            row.user,
            "what-the-caller-may-pass",
            &caller_policy(row),
        )
        .await;

        let (code, body) = call_as(
            &router,
            &access_key_id,
            "CreateStack",
            &acknowledging_iam(&[
                member("StackName", row.stack),
                member("TemplateBody", &passing_template(row.role, row.function)),
            ]),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the create must be accepted — the decision is the RESOURCE's, not \
             the request's — got {code} {body}"
        );

        if !row.may_pass {
            // The gate: the resource that handed the role over fails, carrying the refusal of
            // the hand-over, and the stack rolls back.
            let reason = failed_resource_reason(
                case,
                &router,
                REGION,
                row.stack,
                WORKER,
                resource_status::CREATE_FAILED,
            )
            .await;
            assert!(
                reason.contains(PASS_ROLE),
                "case {case:?}: expected {WORKER} to report the {PASS_ROLE} refusal, got \
                 {reason:?}"
            );
            assert!(
                reason.contains(&format!(":role/{}", row.role)),
                "case {case:?}: expected the refusal to name the role it is about, got {reason:?}"
            );
            let reported = settled_status(&router, case, row.stack).await;
            assert_eq!(
                reported,
                status::ROLLBACK_COMPLETE,
                "case {case:?}: expected the stack to roll back, got {reported:?}"
            );

            // ...and neither resource exists: AWS Lambda holds no function, and the role the
            // rollback could delete is gone from IAM too.
            assert_no_function(&router, case, row.function).await;
            let (code, body) = iam(&router, "GetRole", &[("RoleName", row.role)]).await;
            assert_eq!(
                code, 404,
                "case {case:?}: expected IAM to hold no role {:?} after the rollback, got {code} \
                 {body}",
                row.role
            );
            continue;
        }

        // The control: the hand-over is allowed, so the stack completes and AWS Lambda really
        // holds the function...
        let reported = settled_status(&router, case, row.stack).await;
        assert_eq!(
            reported,
            status::CREATE_COMPLETE,
            "case {case:?}: expected the stack to complete, got {reported:?}"
        );
        let (held, refused) = lambda_holds(&router, row.function).await;
        assert!(
            held,
            "case {case:?}: expected AWS Lambda to hold {:?} once the stack completed, got \
             {refused:?}",
            row.function
        );

        // ...and the function RUNS: every payload is answered by what the handler computed from
        // it, which is in neither the request nor the deployment package.
        for (numbers, total) in payloads() {
            let answered = invoked(
                &router,
                case,
                row.function,
                &json!({"numbers": numbers.clone()}),
            )
            .await;
            let expected = json!({"total": total, "count": numbers.len()});
            assert_eq!(
                answered, expected,
                "case {case:?}: expected the run of {numbers:?} to answer {expected}, got \
                 {answered}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Gate 3: the answer a custom resource's own provider sends.
// ---------------------------------------------------------------------------------------------

/// The logical ids the templates of this gate declare their resources under.
const PROVIDER_ROLE: &str = "ProviderRole";
const PROVIDER: &str = "Provider";
const WRITER: &str = "Writer";

/// The provider the stack itself declares: it writes the object its properties name with the
/// SDK its runtime supplies, and answers CloudFormation through the response module
/// CloudFormation supplies to inline code — `SUCCESS` when the write was served, and `FAILED`
/// carrying the refusal it was answered with when it was not. A `Delete` has nothing of its own
/// to undo, so it is answered straight away, which is what lets the rollback finish.
const WRITES_ITS_OBJECT: &str = r#"
import boto3
import cfnresponse


def handler(event, context):
    properties = event["ResourceProperties"]
    identifier = "writer-" + event["LogicalResourceId"]
    if event["RequestType"] == "Delete":
        cfnresponse.send(
            event,
            context,
            cfnresponse.SUCCESS,
            {},
            physicalResourceId=identifier,
        )
        return
    try:
        boto3.client("s3").put_object(
            Bucket=properties["Bucket"],
            Key=properties["Key"],
            Body=properties["Body"].encode("utf-8"),
        )
    except Exception as refused:
        cfnresponse.send(
            event,
            context,
            cfnresponse.FAILED,
            {},
            physicalResourceId=identifier,
            reason=str(refused),
        )
        return
    cfnresponse.send(
        event,
        context,
        cfnresponse.SUCCESS,
        {"WrittenKey": properties["Key"]},
        physicalResourceId=identifier,
    )
"#;

/// The fixed text CloudFormation wraps an ANSWERED failure in: the provider's own reason, and
/// the id of the request it answered, sit inside it.
const ANSWERED_OPENS: &str =
    "Received response status [FAILED] from custom resource. Message returned: ";

/// The error code Amazon S3 refuses a call no identity-based policy allows with, which is what
/// the provider's own reason carries into that envelope.
const ACCESS_DENIED: &str = "AccessDenied";

/// One row: whether the provider's execution role grants the write its handler makes.
struct Provider {
    case: &'static str,
    stack: &'static str,
    bucket: &'static str,
    key: &'static str,
    body: &'static str,
    function: &'static str,
    resource_type: &'static str,
    grants_the_write: bool,
}

const PROVIDERS: &[Provider] = &[
    Provider {
        case: "a provider whose execution role grants no object write",
        stack: "gate-provider-denied",
        bucket: "gate-provider-denied-bucket",
        key: "seeded/v1.json",
        body: r#"{"seeded": false}"#,
        function: "gate-provider-denied-handler",
        resource_type: "Custom::ObjectWriter",
        grants_the_write: false,
    },
    Provider {
        case: "the control: the same provider whose role grants that write",
        stack: "gate-provider-granted",
        bucket: "gate-provider-granted-bucket",
        key: "seeded/v1.json",
        body: "a body, with spaces & a ✓",
        function: "gate-provider-granted-handler",
        resource_type: "AWS::CloudFormation::CustomResource",
        grants_the_write: true,
    },
];

/// The statements one row's execution role carries: the log writes every execution role needs,
/// plus the object write where the row grants it.
fn provider_policy(row: &Provider) -> Value {
    let mut statements = vec![json!({"Effect": "Allow", "Action": "logs:*", "Resource": "*"})];
    if row.grants_the_write {
        statements.push(json!({
            "Effect": "Allow",
            "Action": "s3:PutObject",
            "Resource": "*",
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements})
}

/// The template one row deploys: the execution role, the provider function the stack itself
/// creates, and the custom resource that function backs.
fn provider_template(row: &Provider) -> String {
    json!({
        "Resources": {
            PROVIDER_ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [{
                        "PolicyName": "what-the-provider-may-do",
                        "PolicyDocument": provider_policy(row),
                    }],
                },
            },
            PROVIDER: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": row.function,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": {"Fn::GetAtt": [PROVIDER_ROLE, "Arn"]},
                    "Code": {"ZipFile": WRITES_ITS_OBJECT},
                    "Timeout": 60,
                },
            },
            WRITER: {
                "Type": row.resource_type,
                "DependsOn": [PROVIDER],
                "Properties": {
                    "ServiceToken": {"Fn::GetAtt": [PROVIDER, "Arn"]},
                    "Bucket": row.bucket,
                    "Key": row.key,
                    "Body": row.body,
                },
            },
        },
    })
    .to_string()
}

#[tokio::test]
async fn a_provider_that_answers_failed_rolls_the_stack_back_and_its_write_never_happened() {
    for row in PROVIDERS {
        let case = row.case;
        let router = router();
        // The bucket is a client's own, created OUTSIDE the stack: it outlives the rollback, so
        // what the read of the key answers is about the object rather than about the bucket.
        let created = s3(&router, "PUT", &format!("/{}", row.bucket), b"").await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the bucket must be created first, got {} {}",
            created.status, created.body
        );

        let (code, body) = create_stack(
            &router,
            row.stack,
            &provider_template(row),
            &acknowledging_iam(&[]),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected CreateStack to be accepted, got {code} {body}"
        );

        if row.grants_the_write {
            // The control: the write is served, the provider answers SUCCESS, and the object it
            // wrote is one Amazon S3 holds with the declared body.
            let reported = settled_status(&router, case, row.stack).await;
            assert_eq!(
                reported,
                status::CREATE_COMPLETE,
                "case {case:?}: expected the stack to complete on its provider's answer, got \
                 {reported:?}"
            );
            let read = s3(
                &router,
                "GET",
                &format!("/{}/{}", row.bucket, row.key),
                b"",
            )
            .await;
            assert_eq!(
                (read.status, read.body.as_str()),
                (200, row.body),
                "case {case:?}: expected the object the provider wrote at {:?} to carry the \
                 declared body, got {} with {:?}",
                row.key,
                read.status,
                read.body
            );
            continue;
        }

        // The gate: the provider answers FAILED with the refusal of the call it made, which
        // CloudFormation records on the resource inside its own envelope for an answered
        // failure.
        let reason = failed_resource_reason(
            case,
            &router,
            REGION,
            row.stack,
            WRITER,
            resource_status::CREATE_FAILED,
        )
        .await;
        assert!(
            reason.starts_with(ANSWERED_OPENS),
            "case {case:?}: expected the reason to open with {ANSWERED_OPENS:?}, got {reason:?}"
        );
        assert!(
            reason.contains(ACCESS_DENIED) && reason.contains("PutObject"),
            "case {case:?}: expected the provider's own reason to name the refused call, got \
             {reason:?}"
        );

        // ...the stack rolls back, nothing was written at the key, and the provider the stack
        // created is gone from AWS Lambda with it.
        let reported = settled_status(&router, case, row.stack).await;
        assert_eq!(
            reported,
            status::ROLLBACK_COMPLETE,
            "case {case:?}: expected the failed create to be rolled back, got {reported:?}"
        );
        let head = s3(
            &router,
            "HEAD",
            &format!("/{}/{}", row.bucket, row.key),
            b"",
        )
        .await;
        assert_eq!(
            head.status, 404,
            "case {case:?}: expected Amazon S3 to hold nothing at {:?}, got {} {}",
            row.key, head.status, head.body
        );
        assert_no_function(&router, case, row.function).await;
    }
}
