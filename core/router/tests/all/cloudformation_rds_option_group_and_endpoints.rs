// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack that declares Amazon RDS resources leaves behind: the OPTION
//! GROUP it declared, and the ENDPOINTS it publishes for a DB instance and a DB cluster.
//!
//! The rules under test:
//!
//! * **A resource type a stack declares is created in the service that OWNS it**, so that
//!   service's own read resolves it after the stack reports `CREATE_COMPLETE`. For
//!   `AWS::RDS::OptionGroup` that service is Amazon RDS and that read is
//!   `DescribeOptionGroups`, and the options the declaration asked for are the ones the group
//!   then carries — a DB instance in the same stack naming the group is what proves the
//!   group is resolvable where a client's own instance would resolve it.
//! * **An endpoint a stack publishes for a resource is the one the owning service published
//!   for it**, read off that service rather than computed: a client takes the address and the
//!   port out of a stack output and CONNECTS to them, so a value of another shape is an
//!   address nothing answers on. `AWS::RDS::DBInstance` publishes `Endpoint.Address` and
//!   `Endpoint.Port`, and `AWS::RDS::DBCluster` those two plus `ReadEndpoint.Address`.
//! * **A property the owning service REFUSES is the resource's own failure**, in that
//!   service's words: a declared `AssociatedRoles` entry naming a role IAM holds nothing
//!   under cannot be associated, so the DB instance fails with Amazon RDS's own sentence for
//!   a role integration it cannot perform and the stack rolls back.
//!
//! Three crates have to agree for these to hold — CloudFormation deploys the estate, Amazon
//! RDS owns the resources and their endpoints, and IAM answers for the role — so the suite
//! lives here rather than in any one of them.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, blocks_of, element, elements, form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the estate lives in.
const REGION: &str = "us-west-2";

/// The master user every DB resource below is created with, and its password — test
/// credentials, and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The class and the storage a declared DB instance is offered with.
const INSTANCE_CLASS: &str = "db.t3.micro";
const ALLOCATED_STORAGE: &str = "20";

/// The engine and the major version the declared option group is for, and an option that
/// engine publishes (`DescribeOptionGroupOptions`, which is where a client reads them).
const OPTION_GROUP_ENGINE: &str = "mysql";
const OPTION_GROUP_MAJOR_VERSION: &str = "8.0";
const DECLARED_OPTION: &str = "MEMCACHED";

/// The statuses a stack settles in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The `ResourceStatus` a resource whose create failed reports.
const RESOURCE_CREATE_FAILED: &str = "CREATE_FAILED";

/// What Amazon RDS answers for a role it cannot act under: ONE fixed sentence, spelled out
/// here rather than taken from the code under test.
const ROLE_NOT_TRUSTED: &str = "IAM role ARN value is invalid or does not include the \
                                required permissions for: AWS_ROLE_INTEGRATION";

/// How long one poll waits before the next, and how many are made: every transition waited
/// for here is owed on another service's own clock, so the suite waits for the CONDITION
/// rather than for a span, with a budget far longer than the spans themselves so a loaded
/// host does not decide the outcome.
const POLL: Duration = Duration::from_millis(50);
/// See [`POLL`].
const POLLS: usize = 2_400;

/// The emulator as it is ASSEMBLED, on one engine: the CloudFormation provisioner's own calls
/// are dispatched back into this same router, so the resources it creates are the ones these
/// reads resolve.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and what each handler reads its Region from.
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

/// One Amazon RDS call over its awsQuery wire, answered with the status and the body.
async fn rds(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "rds", "2014-10-31", action, members).await
}

/// Deploy `body` as the stack `stack` and answer the status it settled in, with the stack's
/// own answer.
async fn deploy(router: &MultiServiceRouter, stack: &str, body: &str) -> (String, String) {
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", body),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateStack {stack:?}: expected HTTP 200, got {status} with body {answered}"
    );
    let mut last = stack_state(router, stack).await;
    for _ in 0..POLLS {
        if !last.0.ends_with("_IN_PROGRESS") {
            return last;
        }
        tokio::time::sleep(POLL).await;
        last = stack_state(router, stack).await;
    }
    last
}

