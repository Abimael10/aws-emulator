// SPDX-License-Identifier: CC0-1.0

//! The MIXED-TRUST pair of IAM roles on a CloudFormation-deployed Aurora DB cluster: the one
//! Amazon RDS associates, and the one it refuses.
//!
//! The rule under test: **a DB cluster's IAM-role association is decided by the TRUST POLICY of
//! the role IAM holds, and the decision is stable.** A role whose trust policy grants
//! `sts:AssumeRole` to the Amazon RDS service principal is associated by the FIRST
//! `AddRoleToDBCluster` call and is then reported on the cluster by `DescribeDBClusters` under
//! `AssociatedRoles` with the TERMINAL `Status` an association that holds carries (`ACTIVE`;
//! `DBClusterRole$Status`' own documentation, read with
//! `smithy_query(query="shape", shape="DBClusterRole")`, states the three values and that
//! `PENDING` is the non-terminal one). A role whose trust policy does not grant it is refused —
//! the Query API's `InvalidParameterValue` with the one fixed sentence that refusal carries —
//! every time it is asked, with no association recorded for it either way.
//!
//! Three crates have to agree for this to hold: CloudFormation deploys the cluster and both
//! roles, IAM holds each role and its trust document, and Amazon RDS decides the association
//! against what IAM answers — so the suite lives here rather than in any one of them. What a
//! single service decides on its own is pinned by its own crate: the trust-document FORMS by
//! `postgres_s3_import_role_trust` and the order the role and the feature name are judged in by
//! `role_association_role_gate_order`, both in
//! `services/service-com-amazonaws-rds-amazonrdsv19/tests/all`.
//!
//! The rows span what the rule generalises over: the engine family the cluster runs and the
//! `FeatureName` that family publishes for an association (`DescribeDBEngineVersions`'
//! `SupportedFeatureNames`, which is where a client reads them — the two S3 features of the
//! Aurora PostgreSQL family and a machine learning integration of Aurora MySQL), and the form
//! the trust document takes (the service named as a string, named in a list beside another
//! service, another service named instead, and an allow of the service denied beside it).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{
    block_of, blocks_of, element, elements, form_encode, opt_element, outputs_of,
};

/// The Region every request here is signed for, and so the Region the estate lives in.
const REGION: &str = "us-west-2";

/// The master user every cluster below is created with, and its password — test credentials,
/// and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The service principal a role Amazon RDS acts under for a DB cluster feature must be granted
/// `sts:AssumeRole` by, and one that is not it.
const RDS_PRINCIPAL: &str = "rds.amazonaws.com";
const OTHER_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The `Status` an association that holds reports: the terminal value of
/// `DBClusterRole$Status`, whose other terminal value is `INVALID` and whose non-terminal one is
/// `PENDING`.
const ACTIVE: &str = "ACTIVE";

/// The status a stack settles in after a create every owning service accepted.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// What Amazon RDS refuses a role it cannot take on with: the Query API's error code for a
/// parameter value it does not accept, and the ONE fixed sentence that refusal carries — no part
/// of it is taken from the request or from this emulator's state, so it is spelled out here
/// rather than read from the code under test.
const REFUSAL_CODE: &str = "InvalidParameterValue";
const REFUSAL_MESSAGE: &str = "IAM role ARN value is invalid or does not include the required \
                               permissions for: AWS_ROLE_INTEGRATION";

/// How long one poll waits before the next, and how many are made: the stack's transitions are
/// owed on CloudFormation's own clock, so this suite waits for the CONDITION rather than for a
/// span, with a budget far longer than the spans themselves so a loaded host does not decide the
/// outcome.
const POLL: Duration = Duration::from_millis(50);
/// See [`POLL`].
const POLLS: usize = 2_400;

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// CloudFormation provisioner's own calls are dispatched back into this same router, so the
/// cluster and the roles it creates are the ones Amazon RDS and IAM then answer about.
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

/// One Amazon RDS call over its awsQuery wire.
async fn rds(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "rds", "2014-10-31", action, members).await
}

