// SPDX-License-Identifier: CC0-1.0

//! The CODE and the SENTENCE this service answers a request that left out a modelled
//! `@required` input member with, driven over the wire the service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML).
//!
//! EC2 declares no error SHAPES at all (`smithy_query(query="operation", …)` reports
//! `errors: []` for every operation), so the ec2Query `<Code>` plus the `<Message>` is the
//! whole contract a client branches on. The rule under test: a request that omits a required
//! input member is refused HTTP 400 with code `MissingParameter`, in the sentence the
//! validator of that operation's SURFACE words —
//!
//! 1. the surfaces of the original EC2 and VPC objects state `The request must contain the
//!    parameter <parameter>`, naming the parameter as the ec2Query wire spells it;
//! 2. the surfaces added to EC2 later state `The request must include the <Member> parameter.
//!    Add the required parameter and retry the request.`, naming the member as the model
//!    declares it;
//! 3. a required FILTER LIST — the member an operation selects what it reports with — states
//!    that member's own non-empty rule, `Filters cannot be null or empty`, for a list that is
//!    absent and for one that arrives empty alike (ec2Query carries a list as indexed
//!    `Filter.N.*` fields, so only the absent half is reachable over the wire);
//! 4. the members EC2 refuses by naming the WIRE PARAMETER GROUP it reads the value under and
//!    nothing else — the whole message is that name, with no sentence around it.
//!
//! The rows span both surfaces, every member KIND the model declares a required member with
//! (an integer, a string id, a list of ids, an enum), SEVERAL required members of one
//! operation rather than only the one some recording left out, and — for each operation — a
//! request that carries every required member, which must NOT be refused for a missing
//! parameter. Both sentences are spelled here independently of the service, so the suite pins
//! the WIRE text.

use service_com_amazonaws_ec2_amazonec2 as ec2;

use super::ec2_query::{call, emulator, error_code, error_message, filters, indexed};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// A `(name, value)` form field.
fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// Rule 1's sentence: EC2's core parameter validator names the parameter the ec2Query wire
/// spells.
fn must_contain(parameter: &str) -> String {
    format!("The request must contain the parameter {parameter}")
}

/// Rule 2's sentence: the later surfaces' own input validation names the modelled member.
fn must_include(member: &str) -> String {
    format!(
        "The request must include the {member} parameter. Add the required parameter and \
         retry the request."
    )
}

/// Rule 3's sentence: a required filter list is neither null nor empty.
const FILTERS_CANNOT_BE_NULL_OR_EMPTY: &str = "Filters cannot be null or empty";

/// The form-field prefix ec2Query flattens `ModifyReservedInstancesRequest$TargetConfigurations`
/// into: the member's `@xmlName`
/// (`smithy_query(query="operation",operation="ModifyReservedInstances")`).
const TARGET_CONFIGURATIONS: &str = "ReservedInstancesConfigurationSetItemType";

/// A well-formed Reserved Instances id — the hyphenated UUID form — the store holds no
/// purchase under.
const RESERVED_INSTANCES_ID: &str = "1a2b3c4d-1234-5678-9abc-1234567890ab";

/// One row: the case's name, the operation, the form fields it sends (every required member
/// except the one under test), and the sentence the refusal has to carry.
struct Row {
    case: &'static str,
    action: &'static str,
    params: Vec<(String, String)>,
    message: String,
}

/// A well-formed network interface id this store holds no interface for. The boundary refuses
/// an absent member before any handler resolves an id, so no row's id has to exist.
const INTERFACE_ID: &str = "eni-0123456789abcdef0";

/// A well-formed instance id this store holds no instance for.
const INSTANCE_ID: &str = "i-0123456789abcdef0";

/// A well-formed VPC id this store holds no VPC for.
const VPC_ID: &str = "vpc-0123456789abcdef0";

/// A well-formed IPAM scope id this store holds no scope for.
const IPAM_SCOPE_ID: &str = "ipam-scope-0123456789abcdef0";

/// A well-formed transit gateway id this store holds no gateway for.
const TRANSIT_GATEWAY_ID: &str = "tgw-0123456789abcdef0";

/// A well-formed transit gateway route table id this store holds no table for.
const ROUTE_TABLE_ID: &str = "tgw-rtb-0123456789abcdef0";

