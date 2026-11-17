// SPDX-License-Identifier: CC0-1.0

//! The internet gateway a CloudFormation stack puts on a VPC it created is DISCOVERABLE in
//! Amazon EC2 from the VPC alone.
//!
//! The rule under test: **once an internet gateway is attached to a VPC — by the
//! `AWS::EC2::VPCGatewayAttachment` a stack declares, or by an `AttachInternetGateway` call a
//! client makes — the attachment is a record of that gateway, so `DescribeInternetGateways`
//! filtered on `attachment.vpc-id` answers exactly the gateway of that VPC, reported with an
//! `Attachments` entry naming the VPC in state `available`; it answers nothing for a VPC no
//! gateway attaches, and nothing again once the attachment is taken away.** That filter is
//! the only way to reach a VPC's gateway when its id is not already known, so every
//! downstream check a client makes of a deployed network — that a public subnet routes
//! `0.0.0.0/0` to the gateway, that an isolated one does not — starts from it.
//!
//! A sibling suite (`default_vpc_internet_path_runs_ecs_tasks`) pins this filter over the
//! Region's ambient default network, and `cloudformation_ec2_cdk_vpc_estate` pins the route
//! tables of a stack-shaped VPC; what is pinned HERE is the attachment a STACK made, read
//! back the way a client that holds only the stack's `VpcId` output has to read it.
//!
//! The rows differ along what the rule generalises over: how many VPC-and-gateway pairs the
//! account holds attached at once (so the filter has to select the one of the VPC asked for
//! rather than every gateway), how many of the stack's VPCs have no gateway at all, and
//! which path made the attachment — the stack's own attachment resource, or an
//! `AttachInternetGateway` call against the ids the stack published, which also takes the
//! attachment away again.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{elements, form_encode, members_of, opt_element, outputs_of};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// How long a poll of a stack's status waits between reads, and how many times it reads: a
/// deadline generous enough for every clock-owed span of a deploy to come due on a loaded
/// host, with the loop stopping as soon as the stack reports a terminal status.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const POLL_ATTEMPTS: usize = 120;

/// The state an internet gateway's attachment is reported in: "The current state of the
/// attachment between the gateway and the VPC (available)"
/// (`DescribeInternetGatewaysRequest$Filters`, `attachment.state`).
const ATTACHMENT_AVAILABLE: &str = "available";

/// A state no internet-gateway attachment is ever in, from the same doc: `available` is the
/// only one it publishes, so this filter value selects nothing however many gateways are
/// attached.
const ATTACHMENT_NOT_AVAILABLE: &str = "detached";

/// Amazon EC2's refusal for an internet-gateway id the account holds no gateway for, which is
/// what a gateway the stack's deletion took away is answered with.
const GATEWAY_GONE: &str = "InvalidInternetGatewayID.NotFound";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: CloudFormation makes the EC2 calls a stack's
/// resources are created with, and Amazon EC2 serves them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One form-encoded call on a query wire, as its HTTP status and body.
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
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// Call one Amazon EC2 operation over its ec2Query wire.
async fn ec2(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "ec2", "2016-11-15", action, members).await
}