/// One engine row: the family the cluster runs and a `FeatureName` that family publishes for a
/// role association.
struct Row {
    case: &'static str,
    /// The stack this row deploys, and the prefix every resource and role of it is named under.
    stack: &'static str,
    engine: &'static str,
    feature_name: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the S3 import feature of the Aurora PostgreSQL family",
        stack: "xsvc-rds-mixed-trust-pg-import",
        engine: "aurora-postgresql",
        feature_name: "s3Import",
    },
    Row {
        case: "the S3 export feature of the Aurora PostgreSQL family",
        stack: "xsvc-rds-mixed-trust-pg-export",
        engine: "aurora-postgresql",
        feature_name: "s3Export",
    },
    Row {
        case: "a machine learning integration of the Aurora MySQL family",
        stack: "xsvc-rds-mixed-trust-mysql",
        engine: "aurora-mysql",
        feature_name: "Bedrock",
    },
];

/// One trust row: a role every stack below carries, the trust document it is created with, and
/// whether Amazon RDS can take that role on.
struct Trust {
    case: &'static str,
    /// The logical id of the role in the template, and the stack output its ARN is read from.
    logical_id: &'static str,
    /// Whether a role carrying this document can be associated.
    assumable: bool,
}

/// The four roles every stack carries: the same S3 policy on each, and a trust document of its
/// own — which is the only thing the association's outcome may depend on.
const TRUSTS: &[Trust] = &[
    Trust {
        case: "a trust policy naming Amazon RDS as a string",
        logical_id: "TrustedRole",
        assumable: true,
    },
    Trust {
        case: "a trust policy naming Amazon RDS in a list beside another service",
        logical_id: "TrustedBesideAnotherRole",
        assumable: true,
    },
    Trust {
        case: "a trust policy naming another service instead",
        logical_id: "MistrustedRole",
        assumable: false,
    },
    Trust {
        case: "a trust policy whose allow of Amazon RDS is denied beside it",
        logical_id: "DeniedRole",
        assumable: false,
    },
];

/// The trust document of the role `trust` names, as the template declares it inline.
fn trust_document(trust: &Trust) -> Value {
    let allow = |service: Value| {
        json!({
            "Effect": "Allow",
            "Principal": { "Service": service },
            "Action": "sts:AssumeRole",
        })
    };
    let statements = match trust.logical_id {
        "TrustedRole" => vec![allow(json!(RDS_PRINCIPAL))],
        "TrustedBesideAnotherRole" => {
            vec![allow(json!([OTHER_PRINCIPAL, RDS_PRINCIPAL]))]
        }
        "MistrustedRole" => vec![allow(json!(OTHER_PRINCIPAL))],
        _ => vec![
            allow(json!(RDS_PRINCIPAL)),
            json!({
                "Effect": "Deny",
                "Principal": { "Service": RDS_PRINCIPAL },
                "Action": "sts:AssumeRole",
            }),
        ],
    };
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// The template one row deploys: the bucket the feature would read, the four roles — each with
/// the SAME S3 policy, so the trust document is the only difference between them — and the
/// cluster the association is made on.
fn template(row: &Row) -> String {
    let stack = row.stack;
    let bucket = format!("{stack}-bucket");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["s3:GetObject", "s3:ListBucket"],
            "Resource": [
                format!("arn:aws:s3:::{bucket}"),
                format!("arn:aws:s3:::{bucket}/*"),
            ],
        }],
    });
    let mut resources = json!({
        "Bucket": {
            "Type": "AWS::S3::Bucket",
            "Properties": { "BucketName": bucket },
        },
        "DbCluster": {
            "Type": "AWS::RDS::DBCluster",
            "Properties": {
                "DBClusterIdentifier": format!("{stack}-cluster"),
                "Engine": row.engine,
                "MasterUsername": MASTER_USER,
                "MasterUserPassword": MASTER_PASSWORD,
            },
        },
    });
    let mut outputs = json!({
        "ClusterIdentifier": { "Value": { "Ref": "DbCluster" } },
    });
    for trust in TRUSTS {
        let logical_id = trust.logical_id;
        resources[logical_id] = json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "RoleName": format!("{stack}-{}", logical_id.to_ascii_lowercase()),
                "AssumeRolePolicyDocument": trust_document(trust),
                "Policies": [{
                    "PolicyName": "read-the-bucket",
                    "PolicyDocument": policy,
                }],
            },
        });
        outputs[logical_id] = json!({ "Value": { "Fn::GetAtt": [logical_id, "Arn"] } });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "an Aurora DB cluster and the mixed-trust roles one of its features \
                        would act under",
        "Resources": resources,
        "Outputs": outputs,
    })
    .to_string()
}