/// A well-formed subnet id this store holds no subnet for.
const SUBNET_ID: &str = "subnet-0123456789abcdef0";

/// Every row of the absent-parameter table.
fn rows() -> Vec<Row> {
    vec![
        // --- Rule 1: the core parameter validator's sentence, parameter spelled as the wire
        // spells it. Three rows of ONE operation, so the rule covers every required member of
        // an operation on that surface and not only the member a recording left out.
        Row {
            case: "an integer the attach needs",
            action: "AttachNetworkInterface",
            params: vec![
                p("NetworkInterfaceId", INTERFACE_ID),
                p("InstanceId", INSTANCE_ID),
            ],
            message: must_contain("deviceIndex"),
        },
        Row {
            case: "the interface the attach acts on",
            action: "AttachNetworkInterface",
            params: vec![p("InstanceId", INSTANCE_ID), p("DeviceIndex", "1")],
            message: must_contain("networkInterfaceId"),
        },
        Row {
            case: "the instance the attach gives it to",
            action: "AttachNetworkInterface",
            params: vec![
                p("NetworkInterfaceId", INTERFACE_ID),
                p("DeviceIndex", "1"),
            ],
            message: must_contain("instanceId"),
        },
        Row {
            case: "the attachment the detach takes down",
            action: "DetachNetworkInterface",
            params: Vec::new(),
            message: must_contain("attachmentId"),
        },
        Row {
            case: "the VPC a subnet is created in",
            action: "CreateSubnet",
            params: vec![p("CidrBlock", "10.0.1.0/24")],
            message: must_contain("vpcId"),
        },
        Row {
            case: "the subnet a delete names",
            action: "DeleteSubnet",
            params: Vec::new(),
            message: must_contain("subnetId"),
        },
        Row {
            // `DescribeSecurityGroupReferencesRequest$GroupId` targets the LIST shape
            // `GroupIds`, and EC2 names that list rather than the singular member the model
            // keeps for the wire's indexed entries.
            case: "the security groups a reference report is asked about",
            action: "DescribeSecurityGroupReferences",
            params: Vec::new(),
            message: must_contain("groupIds"),
        },
        // --- Rule 2: the later surfaces' own sentence, member spelled as the model declares
        // it. Rows span an enum, a string id and a list of ids across two surfaces.
        Row {
            case: "the address family an IPAM pool holds",
            action: "CreateIpamPool",
            params: vec![p("IpamScopeId", IPAM_SCOPE_ID)],
            message: must_include("AddressFamily"),
        },
        Row {
            case: "the scope an IPAM pool belongs to",
            action: "CreateIpamPool",
            params: vec![p("AddressFamily", "ipv4")],
            message: must_include("IpamScopeId"),
        },
        Row {
            case: "the subnets a transit gateway attachment spans",
            action: "CreateTransitGatewayVpcAttachment",
            params: vec![
                p("TransitGatewayId", TRANSIT_GATEWAY_ID),
                p("VpcId", VPC_ID),
            ],
            message: must_include("SubnetIds"),
        },
        Row {
            case: "the transit gateway an attachment is made to",
            action: "CreateTransitGatewayVpcAttachment",
            params: {
                let mut params = vec![p("VpcId", VPC_ID)];
                params.extend(indexed("SubnetIds", &[SUBNET_ID]));
                params
            },
            message: must_include("TransitGatewayId"),
        },
        // --- Rule 3: a required filter list.
        Row {
            case: "the filters a route search selects with",
            action: "SearchTransitGatewayRoutes",
            params: vec![p("TransitGatewayRouteTableId", ROUTE_TABLE_ID)],
            message: FILTERS_CANNOT_BE_NULL_OR_EMPTY.to_string(),
        },
        // --- Rule 4: the parameter GROUP alone, with no sentence around it. The Reserved
        // Instances modify reads its target configurations under the
        // `TargetReservedInstancesConfigurationSet` group, which is what the refusal states;
        // the other required member of the SAME operation has no recorded group, so it keeps
        // its surface's sentence — which is what makes rule 4 keyed to the member and not to
        // the operation.
        Row {
            case: "the target configurations a Reserved Instances modify moves to",
            action: "ModifyReservedInstances",
            params: indexed("ReservedInstancesId", &[RESERVED_INSTANCES_ID]),
            message: "TargetReservedInstancesConfigurationSet".to_string(),
        },
        Row {
            case: "the purchases a Reserved Instances modify acts on",
            action: "ModifyReservedInstances",
            params: vec![
                p(&format!("{TARGET_CONFIGURATIONS}.1.InstanceCount"), "1"),
                p(
                    &format!("{TARGET_CONFIGURATIONS}.1.AvailabilityZone"),
                    "us-east-1a",
                ),
            ],
            message: must_include("ReservedInstancesIds"),
        },
    ]
}

