// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an Amazon VPC Lattice RESOURCE-ACCESS estate, across
//! the three services that own the halves of it: CloudFormation reads the template and makes
//! the calls, Amazon EC2 holds the VPC, subnet and security group, and VPC Lattice holds the
//! service network, the resource gateway, the resource configuration and the two
//! associations.
//!
//! This suite needs more than one service by construction: what the stack PUBLISHES for each
//! VPC Lattice resource is an identifier that service minted, so no single-service test can
//! assert that the handle CloudFormation reports is the one VPC Lattice's own reads answer
//! for.
//!
//! The RULES asserted over the tables below:
//!
//! 1. A template whose `Resources` declare resource types CloudFormation PUBLISHES is
//!    accepted — `CreateStack` answers a `StackId` — and only a type it publishes none of is
//!    refused `Template format error: Unrecognized resource types: [<types>]`. The rows span
//!    the class the rule generalises over: each published VPC Lattice type on its own, all of
//!    them together, and a misspelling of each, which must still be refused.
//! 2. What the stack publishes for such a resource is BACKED by the owning service: the
//!    `Ref` of each is the ARN VPC Lattice minted, `Fn::GetAtt … Id` is the id inside it, and
//!    that service's own read and listing report exactly those values beside the
//!    configuration the template declared — with the template's tags and the three
//!    `aws:cloudformation:*` tags on the resource.
//! 3. A declaration the owning service REFUSES fails that resource and rolls the stack back:
//!    the failed resource carries no `PhysicalResourceId`, its `ResourceStatusReason` is the
//!    resource-handler envelope over that service's own sentence under
//!    `HandlerErrorCode: InvalidRequest`, and the resources created before it are deleted —
//!    while the same template with a value that service accepts deploys.
//! 4. Deleting the stack deletes what it created: every VPC Lattice read of the estate
//!    answers that resource's own not-found afterwards.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{
    blocks_of, element, elements, form_encode, opt_element, outputs_of, pairs_of,
};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The VPC block, the subnet block and the address the declared resource configuration names
/// inside it.
const VPC_CIDR: &str = "10.121.0.0/16";
const SUBNET_CIDR: &str = "10.121.0.0/24";
const RESOURCE_ADDRESS: &str = "10.121.0.50";

/// How long a poll waits between reads of a stack, and how many reads it makes: generous
/// enough for every clock-owed span of the deploy to come due on a loaded host, with the loop
/// stopping as soon as the stack reports a terminal status.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLL_ATTEMPTS: usize = 600;

/// The refusal a template declaring a type CloudFormation publishes none of earns, which no
/// template of a published type may ever earn.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// What one `restJson1` call answered.
struct Answered {
    status: u16,
    body: Value,
}

/// CloudFormation, Amazon EC2 and VPC Lattice over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the stack's resources are created where a client
/// creates them, and each owning service serves its own reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_vpclattice_mercurycontrolplane::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request and what names the Region its ARNs carry. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDROUTERTEST/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
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

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// One Amazon EC2 call over its ec2Query wire.
async fn ec2(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "ec2", "2016-11-15", action, members).await
}

/// One VPC Lattice call over its `restJson1` wire.
async fn lattice(router: &MultiServiceRouter, method: &str, uri: &str) -> Answered {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("vpc-lattice.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("vpc-lattice"))
        .body(Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()))
    };
    Answered { status, body }
}

/// An ARN used as an `@httpLabel`, encoded the way an AWS SDK encodes a non-greedy label.
fn as_label(value: &str) -> String {
    value.replace('/', "%2F")
}

/// The first Availability Zone Amazon EC2 reports for the Region, which the declared subnet
/// is placed in rather than a zone name written here.
async fn first_zone(router: &MultiServiceRouter) -> String {
    let (status, body) = ec2(router, "DescribeAvailabilityZones", &[]).await;
    assert_eq!(
        status, 200,
        "expected 200 from DescribeAvailabilityZones, got {status} / {body}"
    );
    elements(&body, "zoneName")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("expected at least one Availability Zone, got {body}"))
}