/// The `StackStatus` the stack reports now, with the answer it was read from.
async fn stack_state(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {body}"
    );
    (opt_element(&body, "StackStatus").unwrap_or_default(), body)
}

/// The value the stack published under the output `key`, read out of the stack's `Outputs`
/// block so no other repeated `member` of the answer is mistaken for one.
fn output(answered: &str, key: &str) -> Option<String> {
    let outputs = block_of(answered, "Outputs")?;
    blocks_of(outputs, "member")
        .into_iter()
        .find(|member| opt_element(member, "OutputKey").as_deref() == Some(key))
        .and_then(|member| opt_element(member, "OutputValue"))
}

/// Every `ResourceStatusReason` the stack has recorded, so a deployment that did not settle
/// where the case says reports WHY.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&events, "ResourceStatusReason")
}

/// Rule: the option group a stack declares exists in Amazon RDS afterwards, carrying the
/// engine, the major version and the options its declaration asked for — and a DB instance of
/// the same stack resolves it by name.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cfn_option_group_is_created_in_amazon_rds() {
    const STACK: &str = "xsvc-rds-option-group";
    let router = build_router();
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "an option group and a DB instance configured from it",
        "Resources": {
            "Options": {
                "Type": "AWS::RDS::OptionGroup",
                "Properties": {
                    "EngineName": OPTION_GROUP_ENGINE,
                    "MajorEngineVersion": OPTION_GROUP_MAJOR_VERSION,
                    "OptionGroupDescription": "the stack's own option group",
                    "OptionConfigurations": [{ "OptionName": DECLARED_OPTION }],
                },
            },
            "DbInstance": {
                "Type": "AWS::RDS::DBInstance",
                "Properties": {
                    "DBInstanceIdentifier": format!("{STACK}-db"),
                    "Engine": OPTION_GROUP_ENGINE,
                    "DBInstanceClass": INSTANCE_CLASS,
                    "AllocatedStorage": ALLOCATED_STORAGE,
                    "MasterUsername": MASTER_USER,
                    "MasterUserPassword": MASTER_PASSWORD,
                    "OptionGroupName": { "Ref": "Options" },
                },
            },
        },
        "Outputs": {
            "OptionGroupName": { "Value": { "Ref": "Options" } },
        },
    })
    .to_string();

    let (settled, answered) = deploy(&router, STACK, &template).await;
    assert_eq!(
        settled,
        CREATE_COMPLETE,
        "expected the stack to settle in {CREATE_COMPLETE}, got {settled:?} with resource \
         failures {:?}",
        failure_reasons(&router, STACK).await
    );
    let name = output(&answered, "OptionGroupName")
        .unwrap_or_else(|| panic!("expected the stack to publish its option group's name"));

    // Amazon RDS's OWN read of the group: the service that holds an option group is where a
    // client resolves one, so this is what proves the stack created it there.
    let (status, body) = rds(&router, "DescribeOptionGroups", &[("OptionGroupName", &name)]).await;
    assert_eq!(
        status, 200,
        "expected Amazon RDS to hold the option group {name:?}, got {status} with {body}"
    );
    for (member, expected) in [
        ("EngineName", OPTION_GROUP_ENGINE),
        ("MajorEngineVersion", OPTION_GROUP_MAJOR_VERSION),
    ] {
        let reported = element(&body, member);
        assert_eq!(
            reported, expected,
            "expected the group's {member} to be {expected:?}, got {reported:?} in {body}"
        );
    }
    let options = elements(&body, "OptionName");
    assert!(
        options.iter().any(|option| option == DECLARED_OPTION),
        "expected the declared option {DECLARED_OPTION:?} on the group, got {options:?}"
    );
}

