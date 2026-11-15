// SPDX-License-Identifier: CC0-1.0

//! The RDS-credential estate deployed as ONE CloudFormation stack: a generated secret, the DB
//! instance whose master credential comes out of it through a dynamic reference, the target
//! attachment that binds the two, the rotation function with its execution role and invoke
//! grant, and the schedule that binds that function to the secret.
//!
//! The rule under test: **a rotation function PUTS THE NEW CREDENTIAL IN FORCE in the service
//! that owns the database — for an RDS master credential that is `ModifyDBInstance` on the
//! instance the secret belongs to — so the AWS SDK the managed runtime supplies has to reach
//! Amazon RDS from inside the invocation.** A handler whose `setSecret` step cannot make that
//! call raises, the rotation never finishes, and everything that waits on it waits for ever:
//! the stack's own `AWS::SecretsManager::RotationSchedule` resource is held at
//! `CREATE_IN_PROGRESS` while a rotation of its secret is in flight, so the STACK never reaches
//! a terminal status either, and a caller polling `GetSecretValue(VersionStage="AWSCURRENT")`
//! never sees a new version.
//!
//! Six crates have to agree for that to hold — CloudFormation deploys the estate, Amazon RDS
//! owns the database and takes the credential change, Secrets Manager holds the secret and
//! drives the rotation, Lambda runs the function and supplies it the SDK, IAM answers for its
//! execution role and for the invoke grant, STS mints the session its calls are signed with —
//! so the suite lives here rather than in any one of them.
//!
//! The rows differ along the SHAPE of the rotation handler: one written against this service's
//! own operations directly, and one written the way the AWS-published single-user rotation
//! template is (guarding every step on the secret's own metadata and drawing the new credential
//! from Secrets Manager's own generator). Both put the credential in force in Amazon RDS, which
//! is what the rule is about; `lambda_handler_reaches_amazon_rds` is the suite that states the
//! rule for that call itself, and `cloudformation_rotation_schedule_rotates_its_secret` the one
//! for the rotation protocol.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, opt_element, outputs_of, pairs_of};

/// The Region every request here is signed for, and so the Region the estate lives in.
const REGION: &str = "us-west-2";

/// The username the secret is generated around, and the key the generated password lands
/// under.
const USERNAME: &str = "dbadmin";
const PASSWORD_KEY: &str = "password";

/// How long one poll waits before the next, and how many are made: every transition waited for
/// here is owed on another service's own clock, so the suite waits for the CONDITION rather
/// than for a span, with a budget far longer than the spans themselves so a loaded host does
/// not decide the outcome.
const POLL: Duration = Duration::from_millis(50);
/// See [`POLL`].
const POLLS: usize = 2_400;

/// The `StackStatus` a create reports while it runs, and the one a completed create reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `ResourceStatus` every resource of a completed create reports.
const RESOURCE_CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `DBInstanceStatus` an instance that serves connections is in.
const AVAILABLE: &str = "available";