/// What the stack's own events say about every resource that failed, for a case that did not
/// reach the status it was asserted to.
async fn failures(router: &MultiServiceRouter, stack: &str) -> String {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    blocks_of(&events, "member")
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

/// Read a stack until it reports a status that is no longer in progress, answering that
/// read's body.
///
/// A stack whose DELETE has completed is no longer a stack a read by NAME resolves —
/// CloudFormation answers `Stack with id <name> does not exist` for one — so that refusal
/// ends the poll as the terminal answer it is.
async fn poll_to_terminal(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let mut described = String::new();
    for _ in 0..POLL_ATTEMPTS {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        if status == 400 && body.contains("does not exist") {
            return body;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from DescribeStacks, got {status} / {body}"
        );
        described = body;
        if !opt_element(&described, "StackStatus")
            .unwrap_or_default()
            .ends_with("_IN_PROGRESS")
        {
            return described;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    described
}

/// Submit a template and read the stack to a terminal status, answering that read's body.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &Value,
) -> String {
    let template = template.to_string();
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from CreateStack, got {status} / {body}"
    );
    poll_to_terminal(router, case, stack).await
}

/// The template of the whole estate: the EC2 network the gateway is opened in, the service
/// network the configuration is shared into, and the two associations — with `port_ranges`
/// the one value the rows below vary.
fn estate_template(zone: &str, port_ranges: &[&str]) -> Value {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Vpc": {
                "Type": "AWS::EC2::VPC",
                "Properties": {
                    "CidrBlock": VPC_CIDR,
                    "Tags": [{"Key": "estate", "Value": "lattice"}],
                },
            },
            "Subnet": {
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": {"Ref": "Vpc"},
                    "CidrBlock": SUBNET_CIDR,
                    "AvailabilityZone": zone,
                },
            },
            "Sg": {
                "Type": "AWS::EC2::SecurityGroup",
                "Properties": {
                    "GroupDescription": "resource gateway",
                    "VpcId": {"Ref": "Vpc"},
                },
            },
            "ServiceNetwork": {
                "Type": "AWS::VpcLattice::ServiceNetwork",
                "Properties": {
                    "Name": "estate-net",
                    "Tags": [{"Key": "estate", "Value": "lattice"}],
                },
            },
            "ResourceGateway": {
                "Type": "AWS::VpcLattice::ResourceGateway",
                "Properties": {
                    "Name": "estate-gateway",
                    "VpcIdentifier": {"Ref": "Vpc"},
                    "SubnetIds": [{"Ref": "Subnet"}],
                    "SecurityGroupIds": [{"Fn::GetAtt": ["Sg", "GroupId"]}],
                    "Tags": [{"Key": "estate", "Value": "lattice"}],
                },
            },
            "ResourceConfiguration": {
                "Type": "AWS::VpcLattice::ResourceConfiguration",
                "Properties": {
                    "Name": "estate-config",
                    "ResourceConfigurationType": "SINGLE",
                    "ResourceGatewayId": {"Fn::GetAtt": ["ResourceGateway", "Id"]},
                    "ResourceConfigurationDefinition": {"IpResource": RESOURCE_ADDRESS},
                    "PortRanges": port_ranges,
                    "Tags": [{"Key": "estate", "Value": "lattice"}],
                },
            },
            "ResourceAssociation": {
                "Type": "AWS::VpcLattice::ServiceNetworkResourceAssociation",
                "Properties": {
                    "ResourceConfigurationId": {"Fn::GetAtt": ["ResourceConfiguration", "Id"]},
                    "ServiceNetworkId": {"Fn::GetAtt": ["ServiceNetwork", "Id"]},
                    "Tags": [{"Key": "estate", "Value": "lattice"}],
                },
            },
            "VpcAssociation": {
                "Type": "AWS::VpcLattice::ServiceNetworkVpcAssociation",
                "Properties": {
                    "ServiceNetworkIdentifier": {"Fn::GetAtt": ["ServiceNetwork", "Id"]},
                    "VpcIdentifier": {"Ref": "Vpc"},
                    "Tags": [{"Key": "estate", "Value": "lattice"}],
                },
            },
        },
        "Outputs": {
            "GatewayRef": {"Value": {"Ref": "ResourceGateway"}},
            "GatewayArn": {"Value": {"Fn::GetAtt": ["ResourceGateway", "Arn"]}},
            "GatewayId": {"Value": {"Fn::GetAtt": ["ResourceGateway", "Id"]}},
            "ConfigurationRef": {"Value": {"Ref": "ResourceConfiguration"}},
            "ConfigurationArn": {"Value": {"Fn::GetAtt": ["ResourceConfiguration", "Arn"]}},
            "ConfigurationId": {"Value": {"Fn::GetAtt": ["ResourceConfiguration", "Id"]}},
            "NetworkId": {"Value": {"Fn::GetAtt": ["ServiceNetwork", "Id"]}},
            "ResourceAssociationId": {
                "Value": {"Fn::GetAtt": ["ResourceAssociation", "Id"]}
            },
            "VpcAssociationId": {"Value": {"Fn::GetAtt": ["VpcAssociation", "Id"]}},
            "VpcId": {"Value": {"Ref": "Vpc"}},
        },
    })
}