/// One resource kind the endpoint rule holds for: the type, the properties it is declared
/// with, the attributes it publishes and the members the owning service reports them as.
struct EndpointCase {
    case: &'static str,
    stack: &'static str,
    resource_type: &'static str,
    /// The engine the resource runs: RDS for MySQL for the instance, Aurora PostgreSQL for
    /// the cluster.
    engine: &'static str,
    /// `(stack output, the `Fn::GetAtt` attribute, the describe's own member)` triples.
    published: &'static [(&'static str, &'static str, &'static str)],
    /// The describe that answers for the resource, and the member addressing it.
    describe: &'static str,
    identifier_member: &'static str,
}

const ENDPOINT_CASES: &[EndpointCase] = &[
    EndpointCase {
        case: "a DB instance",
        stack: "xsvc-rds-endpoint-instance",
        resource_type: "AWS::RDS::DBInstance",
        engine: "mysql",
        // The instance reports its endpoint as the `Endpoint` STRUCTURE, whose `Address` and
        // `Port` the type publishes under the same two names.
        published: &[
            ("EndpointAddress", "Endpoint.Address", "Address"),
            ("EndpointPort", "Endpoint.Port", "Port"),
        ],
        describe: "DescribeDBInstances",
        identifier_member: "DBInstanceIdentifier",
    },
    EndpointCase {
        case: "a DB cluster",
        stack: "xsvc-rds-endpoint-cluster",
        resource_type: "AWS::RDS::DBCluster",
        engine: "aurora-postgresql",
        // The cluster reports both endpoints as SCALAR members of its own structure.
        published: &[
            ("EndpointAddress", "Endpoint.Address", "Endpoint"),
            ("EndpointPort", "Endpoint.Port", "Port"),
            ("ReaderAddress", "ReadEndpoint.Address", "ReaderEndpoint"),
        ],
        describe: "DescribeDBClusters",
        identifier_member: "DBClusterIdentifier",
    },
];

/// Rule: every endpoint a stack publishes for an RDS resource is the value the owning service
/// publishes for that resource, so a client that dials a stack output reaches the database.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cfn_rds_resource_publishes_the_endpoint_amazon_rds_published() {
    for endpoint_case in ENDPOINT_CASES {
        let case = endpoint_case.case;
        let router = build_router();
        let stack = endpoint_case.stack;
        let identifier = format!("{stack}-rsc");

        let mut properties = json!({
            endpoint_case.identifier_member: identifier,
            "Engine": endpoint_case.engine,
            "MasterUsername": MASTER_USER,
            "MasterUserPassword": MASTER_PASSWORD,
        });
        if endpoint_case.resource_type == "AWS::RDS::DBInstance" {
            properties["DBInstanceClass"] = Value::String(INSTANCE_CLASS.to_string());
            properties["AllocatedStorage"] = Value::String(ALLOCATED_STORAGE.to_string());
        }
        let mut outputs = serde_json::Map::new();
        for (output_key, attribute, _) in endpoint_case.published {
            outputs.insert(
                (*output_key).to_string(),
                json!({ "Value": { "Fn::GetAtt": ["Resource", attribute] } }),
            );
        }
        let template = json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Description": "a database resource whose endpoint the stack publishes",
            "Resources": {
                "Resource": {
                    "Type": endpoint_case.resource_type,
                    "Properties": properties,
                },
            },
            "Outputs": Value::Object(outputs),
        })
        .to_string();

        let (settled, answered) = deploy(&router, stack, &template).await;
        assert_eq!(
            settled,
            CREATE_COMPLETE,
            "case {case:?}: expected the stack to settle in {CREATE_COMPLETE}, got {settled:?} \
             with resource failures {:?}",
            failure_reasons(&router, stack).await
        );

        // What the OWNING service publishes for the resource, which is what the stack's
        // outputs have to carry.
        let (status, body) = rds(
            &router,
            endpoint_case.describe,
            &[(endpoint_case.identifier_member, &identifier)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected Amazon RDS to resolve {identifier:?}, got {status} with \
             {body}"
        );
        // A DB instance carries its two values inside its `Endpoint` structure; a DB cluster
        // carries them as members of its own.
        let reported_in = block_of(&body, "Endpoint")
            .filter(|_| endpoint_case.resource_type == "AWS::RDS::DBInstance")
            .unwrap_or(&body);
        for (output_key, attribute, member) in endpoint_case.published {
            let published = element(reported_in, member);
            assert!(
                !published.is_empty(),
                "case {case:?}: expected Amazon RDS to publish {member} for {identifier:?}, got \
                 {body}"
            );
            let in_output = output(&answered, output_key).unwrap_or_else(|| {
                panic!("case {case:?}: expected the stack to publish {output_key}")
            });
            assert_eq!(
                in_output, published,
                "case {case:?}: expected {attribute} to be the {member} Amazon RDS published \
                 ({published:?}), got {in_output:?}"
            );
        }
    }
}