/// How long one invocation of a rotation function is given, as the function's own `Timeout`:
/// well past what a run needs, because what is under test is the rotation and not how fast an
/// interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// rotation function's own SDK calls are dispatched back into this same router, so the secret
/// it writes and the database it changes are the ones this stack deployed.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// One AWS Secrets Manager call over its awsJson1_1 wire.
async fn secretsmanager(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("secretsmanager.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header("authorization", authorization("secretsmanager"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// A rotation handler written against the two services it has to reach directly: it stages the
/// new credential under `AWSPENDING`, puts it in force on the DB instance the environment names
/// and moves `AWSCURRENT` onto the version it wrote.
const DIRECT_ROTATION_FUNCTION: &str = r##"
import json
import os

import boto3


def lambda_handler(event, context):
    arn = event["SecretId"]
    token = event["ClientRequestToken"]
    step = event["Step"]
    secrets = boto3.client("secretsmanager")
    metadata = secrets.describe_secret(SecretId=arn)
    versions = metadata["VersionIdsToStages"]
    if step == "createSecret":
        current = json.loads(
            secrets.get_secret_value(SecretId=arn, VersionStage="AWSCURRENT")["SecretString"]
        )
        current["password"] = secrets.get_random_password(
            PasswordLength=24, ExcludePunctuation=True
        )["RandomPassword"]
        secrets.put_secret_value(
            SecretId=arn,
            ClientRequestToken=token,
            SecretString=json.dumps(current),
            VersionStages=["AWSPENDING"],
        )
    elif step == "setSecret":
        pending = json.loads(
            secrets.get_secret_value(
                SecretId=arn, VersionId=token, VersionStage="AWSPENDING"
            )["SecretString"]
        )
        boto3.client("rds").modify_db_instance(
            DBInstanceIdentifier=os.environ["DB_INSTANCE_IDENTIFIER"],
            MasterUserPassword=pending["password"],
            ApplyImmediately=True,
        )
    elif step == "finishSecret":
        previous = None
        for version, stages in versions.items():
            if "AWSCURRENT" in stages:
                previous = version
        secrets.update_secret_version_stage(
            SecretId=arn,
            VersionStage="AWSCURRENT",
            MoveToVersionId=token,
            RemoveFromVersionId=previous,
        )
    return {"step": step}
"##;

/// The same rotation, written the way the AWS-published single-user rotation template is: every
/// step guarded on the secret's own metadata, the new credential drawn from Secrets Manager's
/// own generator, and the instance read back out of Amazon RDS before the change is made.
const AWS_STYLE_ROTATION_FUNCTION: &str = r##"
import json
import logging
import os

import boto3

logger = logging.getLogger()
logger.setLevel(logging.INFO)


def lambda_handler(event, context):
    arn = event['SecretId']
    token = event['ClientRequestToken']
    step = event['Step']

    service_client = boto3.client('secretsmanager')
    metadata = service_client.describe_secret(SecretId=arn)
    if not metadata['RotationEnabled']:
        raise ValueError("Secret %s is not enabled for rotation" % arn)
    versions = metadata['VersionIdsToStages']
    if token not in versions:
        raise ValueError(
            "Secret version %s has no stage for rotation of secret %s." % (token, arn))
    if "AWSCURRENT" in versions[token]:
        logger.info("Version %s already set as AWSCURRENT for secret %s." % (token, arn))
        return
    elif "AWSPENDING" not in versions[token]:
        raise ValueError(
            "Secret version %s not set as AWSPENDING for rotation of secret %s." % (token, arn))

    if step == "createSecret":
        create_secret(service_client, arn, token)
    elif step == "setSecret":
        set_secret(service_client, arn, token)
    elif step == "testSecret":
        test_secret(service_client, arn, token)
    elif step == "finishSecret":
        finish_secret(service_client, arn, token)
    else:
        raise ValueError("Invalid step parameter %s" % step)


def pending(service_client, arn, token):
    return json.loads(
        service_client.get_secret_value(
            SecretId=arn, VersionId=token, VersionStage="AWSPENDING"
        )["SecretString"]
    )


def create_secret(service_client, arn, token):
    current = json.loads(
        service_client.get_secret_value(SecretId=arn, VersionStage="AWSCURRENT")["SecretString"]
    )
    try:
        pending(service_client, arn, token)
        logger.info("createSecret: a staged value is already held for %s." % arn)
    except service_client.exceptions.ResourceNotFoundException:
        current['password'] = service_client.get_random_password(
            PasswordLength=24, ExcludePunctuation=True
        )['RandomPassword']
        service_client.put_secret_value(
            SecretId=arn,
            ClientRequestToken=token,
            SecretString=json.dumps(current),
            VersionStages=['AWSPENDING'],
        )


def set_secret(service_client, arn, token):
    staged = pending(service_client, arn, token)
    identifier = os.environ['DB_INSTANCE_IDENTIFIER']
    rds = boto3.client('rds')
    described = rds.describe_db_instances(DBInstanceIdentifier=identifier)
    held = described['DBInstances'][0]
    if held['MasterUsername'] != staged['username']:
        raise ValueError("the staged credential is not the instance's master user")
    rds.modify_db_instance(
        DBInstanceIdentifier=identifier,
        MasterUserPassword=staged['password'],
        ApplyImmediately=True,
    )


def test_secret(service_client, arn, token):
    staged = pending(service_client, arn, token)
    if not staged['password']:
        raise ValueError("the staged credential carries no password")


def finish_secret(service_client, arn, token):
    metadata = service_client.describe_secret(SecretId=arn)
    current_version = None
    for version in metadata["VersionIdsToStages"]:
        if "AWSCURRENT" in metadata["VersionIdsToStages"][version]:
            if version == token:
                return
            current_version = version
            break
    service_client.update_secret_version_stage(
        SecretId=arn,
        VersionStage="AWSCURRENT",
        MoveToVersionId=token,
        RemoveFromVersionId=current_version,
    )
"##;

/// One row of the table: the rotation handler the stack deploys, and the stack it deploys under.
struct Row {
    case: &'static str,
    /// The stack, and the prefix every resource of this row is named under.
    stack: &'static str,
    /// The rotation handler's source, archived into the function's deployment package by
    /// CloudFormation.
    handler: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a rotation handler written against the two services it reaches",
        stack: "xsvc-rds-rotation-direct",
        handler: DIRECT_ROTATION_FUNCTION,
    },
    Row {
        case: "the AWS-published single-user rotation handler shape",
        stack: "xsvc-rds-rotation-aws-style",
        handler: AWS_STYLE_ROTATION_FUNCTION,
    },
];

/// The template one row deploys: the generated secret, the DB instance whose master credential
/// it holds, the attachment binding the two, the rotation function with its role and invoke
/// grant, and the schedule that puts the rotation in force.
fn template(row: &Row) -> String {
    let stack = row.stack;
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "an RDS master credential a rotation function rotates",
        "Resources": {
            "DbSecret": {
                "Type": "AWS::SecretsManager::Secret",
                "Properties": {
                    "Name": format!("{stack}-credentials"),
                    "GenerateSecretString": {
                        "SecretStringTemplate": json!({ "username": USERNAME }).to_string(),
                        "GenerateStringKey": PASSWORD_KEY,
                        "PasswordLength": 20,
                        "ExcludePunctuation": true,
                    },
                },
            },
            "DbInstance": {
                "Type": "AWS::RDS::DBInstance",
                "Properties": {
                    "DBInstanceIdentifier": format!("{stack}-db"),
                    "Engine": "mysql",
                    "DBInstanceClass": "db.t3.micro",
                    "AllocatedStorage": "20",
                    "MasterUsername": {
                        "Fn::Sub": "{{resolve:secretsmanager:${DbSecret}:SecretString:username}}",
                    },
                    "MasterUserPassword": {
                        "Fn::Sub": "{{resolve:secretsmanager:${DbSecret}:SecretString:password}}",
                    },
                },
            },
            "SecretAttachment": {
                "Type": "AWS::SecretsManager::SecretTargetAttachment",
                "Properties": {
                    "SecretId": { "Ref": "DbSecret" },
                    "TargetId": { "Ref": "DbInstance" },
                    "TargetType": "AWS::RDS::DBInstance",
                },
            },
            "RotationRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "rotate-the-credential",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [
                                {
                                    "Effect": "Allow",
                                    "Action": "secretsmanager:*",
                                    "Resource": "*",
                                },
                                {
                                    // What the `setSecret` step needs: the credential change
                                    // on the instance the secret belongs to.
                                    "Effect": "Allow",
                                    "Action": [
                                        "rds:DescribeDBInstances",
                                        "rds:ModifyDBInstance",
                                    ],
                                    "Resource": "*",
                                },
                                {
                                    "Effect": "Allow",
                                    "Action": [
                                        "logs:CreateLogGroup",
                                        "logs:CreateLogStream",
                                        "logs:PutLogEvents",
                                    ],
                                    "Resource": "*",
                                },
                            ],
                        },
                    }],
                },
            },
            "RotationFn": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": { "Fn::GetAtt": ["RotationRole", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {
                        "Variables": {
                            "DB_INSTANCE_IDENTIFIER": { "Ref": "DbInstance" },
                        },
                    },
                    "Code": { "ZipFile": row.handler },
                },
            },
            "InvokePermission": {
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "Action": "lambda:InvokeFunction",
                    "FunctionName": { "Fn::GetAtt": ["RotationFn", "Arn"] },
                    "Principal": "secretsmanager.amazonaws.com",
                },
            },
            // The grant has to be in force, and the credential has to be the instance's, before
            // the rotation this resource starts invokes the function.
            "SecretRotation": {
                "Type": "AWS::SecretsManager::RotationSchedule",
                "DependsOn": ["InvokePermission", "SecretAttachment"],
                "Properties": {
                    "SecretId": { "Ref": "DbSecret" },
                    "RotationLambdaARN": { "Fn::GetAtt": ["RotationFn", "Arn"] },
                    "RotationRules": { "AutomaticallyAfterDays": 1 },
                },
            },
        },
        "Outputs": {
            "SecretArn": { "Value": { "Ref": "DbSecret" } },
            "DbInstanceIdentifier": { "Value": { "Ref": "DbInstance" } },
            "FunctionArn": { "Value": { "Fn::GetAtt": ["RotationFn", "Arn"] } },
        },
    })
    .to_string()
}