/// Rule 1: a template declaring published VPC Lattice types is accepted; one declaring a type
/// CloudFormation publishes none of is still refused, naming exactly that type.
#[tokio::test]
async fn a_template_declaring_published_vpc_lattice_types_is_accepted() {
    let router = emulator();

    // (case, the type the template declares, whether CloudFormation publishes it)
    let rows: &[(&str, &str, bool)] = &[
        (
            "a resource gateway",
            "AWS::VpcLattice::ResourceGateway",
            true,
        ),
        (
            "a resource configuration",
            "AWS::VpcLattice::ResourceConfiguration",
            true,
        ),
        (
            "a service-network/resource association",
            "AWS::VpcLattice::ServiceNetworkResourceAssociation",
            true,
        ),
        (
            "a service-network/VPC association",
            "AWS::VpcLattice::ServiceNetworkVpcAssociation",
            true,
        ),
        ("a service network", "AWS::VpcLattice::ServiceNetwork", true),
        // A type of that namespace CloudFormation publishes none of, and a misspelling of a
        // type it does: the refusal is reserved for exactly these.
        (
            "a type of the namespace that is not published",
            "AWS::VpcLattice::ResourceGatewayOfNothing",
            false,
        ),
        (
            "a misspelled resource configuration",
            "AWS::VpcLattice::ResourceConfigurations",
            false,
        ),
    ];

    for (index, (case, resource_type, published)) in rows.iter().enumerate() {
        // Only the TYPE is under test here, so the declaration carries no properties: a type
        // CloudFormation does not publish is refused before any property is read.
        let template = json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": {"Declared": {"Type": resource_type}},
        })
        .to_string();
        let stack = format!("registry-case-{index}");
        let (status, body) = cloudformation(
            &router,
            "CreateChangeSet",
            &[
                ("StackName", &stack),
                ("ChangeSetName", "registry"),
                ("ChangeSetType", "CREATE"),
                ("TemplateBody", &template),
            ],
        )
        .await;
        if *published {
            assert_eq!(
                status, 200,
                "case {case:?}: a published type must be accepted, got {status} / {body}"
            );
            assert!(
                !body.contains(UNRECOGNIZED_PREFIX),
                "case {case:?}: a published type must not be refused as unrecognized, got {body}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: an unpublished type must be refused, got {status} / {body}"
        );
        assert!(
            body.contains(&format!("{UNRECOGNIZED_PREFIX}{resource_type}]")),
            "case {case:?}: expected the refusal to name exactly the unpublished type, got {body}"
        );
    }
}