/// A `DescribeStacks` answer for `stack`.
async fn describe_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks {stack:?}, got {status} with body \
         {body}"
    );
    body
}

/// Poll `stack` until the operation in flight has finished, and answer the status it finished in
/// with the answer that reported it: the transitions on the way are owed on the stack's own
/// clock, so this waits for the condition rather than for a span.
async fn wait_past_the_operation(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> (String, String) {
    for _ in 0..POLLS {
        let body = describe_stack(router, case, stack).await;
        let status = opt_element(&body, "StackStatus").unwrap_or_default();
        if !status.ends_with("_IN_PROGRESS") {
            return (status, body);
        }
        tokio::time::sleep(POLL).await;
    }
    let body = describe_stack(router, case, stack).await;
    let status = opt_element(&body, "StackStatus").unwrap_or_default();
    (status, body)
}

/// Every `ResourceStatusReason` the stack has recorded, so a deployment that did not settle where
/// the rule says reports WHY rather than only that it did not.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&events, "ResourceStatusReason")
}

/// Deploy one row's stack and answer its outputs: the cluster identifier and one role ARN per
/// trust row, each read as the client reads it.
async fn deploy(
    router: &MultiServiceRouter,
    row: &Row,
) -> std::collections::BTreeMap<String, String> {
    let case = row.case;
    let body = template(row);
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", row.stack),
            ("TemplateBody", &body),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
    );
    let (settled, described) = wait_past_the_operation(router, case, row.stack).await;
    assert_eq!(
        settled,
        CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {settled:?} with \
         resource failures {:?}",
        failure_reasons(router, row.stack).await
    );
    let outputs = outputs_of(&described);
    for trust in TRUSTS {
        assert!(
            outputs.contains_key(trust.logical_id),
            "case {case:?}/{:?}: expected the stack to publish the role's ARN, got outputs {:?}",
            trust.case,
            outputs
        );
    }
    outputs
}