/// What the stack's own events say about every resource that failed, for a deploy or a
/// deletion that did not reach the status it was asserted to.
async fn failures(router: &MultiServiceRouter, stack_name: &str) -> String {
    let (_, events) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack_name)]).await;
    members_of(&events)
        .into_iter()
        .filter(|member| member.contains("_FAILED"))
        .map(|member| {
            format!(
                "{:?} {:?} {:?}",
                opt_element(member, "LogicalResourceId").unwrap_or_default(),
                opt_element(member, "ResourceStatus").unwrap_or_default(),
                opt_element(member, "ResourceStatusReason").unwrap_or_default(),
            )
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// Read a stack until it reports a status no longer in progress, and answer that read's body.
async fn poll_to_terminal(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let mut described = String::new();
    for _ in 0..POLL_ATTEMPTS {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        described = body;
        if !opt_element(&described, "StackStatus")
            .unwrap_or_default()
            .ends_with("_IN_PROGRESS")
        {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    described
}

/// Deploy a stack and assert it settled at `CREATE_COMPLETE`, quoting what failed when it did
/// not. Answers the `DescribeStacks` body that reported it, whose `Outputs` carry the ids EC2
/// assigned.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &Value,
) -> String {
    let template = template.to_string();
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = poll_to_terminal(router, case, stack_name).await;
    let reported = opt_element(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        reported, "CREATE_COMPLETE",
        "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {reported:?}; its \
         failed resources: {}",
        failures(router, stack_name).await
    );
    described
}

/// Delete a stack and read it until it is no longer reported in progress: the answer is the
/// status it ended on, which is empty once the stack is no longer described at all — the
/// terminal answer for a deletion.
async fn tear_down(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let (status, body) = cloudformation(router, "DeleteStack", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
    );
    let mut reported = String::new();
    for _ in 0..POLL_ATTEMPTS {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        // A deleted stack is no longer described, which is terminal for a deletion.
        if status != 200 {
            return String::new();
        }
        reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    reported
}

/// The internet gateways `DescribeInternetGateways` reports for `filters`, as the answer's
/// body: each entry of `filters` is one `Filter.<n>` of the request, carrying one value.
///
/// "If you specify multiple filters, the filters are joined with an AND" (`Filter`), which is
/// what lets a row ask for one VPC's gateway AND a state of the attachment at once.
async fn gateways_matching(
    router: &MultiServiceRouter,
    case: &str,
    filters: &[(&str, &str)],
) -> String {
    let mut written: Vec<(String, String)> = Vec::new();
    for (index, (name, value)) in filters.iter().enumerate() {
        let position = index + 1;
        written.push((format!("Filter.{position}.Name"), (*name).to_string()));
        written.push((format!("Filter.{position}.Value.1"), (*value).to_string()));
    }
    let members: Vec<(&str, &str)> = written
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let (status, body) = ec2(router, "DescribeInternetGateways", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeInternetGateways{filters:?}, got {status} \
         with body {body}"
    );
    body
}

/// Assert that the gateway of `vpc_id` is `gateway_id` and nothing else, and that the
/// attachment it is reported with names that VPC in state `available`.
///
/// Read through the filter a client that holds only the VPC id has to use, then read BACK
/// through the gateway's own id, so the attachment is one record both readers report rather
/// than one the filter computed for itself.
async fn assert_gateway_of_vpc(
    router: &MultiServiceRouter,
    case: &str,
    vpc_id: &str,
    gateway_id: &str,
) {
    let selected = gateways_matching(router, case, &[("attachment.vpc-id", vpc_id)]).await;
    assert_eq!(
        elements(&selected, "internetGatewayId"),
        vec![gateway_id.to_string()],
        "case {case:?}: expected attachment.vpc-id {vpc_id} to select exactly the gateway \
         {gateway_id}, got {selected}"
    );
    assert_eq!(
        elements(&selected, "vpcId"),
        vec![vpc_id.to_string()],
        "case {case:?}: expected the gateway's attachment to name the VPC {vpc_id}, got \
         {selected}"
    );
    assert_eq!(
        elements(&selected, "state"),
        vec![ATTACHMENT_AVAILABLE.to_string()],
        "case {case:?}: expected the attachment of {gateway_id} reported \
         {ATTACHMENT_AVAILABLE}, got {selected}"
    );

    // The state is part of the same record, so the AND of both filters selects the same one
    // gateway, and the state no internet-gateway attachment is in selects none.
    let both = gateways_matching(
        router,
        case,
        &[
            ("attachment.vpc-id", vpc_id),
            ("attachment.state", ATTACHMENT_AVAILABLE),
        ],
    )
    .await;
    assert_eq!(
        elements(&both, "internetGatewayId"),
        vec![gateway_id.to_string()],
        "case {case:?}: expected attachment.vpc-id {vpc_id} AND attachment.state \
         {ATTACHMENT_AVAILABLE} to select {gateway_id}, got {both}"
    );
    let neither = gateways_matching(
        router,
        case,
        &[
            ("attachment.vpc-id", vpc_id),
            ("attachment.state", ATTACHMENT_NOT_AVAILABLE),
        ],
    )
    .await;
    assert!(
        elements(&neither, "internetGatewayId").is_empty(),
        "case {case:?}: expected attachment.state {ATTACHMENT_NOT_AVAILABLE} to select no \
         internet gateway of {vpc_id}, got {neither}"
    );

    // The same attachment read back through the gateway's own id: one record, two readers.
    let (status, by_id) = ec2(
        router,
        "DescribeInternetGateways",
        &[("InternetGatewayId.1", gateway_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected Amazon EC2 to hold the gateway {gateway_id}, got HTTP {status} \
         with body {by_id}"
    );
    assert_eq!(
        elements(&by_id, "vpcId"),
        vec![vpc_id.to_string()],
        "case {case:?}: expected the gateway {gateway_id} read by id to report its attachment \
         to {vpc_id}, got {by_id}"
    );
}

/// Assert that no internet gateway at all is reported for `vpc_id`.
async fn assert_no_gateway_of_vpc(router: &MultiServiceRouter, case: &str, vpc_id: &str) {
    let selected = gateways_matching(router, case, &[("attachment.vpc-id", vpc_id)]).await;
    assert!(
        elements(&selected, "internetGatewayId").is_empty(),
        "case {case:?}: expected no internet gateway attached to {vpc_id}, got {selected}"
    );
}

/// One row of the table: how many VPC-and-gateway pairs the stack declares with an
/// `AWS::EC2::VPCGatewayAttachment` between them, how many further VPCs it declares with no
/// gateway at all, and whether it also declares a pair the CLIENT attaches itself through
/// `AttachInternetGateway` once the stack is complete.
struct Row {
    case: &'static str,
    /// The stack name, which is also what makes each row's resources its own.
    stack: &'static str,
    attached_pairs: usize,
    gatewayless_vpcs: usize,
    client_attached_pair: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "one VPC the stack attaches a gateway to",
        stack: "igw-attachment-one",
        attached_pairs: 1,
        gatewayless_vpcs: 0,
        client_attached_pair: false,
    },
    Row {
        case: "one attached VPC beside two the stack gives no gateway",
        stack: "igw-attachment-mixed",
        attached_pairs: 1,
        gatewayless_vpcs: 2,
        client_attached_pair: false,
    },
    Row {
        case: "three attached VPCs, each with its own gateway",
        stack: "igw-attachment-three",
        attached_pairs: 3,
        gatewayless_vpcs: 0,
        client_attached_pair: false,
    },
    Row {
        case: "one VPC the stack attaches, one the client attaches and detaches itself",
        stack: "igw-attachment-client",
        attached_pairs: 1,
        gatewayless_vpcs: 1,
        client_attached_pair: true,
    },
];

/// The logical ids one row's template uses, so the template and the assertions name the same
/// resources.
fn vpc_logical_id(index: usize) -> String {
    format!("Vpc{index}")
}
fn gateway_logical_id(index: usize) -> String {
    format!("Igw{index}")
}
fn gatewayless_logical_id(index: usize) -> String {
    format!("Plain{index}")
}

/// A VPC's own CIDR block, distinct per VPC of a row so each declaration is its own.
fn vpc_cidr(index: usize) -> String {
    format!("10.{}.0.0/16", 100 + index)
}

/// Declare one `AWS::EC2::VPC` in a row's template, with the output its id leaves the stack
/// through: `index` is its position among the row's VPCs, which is what gives it a block of
/// its own.
fn declare_vpc(
    resources: &mut Map<String, Value>,
    outputs: &mut Map<String, Value>,
    logical_id: String,
    index: usize,
) {
    resources.insert(
        logical_id.clone(),
        json!({
            "Type": "AWS::EC2::VPC",
            "Properties": { "CidrBlock": vpc_cidr(index) },
        }),
    );
    outputs.insert(
        format!("{logical_id}Id"),
        json!({ "Value": { "Ref": logical_id } }),
    );
}

/// The template one row deploys: each attached pair as the VPC, the internet gateway and the
/// `AWS::EC2::VPCGatewayAttachment` that puts the one on the other, each gatewayless VPC on
/// its own, and — for the row whose attachment the client makes — a further VPC and gateway
/// with NO attachment resource between them. Every id leaves the stack through an output, the
/// way a client learns a deployed VPC's id.
fn template(row: &Row) -> Value {
    let mut resources = Map::new();
    let mut outputs = Map::new();
    let mut declared = 0usize;

    for pair in 0..row.attached_pairs {
        let vpc = vpc_logical_id(pair);
        let gateway = gateway_logical_id(pair);
        declare_vpc(&mut resources, &mut outputs, vpc.clone(), declared);
        declared += 1;
        resources.insert(
            gateway.clone(),
            json!({ "Type": "AWS::EC2::InternetGateway" }),
        );
        outputs.insert(
            format!("{gateway}Id"),
            json!({ "Value": { "Ref": gateway } }),
        );
        resources.insert(
            format!("{gateway}Attachment"),
            json!({
                "Type": "AWS::EC2::VPCGatewayAttachment",
                "Properties": {
                    "VpcId": { "Ref": vpc },
                    "InternetGatewayId": { "Ref": gateway },
                },
            }),
        );
    }
    for plain in 0..row.gatewayless_vpcs {
        let vpc = gatewayless_logical_id(plain);
        declare_vpc(&mut resources, &mut outputs, vpc, declared);
        declared += 1;
    }
    if row.client_attached_pair {
        declare_vpc(
            &mut resources,
            &mut outputs,
            "ClientVpc".to_string(),
            declared,
        );
        resources.insert(
            "ClientIgw".to_string(),
            json!({ "Type": "AWS::EC2::InternetGateway" }),
        );
        outputs.insert(
            "ClientIgwId".to_string(),
            json!({ "Value": { "Ref": "ClientIgw" } }),
        );
    }

    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
}

/// A stack-created VPC's internet gateway is the one `DescribeInternetGateways` answers for
/// that VPC's `attachment.vpc-id`, with the attachment naming the VPC in state `available`; a
/// VPC the stack gave no gateway has none, a gateway the client attaches itself is found the
/// same way, a detach takes it out of that answer again, and the stack's deletion takes the
/// gateways away altogether.
#[tokio::test]
async fn a_stack_created_vpcs_internet_gateway_is_found_by_its_attachment() {
    for row in ROWS {
        let case = row.case;
        let router = build_router();
        let described = deploy(&router, case, row.stack, &template(row)).await;
        let outputs = outputs_of(&described);
        let published = |key: &str| -> String {
            outputs.get(key).cloned().unwrap_or_else(|| {
                panic!("case {case:?}: expected the stack to publish the output {key}: {outputs:?}")
            })
        };

        // Every pair the stack attached: the gateway is reachable from the VPC id alone.
        let mut vpcs: Vec<String> = Vec::new();
        let mut gateways: Vec<String> = Vec::new();
        for pair in 0..row.attached_pairs {
            let vpc = published(&format!("{}Id", vpc_logical_id(pair)));
            let gateway = published(&format!("{}Id", gateway_logical_id(pair)));
            assert_gateway_of_vpc(&router, case, &vpc, &gateway).await;
            vpcs.push(vpc);
            gateways.push(gateway);
        }

        // A VPC the stack declared no gateway for has none: the filter reports an absence
        // rather than every gateway of the account.
        for plain in 0..row.gatewayless_vpcs {
            let vpc = published(&format!("{}Id", gatewayless_logical_id(plain)));
            assert_no_gateway_of_vpc(&router, case, &vpc).await;
            vpcs.push(vpc);
        }

        // The other path to the same record: the client attaches the ids the stack published,
        // which only a gateway and a VPC Amazon EC2 really holds could accept, and the detach
        // takes the attachment back out of the filter's answer while the gateway remains.
        if row.client_attached_pair {
            let vpc = published("ClientVpcId");
            let gateway = published("ClientIgwId");
            assert_no_gateway_of_vpc(&router, case, &vpc).await;
            let (status, body) = ec2(
                &router,
                "AttachInternetGateway",
                &[("InternetGatewayId", &gateway), ("VpcId", &vpc)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from AttachInternetGateway({gateway}, {vpc}), \
                 got {status} with body {body}"
            );
            assert_gateway_of_vpc(&router, case, &vpc, &gateway).await;

            let (status, body) = ec2(
                &router,
                "DetachInternetGateway",
                &[("InternetGatewayId", &gateway), ("VpcId", &vpc)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from DetachInternetGateway({gateway}, {vpc}), \
                 got {status} with body {body}"
            );
            assert_no_gateway_of_vpc(&router, case, &vpc).await;
            // The gateway itself is still there, attached to nothing.
            let (status, by_id) = ec2(
                &router,
                "DescribeInternetGateways",
                &[("InternetGatewayId.1", &gateway)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the detached gateway {gateway} to still be held, got \
                 HTTP {status} with body {by_id}"
            );
            assert!(
                elements(&by_id, "vpcId").is_empty(),
                "case {case:?}: expected the detached gateway {gateway} to report no \
                 attachment, got {by_id}"
            );
            vpcs.push(vpc);
            gateways.push(gateway);
        }

        // The other half of the rule: what the stack created goes with it, so no VPC of the
        // row has a gateway afterwards and each gateway id resolves to nothing.
        let reported = tear_down(&router, case, row.stack).await;
        assert!(
            reported.is_empty() || reported == "DELETE_COMPLETE",
            "case {case:?}: expected the stack's deletion to complete, got {reported:?}; its \
             failed resources: {}",
            failures(&router, row.stack).await
        );
        for vpc in &vpcs {
            assert_no_gateway_of_vpc(&router, case, vpc).await;
        }
        for gateway in &gateways {
            let (status, answered) = ec2(
                &router,
                "DescribeInternetGateways",
                &[("InternetGatewayId.1", gateway)],
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected the deleted gateway {gateway} to be refused, got HTTP \
                 {status} with body {answered}"
            );
            let code = opt_element(&answered, "Code").unwrap_or_default();
            assert_eq!(
                code, GATEWAY_GONE,
                "case {case:?}: expected the deleted {gateway} to be refused with \
                 {GATEWAY_GONE}, got {code:?} from {answered}"
            );
        }
    }
}