/// Rules 2 and 4: the whole estate deploys, everything the stack publishes is backed by VPC
/// Lattice's own reads and listings, and deleting the stack removes it again.
#[tokio::test]
async fn the_estate_a_stack_declares_is_backed_by_vpc_lattices_own_reads() {
    let router = emulator();
    let zone = first_zone(&router).await;
    let stack = "lattice-estate";
    let described = create_and_settle(
        &router,
        "the estate",
        stack,
        &estate_template(&zone, &["443"]),
    )
    .await;
    let reported = opt_element(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        reported, "CREATE_COMPLETE",
        "expected the stack to settle at CREATE_COMPLETE, got {reported:?}; its failed \
         resources: {}",
        failures(&router, stack).await
    );

    let outputs = outputs_of(&described);
    let published = |key: &str| -> String {
        outputs
            .get(key)
            .cloned()
            .unwrap_or_else(|| panic!("expected the stack to publish {key}, got {outputs:?}"))
    };

    // Rule 2: `Ref` is the ARN that service minted, which is also what `Fn::GetAtt … Arn`
    // reads, for each type whose `Ref` returns one.
    for (case, reference, arn) in [
        (
            "resource gateway",
            published("GatewayRef"),
            published("GatewayArn"),
        ),
        (
            "resource configuration",
            published("ConfigurationRef"),
            published("ConfigurationArn"),
        ),
    ] {
        assert_eq!(
            reference, arn,
            "case {case:?}: Ref must return the resource's own Arn"
        );
        assert!(
            arn.starts_with(&format!("arn:aws:vpc-lattice:{REGION}:")),
            "case {case:?}: expected a VPC Lattice ARN of the signed Region, got {arn:?}"
        );
    }

    let gateway_id = published("GatewayId");
    let configuration_id = published("ConfigurationId");
    let network_id = published("NetworkId");
    let resource_association_id = published("ResourceAssociationId");
    let vpc_association_id = published("VpcAssociationId");
    let vpc_id = published("VpcId");

    // The gateway VPC Lattice holds reads back the network the template declared.
    let gateway = lattice(&router, "GET", &format!("/resourcegateways/{gateway_id}")).await;
    assert_eq!(
        gateway.status, 200,
        "the gateway the stack published must be one VPC Lattice holds, got {} / {}",
        gateway.status, gateway.body
    );
    assert_eq!(
        gateway.body["vpcId"].as_str(),
        Some(vpc_id.as_str()),
        "the gateway must sit in the VPC the stack created, got {}",
        gateway.body
    );
    assert_eq!(
        gateway.body["arn"].as_str(),
        Some(published("GatewayArn").as_str()),
        "the gateway's ARN must be the one the stack published, got {}",
        gateway.body
    );
    assert_eq!(
        gateway.body["subnetIds"].as_array().map(Vec::len),
        Some(1),
        "the gateway must carry the one subnet the template declared, got {}",
        gateway.body
    );
    assert_eq!(
        gateway.body["securityGroupIds"].as_array().map(Vec::len),
        Some(1),
        "the gateway must carry the one security group the template declared, got {}",
        gateway.body
    );

    // The configuration reads back the type, the port ranges and the definition declared.
    let configuration = lattice(
        &router,
        "GET",
        &format!("/resourceconfigurations/{configuration_id}"),
    )
    .await;
    assert_eq!(
        configuration.status, 200,
        "the configuration the stack published must be one VPC Lattice holds, got {} / {}",
        configuration.status, configuration.body
    );
    assert_eq!(
        configuration.body["type"].as_str(),
        Some("SINGLE"),
        "expected the declared type, got {}",
        configuration.body
    );
    assert_eq!(
        configuration.body["portRanges"],
        json!(["443"]),
        "expected the declared port ranges, got {}",
        configuration.body
    );
    assert_eq!(
        configuration.body["resourceConfigurationDefinition"],
        json!({"ipResource": {"ipAddress": RESOURCE_ADDRESS}}),
        "expected the declared definition as the API's own union member, got {}",
        configuration.body
    );
    assert_eq!(
        configuration.body["resourceGatewayId"].as_str(),
        Some(gateway_id.as_str()),
        "expected the configuration to name the gateway the stack created, got {}",
        configuration.body
    );

    // Both associations read back ACTIVE and name their parents.
    let resource_association = lattice(
        &router,
        "GET",
        &format!("/servicenetworkresourceassociations/{resource_association_id}"),
    )
    .await;
    assert_eq!(
        resource_association.status, 200,
        "the resource association the stack published must be one VPC Lattice holds, got {} / {}",
        resource_association.status, resource_association.body
    );
    for (member, expected) in [
        ("resourceConfigurationId", configuration_id.as_str()),
        ("serviceNetworkId", network_id.as_str()),
    ] {
        assert_eq!(
            resource_association.body[member].as_str(),
            Some(expected),
            "case {member:?}: the association must name the parent the stack created, got {}",
            resource_association.body
        );
    }
    let vpc_association = lattice(
        &router,
        "GET",
        &format!("/servicenetworkvpcassociations/{vpc_association_id}"),
    )
    .await;
    assert_eq!(
        vpc_association.status, 200,
        "the VPC association the stack published must be one VPC Lattice holds, got {} / {}",
        vpc_association.status, vpc_association.body
    );
    assert_eq!(
        vpc_association.body["vpcId"].as_str(),
        Some(vpc_id.as_str()),
        "the VPC association must name the VPC the stack created, got {}",
        vpc_association.body
    );

    // Every id the stack published is one that service's own LISTING reports.
    for (case, uri, expected) in [
        ("resource gateways", "/resourcegateways".to_string(), gateway_id.clone()),
        (
            "resource configurations",
            "/resourceconfigurations".to_string(),
            configuration_id.clone(),
        ),
        (
            "resource associations",
            format!("/servicenetworkresourceassociations?serviceNetworkIdentifier={network_id}"),
            resource_association_id.clone(),
        ),
        (
            "VPC associations",
            format!("/servicenetworkvpcassociations?serviceNetworkIdentifier={network_id}"),
            vpc_association_id.clone(),
        ),
    ] {
        let listed = lattice(&router, "GET", &uri).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected 200, got {} / {}",
            listed.status, listed.body
        );
        let ids: Vec<&str> = listed.body["items"]
            .as_array()
            .map(|items| items.iter().filter_map(|item| item["id"].as_str()).collect())
            .unwrap_or_default();
        assert!(
            ids.contains(&expected.as_str()),
            "case {case:?}: the listing must report the id the stack published ({expected}), \
             got {}",
            listed.body
        );
    }

    // Rule 2's tag half: the template's own tag and the three CloudFormation records are what
    // the owning service's tag read reports for the resource.
    let stack_id = element(&described, "StackId");
    for (case, arn) in [
        ("resource gateway", published("GatewayArn")),
        ("resource configuration", published("ConfigurationArn")),
    ] {
        let tags = lattice(&router, "GET", &format!("/tags/{}", as_label(&arn))).await;
        assert_eq!(
            tags.status, 200,
            "case {case:?}: expected 200 from ListTagsForResource, got {} / {}",
            tags.status, tags.body
        );
        let held = tags.body["tags"].clone();
        assert_eq!(
            held["estate"].as_str(),
            Some("lattice"),
            "case {case:?}: the declaration's own tag must reach the resource, got {held}"
        );
        assert_eq!(
            held["aws:cloudformation:stack-name"].as_str(),
            Some(stack),
            "case {case:?}: CloudFormation must record the stack's name on the resource, got \
             {held}"
        );
        assert_eq!(
            held["aws:cloudformation:stack-id"].as_str(),
            Some(stack_id.as_str()),
            "case {case:?}: CloudFormation must record the stack's id on the resource, got {held}"
        );
        assert!(
            held["aws:cloudformation:logical-id"].is_string(),
            "case {case:?}: CloudFormation must record the resource's logical id, got {held}"
        );
    }

    // Rule 4: the stack's delete deletes what it created, so every read of the estate answers
    // that resource's own not-found afterwards.
    let (status, body) = cloudformation(&router, "DeleteStack", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "expected 200 from DeleteStack, got {status} / {body}"
    );
    let described = poll_to_terminal(&router, "the delete", stack).await;
    let deleted = opt_element(&described, "StackStatus").unwrap_or_default();
    assert!(
        deleted.is_empty() || deleted == "DELETE_COMPLETE",
        "expected the stack to settle at DELETE_COMPLETE, got {deleted:?}"
    );

    for (case, uri) in [
        (
            "the resource configuration",
            format!("/resourceconfigurations/{configuration_id}"),
        ),
        (
            "the resource gateway",
            format!("/resourcegateways/{gateway_id}"),
        ),
        ("the service network", format!("/servicenetworks/{network_id}")),
        (
            "the resource association",
            format!("/servicenetworkresourceassociations/{resource_association_id}"),
        ),
        (
            "the VPC association",
            format!("/servicenetworkvpcassociations/{vpc_association_id}"),
        ),
    ] {
        let read = lattice(&router, "GET", &uri).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a resource the stack deleted must be gone, got {} / {}",
            read.status, read.body
        );
    }
}