/// The `(RoleArn, FeatureName, Status)` triples Amazon RDS reports on the DB cluster
/// `identifier`, read one per `AssociatedRoles` entry.
async fn associated_roles(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> Vec<(String, String, String)> {
    let (status, body) = rds(
        router,
        "DescribeDBClusters",
        &[("DBClusterIdentifier", identifier)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected Amazon RDS to resolve the cluster {identifier:?}, got {status} \
         with body {body}"
    );
    let Some(listed) = block_of(&body, "AssociatedRoles") else {
        return Vec::new();
    };
    blocks_of(listed, "DBClusterRole")
        .into_iter()
        .map(|entry| {
            (
                element(entry, "RoleArn"),
                element(entry, "FeatureName"),
                element(entry, "Status"),
            )
        })
        .collect()
}

/// One `AddRoleToDBCluster` call, as `(HTTP status, Error.Code, Error.Message)` — the three
/// things a client branches on. A success carries no error elements, so both read as empty.
async fn add_role(
    router: &MultiServiceRouter,
    identifier: &str,
    role_arn: &str,
    feature_name: &str,
) -> (u16, String, String) {
    let (status, body) = rds(
        router,
        "AddRoleToDBCluster",
        &[
            ("DBClusterIdentifier", identifier),
            ("RoleArn", role_arn),
            ("FeatureName", feature_name),
        ],
    )
    .await;
    (
        status,
        opt_element(&body, "Code").unwrap_or_default(),
        opt_element(&body, "Message").unwrap_or_default(),
    )
}

/// Every engine row: the cluster takes the roles whose trust policy grants Amazon RDS the assume
/// and refuses the ones that do not — each decision on the FIRST call — and reports exactly the
/// accepted associations, each with the terminal `ACTIVE` status.
#[tokio::test]
async fn an_aurora_cluster_takes_only_the_roles_whose_trust_policy_names_amazon_rds() {
    for row in ROWS {
        let router = build_router();
        let outputs = deploy(&router, row).await;
        let identifier = outputs
            .get("ClusterIdentifier")
            .expect("the stack publishes the cluster identifier")
            .clone();

        let mut accepted: Vec<(String, String, String)> = Vec::new();
        for trust in TRUSTS {
            let case = format!("{} with {}", row.case, trust.case);
            let role_arn = outputs
                .get(trust.logical_id)
                .expect("the stack publishes the role's ARN")
                .clone();
            let (status, code, message) =
                add_role(&router, &identifier, &role_arn, row.feature_name).await;
            if trust.assumable {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the association to be made on the first call, got \
                     HTTP {status} with code {code:?} and message {message:?}"
                );
                accepted.push((
                    role_arn,
                    row.feature_name.to_string(),
                    ACTIVE.to_string(),
                ));
            } else {
                assert_eq!(
                    (status, code.as_str(), message.as_str()),
                    (400, REFUSAL_CODE, REFUSAL_MESSAGE),
                    "case {case:?}: expected HTTP 400 {REFUSAL_CODE} with the role-integration \
                     sentence, got HTTP {status} with code {code:?} and message {message:?}"
                );
            }

            // Whatever the decision was, the cluster reports exactly the associations that were
            // MADE: a refused role is recorded nowhere, and an accepted one is reported at once
            // with the terminal status, never left non-terminal.
            let reported = associated_roles(&router, &case, &identifier).await;
            assert_eq!(
                reported, accepted,
                "case {case:?}: expected the cluster's associations {accepted:?}, got {reported:?}"
            );
        }
    }
}

/// The refusal is STABLE: a role whose trust policy does not grant Amazon RDS the assume is
/// refused the same way on every attempt, however many times it is asked, and never becomes an
/// association — while the role that was accepted stays `ACTIVE` throughout.
#[tokio::test]
async fn the_refusal_of_a_mistrusted_role_holds_on_every_attempt() {
    /// How many times the mistrusted role is offered: the claim is about EVERY attempt, so one
    /// call cannot settle it.
    const ATTEMPTS: usize = 4;

    let row = &ROWS[0];
    let router = build_router();
    // A stack of its own, so this test and the one above cannot see each other's resources.
    let stacked = Row {
        case: "a mistrusted role offered over and over",
        stack: "xsvc-rds-mixed-trust-repeated",
        engine: row.engine,
        feature_name: row.feature_name,
    };
    let outputs = deploy(&router, &stacked).await;
    let identifier = outputs
        .get("ClusterIdentifier")
        .expect("the stack publishes the cluster identifier")
        .clone();
    let trusted = outputs
        .get("TrustedRole")
        .expect("the stack publishes the trusted role's ARN")
        .clone();
    let mistrusted = outputs
        .get("MistrustedRole")
        .expect("the stack publishes the mistrusted role's ARN")
        .clone();

    let (status, code, message) =
        add_role(&router, &identifier, &trusted, stacked.feature_name).await;
    assert_eq!(
        status, 200,
        "case \"the trusted role\": expected the association to be made on the first call, got \
         HTTP {status} with code {code:?} and message {message:?}"
    );
    let held = vec![(
        trusted.clone(),
        stacked.feature_name.to_string(),
        ACTIVE.to_string(),
    )];

    for attempt in 1..=ATTEMPTS {
        let case = format!("attempt {attempt} of {ATTEMPTS} with the mistrusted role");
        let (status, code, message) =
            add_role(&router, &identifier, &mistrusted, stacked.feature_name).await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, REFUSAL_CODE, REFUSAL_MESSAGE),
            "case {case:?}: expected HTTP 400 {REFUSAL_CODE} with the role-integration sentence \
             on every attempt, got HTTP {status} with code {code:?} and message {message:?}"
        );
        let reported = associated_roles(&router, &case, &identifier).await;
        assert_eq!(
            reported, held,
            "case {case:?}: expected the cluster to still report only {held:?}, got {reported:?}"
        );
    }
}
