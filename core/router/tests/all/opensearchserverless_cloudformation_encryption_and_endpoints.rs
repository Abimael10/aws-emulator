// SPDX-License-Identifier: CC0-1.0

//! CloudFormation deploys the OpenSearch Serverless resources a template declares THROUGH
//! that service's own API, and publishes the identifiers it minted.
//!
//! RULE: a resource provider sends every property of its declaration that corresponds to a
//! member of the owning service's create — the STRUCTURED ones as the structure that member
//! takes, re-cased into the model's own spelling — and publishes as the resource's physical
//! id (which is what `Ref` resolves to) the identifier that service ASSIGNED, never a name
//! the template declared. So:
//!
//! * an `AWS::OpenSearchServerless::Collection` that declares its own `EncryptionConfig`
//!   carries that setting to `CreateCollection`, which is what lets a stack create a
//!   collection no `encryption` security policy covers — the stack reaches
//!   `CREATE_COMPLETE`, its `{"Ref": …}` is the `CollectionId`, and `BatchGetCollection`
//!   reports that collection `ACTIVE` with the declared name, type and key;
//! * an `AWS::OpenSearchServerless::VpcEndpoint` is created in that service and publishes
//!   the `vpce-…` `id` it minted, which `BatchGetVpcEndpoint` accepts and reports `ACTIVE` —
//!   a name is a value `VpcEndpointId`'s own `^vpce-[0-9a-z]*$` rejects where an id is
//!   required.
//!
//! A declaration the owning service REFUSES is not reported as created: the failed create is
//! rolled back and no reader of that service reports the resource.
//!
//! Sources: `smithy_query(query="operation", operation="CreateCollection,CreateVpcEndpoint,
//! BatchGetCollection,BatchGetVpcEndpoint")` and `smithy_query(query="shape",
//! shape="EncryptionConfig,VpcEndpointStatus")` on the opensearchserverless model. The
//! CloudFormation resource types' own property names are not modelled, so they are the
//! upper-camel spelling of those members.
//!
//! This suite needs CloudFormation, OpenSearch Serverless and Amazon EC2 on ONE engine —
//! CloudFormation reaches the owning service through the in-process callback, and that
//! service resolves an endpoint's network against EC2 — which is why it lives beside the
//! router rather than in any one service's crate. The provisioning walk is stack-hungry, so
//! every case is driven on a sized thread.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_opensearchserverless_opensearchserverless::state::AWS_OWNED_KEY;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element, outputs_of, physical_ids_of};

/// Keep this at or above the other CloudFormation suites' stack: the same provisioning walk
/// runs here.
const DEPLOY_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// How long one poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a poll keeps looking before giving up. Generous: every transition these cases
/// wait for is owed by the clock in well under a second, and nothing here is timed.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// The stack status a deployment that went through settles in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The statuses a stack operation can come to rest in, so a poll stops at whichever one the
/// deployment reached rather than at the one it hoped for.
const TERMINAL_STACK_STATUSES: &[&str] = &[
    CREATE_COMPLETE,
    "CREATE_FAILED",
    "ROLLBACK_COMPLETE",
    "ROLLBACK_FAILED",
    "DELETE_COMPLETE",
    "DELETE_FAILED",
];

/// The status OpenSearch Serverless reports a collection and an endpoint that are up in.
const ACTIVE: &str = "ACTIVE";

/// One emulator carrying the three services these cases need over ONE shared engine, as the
/// production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_opensearchserverless_opensearchserverless::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// Run `case` on a thread sized for the provisioning walk, so a panic inside it fails the
/// test.
fn on_a_sized_thread(name: &str, case: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(DEPLOY_STACK_SIZE)
        .spawn(case)
        .expect("spawn the sized thread")
        .join()
        .expect("the case runs to completion");
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request here; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Drive one CloudFormation action over its awsQuery wire, answering the status and the XML
/// body.
fn cloudformation(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    (status, body)
}

/// Drive one OpenSearch Serverless operation over its awsJson1_0 wire, answering the status,
/// the bare error code (empty on success) and the parsed body.
fn aoss(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    operation: &str,
    body: Value,
) -> (u16, String, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("aoss.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("OpenSearchServerless.{operation}"))
        .header("authorization", authorization("aoss"))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    let code = parsed
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string();
    (status, code, parsed)
}

/// One ec2Query action that must succeed, answered with its XML body.
fn ec2(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {text}"
    );
    text
}

/// A current-thread runtime with time enabled, which is what a poll loop needs.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a current-thread runtime")
}