/// The `StackStatus` the stack reports now, with the answer it was read from.
async fn stack_status(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {body}"
    );
    (opt_element(&body, "StackStatus").unwrap_or_default(), body)
}

/// Poll `stack` until the create has finished, in whatever status it finished in — which is
/// what the rule says it must reach: the transitions on the way are owed on the stack's own
/// clock, so this waits for the condition rather than for a span.
async fn wait_past_the_create(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let mut last = stack_status(router, stack).await;
    for _ in 0..POLLS {
        if !last.0.ends_with("_IN_PROGRESS") {
            return last;
        }
        tokio::time::sleep(POLL).await;
        last = stack_status(router, stack).await;
    }
    last
}

/// Every `ResourceStatusReason` the stack has recorded, so a deployment that did not reach the
/// status the rule states reports WHY rather than only that it did not.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&events, "ResourceStatusReason")
}

/// The `AWSCURRENT` version of `secret_id`, as the pair a client reads: its id and the password
/// the document holds.
async fn current_credential(
    router: &MultiServiceRouter,
    case: &str,
    secret_id: &str,
) -> (String, String) {
    let (status, read) = secretsmanager(
        router,
        "GetSecretValue",
        &json!({ "SecretId": secret_id, "VersionStage": "AWSCURRENT" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetSecretValue, got {status} with body {read}"
    );
    let document: Value =
        serde_json::from_str(read["SecretString"].as_str().unwrap_or_default()).unwrap_or(Value::Null);
    (
        read["VersionId"].as_str().unwrap_or_default().to_string(),
        document[PASSWORD_KEY].as_str().unwrap_or_default().to_string(),
    )
}

/// Every row: the estate deploys, the stack reaches a terminal status, and the rotation its
/// schedule started really ran — a new version holds `AWSCURRENT`, its password is not the one
/// the secret was generated with, and the secret reports when it was rotated.
#[tokio::test]
async fn the_rds_credential_estate_rotates_through_its_function() {
    for row in ROWS {
        let case = row.case;
        let router = build_router();
        let stack = row.stack;

        let body = template(row);
        let (status, answered) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", stack),
                ("TemplateBody", &body),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
        );

        // The stack must leave `*_IN_PROGRESS` at all, and in the status a deployment whose
        // resources every owning service accepted reaches.
        let (reached, described) = wait_past_the_create(&router, stack).await;
        let (_, resources) =
            cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
        assert_eq!(
            reached, CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {reached:?} with \
             resources {:?} and resource failures {:?}",
            pairs_of(&resources, "LogicalResourceId", "ResourceStatus"),
            failure_reasons(&router, stack).await
        );
        let reported = pairs_of(&resources, "LogicalResourceId", "ResourceStatus");
        for logical_id in [
            "DbSecret",
            "DbInstance",
            "SecretAttachment",
            "RotationRole",
            "RotationFn",
            "InvokePermission",
            "SecretRotation",
        ] {
            let answered = reported.get(logical_id).map(String::as_str);
            assert_eq!(
                answered,
                Some(RESOURCE_CREATE_COMPLETE),
                "case {case:?}: expected {logical_id} {RESOURCE_CREATE_COMPLETE}, got \
                 {answered:?} in {resources}"
            );
        }

        let outputs = outputs_of(&described);
        let secret_arn = outputs
            .get("SecretArn")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no SecretArn output in {described}"));
        let identifier = outputs
            .get("DbInstanceIdentifier")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no DbInstanceIdentifier output in {described}"));

        // The database the credential belongs to is the one Amazon RDS holds, with the master
        // user the secret was generated around. Its status is read until that service reports
        // it serving again: the credential change the rotation made leaves the instance
        // `modifying` for a span that is Amazon RDS's own.
        let mut instance = String::new();
        for _ in 0..POLLS {
            let (status, body) = query(
                &router,
                "rds",
                "2014-10-31",
                "DescribeDBInstances",
                &[("DBInstanceIdentifier", &identifier)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected Amazon RDS to resolve {identifier:?}, got {status} with \
                 {body}"
            );
            instance = body;
            if element(&instance, "DBInstanceStatus") == AVAILABLE {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        for (member, wanted) in [
            ("DBInstanceStatus", AVAILABLE),
            ("MasterUsername", USERNAME),
        ] {
            let answered = element(&instance, member);
            assert_eq!(
                answered, wanted,
                "case {case:?}: expected the instance's {member} {wanted:?}, got {answered:?} in \
                 {instance}"
            );
        }

        // The deployment itself started a rotation (`RotateImmediatelyOnUpdate` defaults to
        // rotating immediately), and the schedule resource is only complete once that rotation
        // is over — so by now `AWSCURRENT` is on a version the FUNCTION wrote, whose password is
        // not the generated one.
        let (rotated_version, rotated_password) =
            current_credential(&router, case, &secret_arn).await;
        let (status, metadata) =
            secretsmanager(&router, "DescribeSecret", &json!({ "SecretId": secret_arn })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeSecret, got {status} with {metadata}"
        );
        assert_eq!(
            metadata["RotationEnabled"], json!(true),
            "case {case:?}: expected the secret to report rotation enabled, got {metadata}"
        );
        assert!(
            metadata["LastRotatedDate"].is_number(),
            "case {case:?}: expected LastRotatedDate once a rotation has run, got {metadata}"
        );
        assert!(
            !rotated_password.is_empty(),
            "case {case:?}: expected the rotated credential to carry a password, got \
             {rotated_password:?}"
        );

        // ...and a rotation a CLIENT asks for rotates it again: a different version holds
        // `AWSCURRENT`, with a different password, and the version it replaced carries
        // `AWSPREVIOUS`.
        let (status, answered) = secretsmanager(
            &router,
            "RotateSecret",
            &json!({ "SecretId": secret_arn }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from RotateSecret, got {status} with {answered}"
        );
        let mut current = (rotated_version.clone(), rotated_password.clone());
        for _ in 0..POLLS {
            current = current_credential(&router, case, &secret_arn).await;
            if current.0 != rotated_version {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        assert_ne!(
            current.0, rotated_version,
            "case {case:?}: expected AWSCURRENT to move to a new version, got {current:?}"
        );
        assert_ne!(
            current.1, rotated_password,
            "case {case:?}: expected the rotated credential to be a new password, got {current:?}"
        );
        let (status, metadata) =
            secretsmanager(&router, "DescribeSecret", &json!({ "SecretId": secret_arn })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeSecret, got {status} with {metadata}"
        );
        let stages = metadata["VersionIdsToStages"][&rotated_version].clone();
        assert_eq!(
            stages,
            json!(["AWSPREVIOUS"]),
            "case {case:?}: expected the replaced version to carry AWSPREVIOUS alone, got \
             {metadata}"
        );
    }
}