/// Rule: a declared `AssociatedRoles` entry the owning service will not associate is the DB
/// instance's own failure, reported in that service's words — here a role ARN of the modelled
/// form that IAM holds nothing under, which Amazon RDS cannot act under for any feature.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cfn_db_instance_fails_on_a_role_iam_does_not_hold() {
    const STACK: &str = "xsvc-rds-roles-absent";
    const DB_INSTANCE: &str = "DbInstance";
    let router = build_router();
    let identifier = format!("{STACK}-db");
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a DB instance declaring a role that does not exist",
        "Resources": {
            DB_INSTANCE: {
                "Type": "AWS::RDS::DBInstance",
                "Properties": {
                    "DBInstanceIdentifier": identifier,
                    "Engine": "postgres",
                    "DBInstanceClass": INSTANCE_CLASS,
                    "AllocatedStorage": ALLOCATED_STORAGE,
                    "MasterUsername": MASTER_USER,
                    "MasterUserPassword": MASTER_PASSWORD,
                    "AssociatedRoles": [{
                        "RoleArn": format!("arn:aws:iam::000000000000:role/{STACK}-nowhere"),
                        "FeatureName": "s3Import",
                    }],
                },
            },
        },
    })
    .to_string();

    let (settled, _) = deploy(&router, STACK, &template).await;
    assert_eq!(
        settled,
        ROLLBACK_COMPLETE,
        "expected the stack to settle in {ROLLBACK_COMPLETE}, got {settled:?} with resource \
         failures {:?}",
        failure_reasons(&router, STACK).await
    );

    let (_, events) = cloudformation(&router, "DescribeStackEvents", &[("StackName", STACK)]).await;
    let reason = blocks_of(&events, "member")
        .into_iter()
        .find(|member| {
            element(member, "LogicalResourceId") == DB_INSTANCE
                && element(member, "ResourceStatus") == RESOURCE_CREATE_FAILED
        })
        .map(|member| element(member, "ResourceStatusReason"))
        .unwrap_or_else(|| {
            panic!("expected a {RESOURCE_CREATE_FAILED} event for {DB_INSTANCE}, got {events}")
        });
    assert!(
        reason.contains(ROLE_NOT_TRUSTED),
        "expected the DB instance's reason to carry {ROLE_NOT_TRUSTED:?}, got {reason:?}"
    );

    // And the rollback took the instance away again, so the stack publishes no database
    // carrying a half-made association.
    let (status, body) = rds(
        &router,
        "DescribeDBInstances",
        &[("DBInstanceIdentifier", &identifier)],
    )
    .await;
    assert_eq!(
        status, 404,
        "expected Amazon RDS to hold no instance {identifier:?} after the rollback, got {status} \
         with {body}"
    );
}