/// Rule 3: a declaration the owning service refuses fails that resource and rolls the stack
/// back, and the same template with a value it accepts deploys.
#[tokio::test]
async fn a_refused_declaration_rolls_the_stack_back_without_a_physical_id() {
    let router = emulator();
    let zone = first_zone(&router).await;
    let stack = "lattice-rollback";

    // The one value that differs from the deploying template: a port range whose first port
    // is the greater, which VPC Lattice refuses.
    let described = create_and_settle(
        &router,
        "an invalid port range",
        stack,
        &estate_template(&zone, &["100-50"]),
    )
    .await;
    let reported = opt_element(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        reported, "ROLLBACK_COMPLETE",
        "expected the refused declaration to roll the stack back, got {reported:?}; its failed \
         resources: {}",
        failures(&router, stack).await
    );

    // The resource the owning service refused is reported as the FAILED one, with the
    // resource-handler envelope over that service's own sentence as its reason and no
    // physical id: nothing was created for it to name.
    let (_, events) = cloudformation(&router, "DescribeStackEvents", &[("StackName", stack)]).await;
    let failed = blocks_of(&events, "member")
        .into_iter()
        .find(|member| {
            opt_element(member, "LogicalResourceId").as_deref() == Some("ResourceConfiguration")
                && opt_element(member, "ResourceStatus").as_deref() == Some("CREATE_FAILED")
        })
        .unwrap_or_else(|| {
            panic!("expected a CREATE_FAILED event for the refused configuration, got {events}")
        });
    assert!(
        opt_element(failed, "PhysicalResourceId").is_none_or(|id| id.is_empty()),
        "a resource the owning service refused carries no PhysicalResourceId, got {failed}"
    );
    let reason = opt_element(failed, "ResourceStatusReason")
        .unwrap_or_else(|| panic!("expected the failed event to carry a reason, got {failed}"));
    for fragment in [
        "100-50 is not a valid port range",
        "(Service: VpcLattice, Status Code: 400",
        "HandlerErrorCode: InvalidRequest",
    ] {
        assert!(
            reason.contains(fragment),
            "expected the handler reason to carry {fragment:?}, got {reason:?}"
        );
    }

    // The resources created before the refusal are deleted, so the stack's own record
    // reports each of them removed rather than standing.
    let (status, resources) =
        cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "expected 200 from DescribeStackResources, got {status} / {resources}"
    );
    let statuses: BTreeMap<String, String> =
        pairs_of(&resources, "LogicalResourceId", "ResourceStatus");
    for logical in ["Vpc", "ResourceGateway"] {
        assert_eq!(
            statuses.get(logical).map(String::as_str),
            Some("DELETE_COMPLETE"),
            "case {logical:?}: the rollback must delete what it had created, got {statuses:?}"
        );
    }

    // The rollback deleted what the stack had created before the refusal, so Amazon EC2 no
    // longer holds the VPC and VPC Lattice no longer holds the gateway.
    let gateway_arns: Vec<String> = {
        let listed = lattice(&router, "GET", "/resourcegateways").await;
        listed.body["items"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["arn"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    assert!(
        gateway_arns.is_empty(),
        "the rollback must delete the gateway it had created, got {gateway_arns:?}"
    );

    // The identical template with a port range that service accepts deploys.
    let described = create_and_settle(
        &router,
        "the same estate with an accepted port range",
        "lattice-accepted",
        &estate_template(&zone, &["443"]),
    )
    .await;
    let reported = opt_element(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        reported, "CREATE_COMPLETE",
        "expected the accepted port range to deploy, got {reported:?}; its failed resources: {}",
        failures(&router, "lattice-accepted").await
    );
}