/// One complete request: the case's name, the operation, and the form fields it sends.
type CompleteRequest = (&'static str, &'static str, Vec<(String, String)>);

/// One request per row that carries EVERY required member, so the refusals above are about the
/// member each row left out rather than about anything else in the request.
fn complete_requests() -> Vec<CompleteRequest> {
    vec![
        (
            "a complete attach",
            "AttachNetworkInterface",
            vec![
                p("NetworkInterfaceId", INTERFACE_ID),
                p("InstanceId", INSTANCE_ID),
                p("DeviceIndex", "1"),
            ],
        ),
        (
            "a complete detach",
            "DetachNetworkInterface",
            vec![p("AttachmentId", "eni-attach-0123456789abcdef0")],
        ),
        (
            "a complete subnet create",
            "CreateSubnet",
            vec![p("VpcId", VPC_ID), p("CidrBlock", "10.0.1.0/24")],
        ),
        (
            "a complete subnet delete",
            "DeleteSubnet",
            vec![p("SubnetId", SUBNET_ID)],
        ),
        (
            "a complete reference report",
            "DescribeSecurityGroupReferences",
            indexed("GroupId", &["sg-0123456789abcdef0"]),
        ),
        (
            "a complete IPAM pool create",
            "CreateIpamPool",
            vec![p("IpamScopeId", IPAM_SCOPE_ID), p("AddressFamily", "ipv4")],
        ),
        ("a complete attachment create", "CreateTransitGatewayVpcAttachment", {
            let mut params = vec![
                p("TransitGatewayId", TRANSIT_GATEWAY_ID),
                p("VpcId", VPC_ID),
            ];
            params.extend(indexed("SubnetIds", &[SUBNET_ID]));
            params
        }),
        ("a complete route search", "SearchTransitGatewayRoutes", {
            let mut params = vec![p("TransitGatewayRouteTableId", ROUTE_TABLE_ID)];
            params.extend(filters(&[("type", &["static"])]));
            params
        }),
        (
            "a complete Reserved Instances modify",
            "ModifyReservedInstances",
            {
                let mut params = indexed("ReservedInstancesId", &[RESERVED_INSTANCES_ID]);
                params.push(p(&format!("{TARGET_CONFIGURATIONS}.1.InstanceCount"), "1"));
                params.push(p(
                    &format!("{TARGET_CONFIGURATIONS}.1.AvailabilityZone"),
                    "us-east-1a",
                ));
                params
            },
        ),
    ]
}

/// A request that leaves out a required member is refused `MissingParameter` in its own
/// surface's sentence, and a request that carries every required member is not refused for a
/// missing parameter at all.
#[tokio::test]
async fn a_required_member_the_request_left_out_is_refused_in_its_surfaces_own_sentence() {
    let (router, _engine) = emulator();

    for Row {
        case,
        action,
        params,
        message: expected,
    } in rows()
    {
        let (status, body) = call(&router, REGION, action, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from {action}, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code,
            ec2::state::MISSING_PARAMETER,
            "case {case:?}: expected {} from {action}, got {code:?} in {body}",
            ec2::state::MISSING_PARAMETER
        );
        let message = error_message(&body);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?} from {action}, got {message:?}"
        );
    }

    // The same operations, each carrying every required member: whatever else is wrong with
    // them — every id above names a resource this store does not hold — none is a parameter
    // the request left out.
    for (case, action, params) in complete_requests() {
        let (_status, body) = call(&router, REGION, action, &params).await;
        let code = error_code(&body);
        assert_ne!(
            code,
            ec2::state::MISSING_PARAMETER,
            "case {case:?}: a request carrying every required member of {action} must not be \
             refused for a missing parameter, got {body}"
        );
    }
}