/// Deploy `template` as the stack `stack_name` and poll `DescribeStacks` until the stack
/// comes to rest, answering `(the status it settled in, that answer's XML)`.
///
/// The settling is owed by the clock and by the owning service's own progress, so this waits
/// for the CONDITION — a terminal status — under a generous deadline, and times nothing.
fn deployed(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    stack_name: &str,
    template: &str,
) -> (String, String) {
    let (status, body) = cloudformation(
        router,
        runtime,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", template)],
    );
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must be accepted, got {status} with {body}"
    );
    settled(router, runtime, case, stack_name)
}

/// Poll `DescribeStacks` until the stack's operation comes to rest.
fn settled(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    stack_name: &str,
) -> (String, String) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (_, described) = cloudformation(
            router,
            runtime,
            "DescribeStacks",
            &[("StackName", stack_name)],
        );
        // A stack that has already gone away answers no status at all, which is the
        // `DELETE_COMPLETE` a delete polls for.
        let status = opt_element(&described, "StackStatus").unwrap_or_default();
        if TERMINAL_STACK_STATUSES.contains(&status.as_str()) {
            return (status, described);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack {stack_name} never came to rest; last status \
             {status:?} in {described}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The `collectionDetails` entry `BatchGetCollection` reports for `id`, polled until that
/// service reports the collection `ACTIVE` — the move out of `CREATING` is owed by the clock.
fn active_collection(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    id: &str,
) -> Value {
    polled_detail(
        router,
        runtime,
        case,
        "BatchGetCollection",
        "collectionDetails",
        id,
    )
}

/// The `vpcEndpointDetails` entry `BatchGetVpcEndpoint` reports for `id`, polled until that
/// service reports the endpoint `ACTIVE`.
fn active_vpc_endpoint(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    id: &str,
) -> Value {
    polled_detail(
        router,
        runtime,
        case,
        "BatchGetVpcEndpoint",
        "vpcEndpointDetails",
        id,
    )
}

/// The one detail `operation` reports under `member` for `id`, once its `status` is `ACTIVE`.
fn polled_detail(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    operation: &str,
    member: &str,
    id: &str,
) -> Value {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, code, read) = aoss(router, runtime, operation, json!({ "ids": [id] }));
        assert_eq!(
            (status, code.as_str()),
            (200, ""),
            "case {case:?}: {operation}({id}) must be served, got {status} {code} with {read}"
        );
        let detail = read
            .get(member)
            .and_then(Value::as_array)
            .and_then(|details| details.first())
            .cloned()
            .unwrap_or(Value::Null);
        if detail.get("status").and_then(Value::as_str) == Some(ACTIVE) {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {id} never reached {ACTIVE}; last {operation} answer {read}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The text of one member of a detail document, or `""` when it carries none.
fn member_text(detail: &Value, member: &str) -> String {
    detail
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The names `ListVpcEndpoints` reports, which is what says whether an endpoint exists at
/// all.
fn listed_endpoint_names(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
) -> Vec<String> {
    let (status, code, listing) = aoss(router, runtime, "ListVpcEndpoints", json!({}));
    assert_eq!(
        (status, code.as_str()),
        (200, ""),
        "ListVpcEndpoints must be served, got {status} {code} with {listing}"
    );
    listing
        .get("vpcEndpointSummaries")
        .and_then(Value::as_array)
        .map(|summaries| {
            summaries
                .iter()
                .map(|summary| member_text(summary, "name"))
                .collect()
        })
        .unwrap_or_default()
}

/// True when `id` satisfies `CollectionId`'s modelled `^[a-z0-9]{3,40}$`, which is what a
/// `Ref` to a collection has to return for `BatchGetCollection` to accept it.
fn is_collection_id(id: &str) -> bool {
    (3..=40).contains(&id.len())
        && id
            .chars()
            .all(|held| held.is_ascii_lowercase() || held.is_ascii_digit())
}

/// True when `id` satisfies `VpcEndpointId`'s modelled `^vpce-[0-9a-z]*$`, which is what a
/// `Ref` to an endpoint has to return for `BatchGetVpcEndpoint` to accept it.
fn is_vpc_endpoint_id(id: &str) -> bool {
    id.strip_prefix("vpce-").is_some_and(|rest| {
        rest.chars()
            .all(|held| held.is_ascii_lowercase() || held.is_ascii_digit())
    })
}

/// A template declaring ONE collection with the properties `properties` states, and its
/// `Ref` as an output.
fn collection_template(properties: Value) -> String {
    json!({
        "Resources": { "Collection": {
            "Type": "AWS::OpenSearchServerless::Collection",
            "Properties": properties,
        } },
        "Outputs": { "CollectionId": { "Value": { "Ref": "Collection" } } },
    })
    .to_string()
}

/// A template declaring ONE managed interface endpoint with the properties `properties`
/// states, and its `Ref` as an output.
fn vpc_endpoint_template(properties: Value) -> String {
    json!({
        "Resources": { "Endpoint": {
            "Type": "AWS::OpenSearchServerless::VpcEndpoint",
            "Properties": properties,
        } },
        "Outputs": { "EndpointId": { "Value": { "Ref": "Endpoint" } } },
    })
    .to_string()
}

/// A customer managed key ARN in the form `EncryptionConfig$kmsKeyArn`'s own pattern admits
/// (`^arn:(aws|…):kms:[a-z0-9-]+:[0-9]{12}:key/<uuid>$`). It is the value the DECLARATION
/// carries: `CreateCollection` records the key its request names for the collection to report
/// back, so what this case asserts is that the declared setting reached that call.
const DECLARED_KMS_KEY_ARN: &str =
    "arn:aws:kms:us-east-1:000000000000:key/1234abcd-12ab-34cd-56ef-1234567890ab";

#[test]
fn a_stack_carries_a_collection_s_declared_encryption_to_the_owning_service() {
    on_a_sized_thread("aoss-cfn-collection-encryption", || {
        let router = emulator();
        let runtime = runtime();

        // (case, the declared properties, the key the collection must report) — the rows
        // span the members `EncryptionConfig` carries and the collection types the property
        // travels beside.
        let rows: Vec<(&str, Value, &str)> = vec![
            (
                "the AWS-owned key on a SEARCH collection",
                json!({
                    "Name": "cfn-encrypted-search",
                    "Type": "SEARCH",
                    "EncryptionConfig": { "AWSOwnedKey": true },
                }),
                AWS_OWNED_KEY,
            ),
            (
                "a customer managed key on a VECTORSEARCH collection",
                json!({
                    "Name": "cfn-encrypted-vectors",
                    "Type": "VECTORSEARCH",
                    "EncryptionConfig": { "KmsKeyArn": DECLARED_KMS_KEY_ARN },
                }),
                DECLARED_KMS_KEY_ARN,
            ),
            (
                "the AWS-owned key beside a description on a TIMESERIES collection",
                json!({
                    "Name": "cfn-encrypted-series",
                    "Type": "TIMESERIES",
                    "Description": "deployed by a stack",
                    "EncryptionConfig": { "AWSOwnedKey": true },
                }),
                AWS_OWNED_KEY,
            ),
        ];

        for (position, (case, properties, key)) in rows.into_iter().enumerate() {
            let declared_name = member_text(&properties, "Name");
            let declared_type = member_text(&properties, "Type");
            let stack_name = format!("aoss-encrypted-{position}");
            let (status, described) = deployed(
                &router,
                &runtime,
                case,
                &stack_name,
                &collection_template(properties.clone()),
            );
            assert_eq!(
                status, CREATE_COMPLETE,
                "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {status} \
                 in {described}"
            );

            // `Ref` is the identifier the owning service minted, not the declared name.
            let outputs: BTreeMap<String, String> = outputs_of(&described);
            let reference = outputs.get("CollectionId").cloned().unwrap_or_default();
            assert!(
                is_collection_id(&reference),
                "case {case:?}: expected Ref to be a CollectionId, got {reference:?} in \
                 {described}"
            );
            assert_ne!(
                reference, declared_name,
                "case {case:?}: expected Ref to be the minted id rather than the declared name"
            );

            // And the collection is real: the owning service reports it, with the
            // declaration's own name, type and encryption setting.
            let detail = active_collection(&router, &runtime, case, &reference);
            for (member, expected) in [
                ("name", declared_name.as_str()),
                ("type", declared_type.as_str()),
                ("kmsKeyArn", key),
            ] {
                let reported = member_text(&detail, member);
                assert_eq!(
                    reported, expected,
                    "case {case:?}: expected the collection's {member} to be {expected:?}, got \
                     {reported:?} in {detail}"
                );
            }
        }
    });
}

#[test]
fn a_stack_publishes_the_vpc_endpoint_id_the_owning_service_minted() {
    on_a_sized_thread("aoss-cfn-vpc-endpoint", || {
        let router = emulator();
        let runtime = runtime();

        // The network the endpoints are placed in is EC2's, so every id below is one EC2
        // itself minted.
        let vpc = element(
            &ec2(
                &router,
                &runtime,
                "CreateVpc",
                &[("CidrBlock", "10.70.0.0/16")],
            ),
            "vpcId",
        );
        let subnets: Vec<String> = ["10.70.1.0/24", "10.70.2.0/24"]
            .iter()
            .map(|cidr| {
                element(
                    &ec2(
                        &router,
                        &runtime,
                        "CreateSubnet",
                        &[("VpcId", vpc.as_str()), ("CidrBlock", cidr)],
                    ),
                    "subnetId",
                )
            })
            .collect();
        let security_group = element(
            &ec2(
                &router,
                &runtime,
                "CreateSecurityGroup",
                &[
                    ("GroupName", "aoss-cfn-endpoint"),
                    ("GroupDescription", "the stack endpoint"),
                    ("VpcId", vpc.as_str()),
                ],
            ),
            "groupId",
        );

        // (case, the declared properties, the subnets the endpoint must report) — the rows
        // span how many subnets the placement names and whether security groups travel with
        // it.
        let rows: Vec<(&str, Value, Vec<String>)> = vec![
            (
                "one subnet and no security group",
                json!({
                    "Name": "cfn-endpoint-one",
                    "VpcId": &vpc,
                    "SubnetIds": [&subnets[0]],
                }),
                vec![subnets[0].clone()],
            ),
            (
                "two subnets and a security group",
                json!({
                    "Name": "cfn-endpoint-two",
                    "VpcId": &vpc,
                    "SubnetIds": [&subnets[0], &subnets[1]],
                    "SecurityGroupIds": [&security_group],
                }),
                subnets.clone(),
            ),
        ];

        for (position, (case, properties, placed_in)) in rows.into_iter().enumerate() {
            let declared_name = member_text(&properties, "Name");
            let stack_name = format!("aoss-endpoint-{position}");
            let (status, described) = deployed(
                &router,
                &runtime,
                case,
                &stack_name,
                &vpc_endpoint_template(properties.clone()),
            );
            assert_eq!(
                status, CREATE_COMPLETE,
                "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {status} \
                 in {described}"
            );

            let outputs: BTreeMap<String, String> = outputs_of(&described);
            let reference = outputs.get("EndpointId").cloned().unwrap_or_default();
            assert!(
                is_vpc_endpoint_id(&reference),
                "case {case:?}: expected Ref to be a VpcEndpointId, got {reference:?} in \
                 {described}"
            );
            assert_ne!(
                reference, declared_name,
                "case {case:?}: expected Ref to be the minted id rather than the declared name"
            );

            // The stack's own physical resource id is that same value, so anything reading
            // the resource rather than the output gets the id too.
            let (_, resources) = cloudformation(
                &router,
                &runtime,
                "DescribeStackResources",
                &[("StackName", &stack_name)],
            );
            let physical = physical_ids_of(&resources)
                .get("Endpoint")
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                physical, reference,
                "case {case:?}: expected the physical resource id to be the minted id, got \
                 {physical:?} in {resources}"
            );

            // And the endpoint is real: the owning service accepts that id and reports the
            // endpoint it created from the declaration.
            let detail = active_vpc_endpoint(&router, &runtime, case, &reference);
            assert_eq!(
                member_text(&detail, "name"),
                declared_name,
                "case {case:?}: expected the endpoint's name to be the declared one, got {detail}"
            );
            assert_eq!(
                member_text(&detail, "vpcId"),
                vpc,
                "case {case:?}: expected the endpoint to report EC2's own VPC, got {detail}"
            );
            assert_eq!(
                detail.get("subnetIds").cloned().unwrap_or(Value::Null),
                json!(placed_in),
                "case {case:?}: expected the endpoint to report the declared subnets, got {detail}"
            );
        }

        // A declaration the owning service REFUSES leaves no endpoint behind: the placement
        // below names a VPC EC2 holds no record of, so the create is refused and the stack
        // rolls back rather than reporting an endpoint nothing holds.
        let case = "a VPC EC2 does not hold";
        let (status, described) = deployed(
            &router,
            &runtime,
            case,
            "aoss-endpoint-refused",
            &vpc_endpoint_template(json!({
                "Name": "cfn-endpoint-phantom",
                "VpcId": "vpc-0f1e2d3c4b5a69780",
                "SubnetIds": [&subnets[0]],
            })),
        );
        assert_ne!(
            status, CREATE_COMPLETE,
            "case {case:?}: a refused create must not be reported as complete, got {described}"
        );
        let names = listed_endpoint_names(&router, &runtime);
        assert!(
            !names.contains(&"cfn-endpoint-phantom".to_string()),
            "case {case:?}: a refused create must leave no endpoint listed, got {names:?}"
        );
    });
}
