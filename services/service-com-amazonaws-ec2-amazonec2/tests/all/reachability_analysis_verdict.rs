// SPDX-License-Identifier: CC0-1.0

//! What a Reachability Analyzer analysis REPORTS about a path, driven over the wire the
//! service actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose
//! body carries `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire
//! protocol").
//!
//! The rule under test: **an analysis of a path between two resources of one VPC ends
//! `succeeded` and reports whether the destination is reachable, derived from the security
//! groups, the network ACLs and the routes the account holds.** "Indicates whether the
//! destination is reachable from the source"
//! (`NetworkInsightsAnalysis$NetworkPathFound`) is a function of that state: a packet the
//! destination's inbound rules do not admit is not reachable, one a network ACL on the path
//! denies is not either, and the same path becomes reachable the moment a rule admits it.
//!
//! The rows span what the rule generalises over: which protocol and port the path analyses,
//! how the admitting rule names its source (another security group, or an address range), and
//! which component on the path is the one that stops the packet (a security group, a network
//! ACL). A path this emulator traces no components for — one whose ends are not two resources
//! of one VPC — still ends `failed` carrying the reason, which is the last test here: nothing
//! reports a verdict about components nothing walked.

use ec2::state::{
    ANALYSIS_STATUS_FAILED, ANALYSIS_STATUS_RUNNING, ANALYSIS_STATUS_SUCCEEDED,
    PATH_ANALYSIS_UNSUPPORTED_STATUS_MESSAGE, TRANSITION_DELAY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, indexed, unescaped};
use super::wire::{child_text, document, param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The image every launch names; which image an instance came from has no bearing on the
/// network its packets cross.
const AMI: &str = "ami-0123456789abcdef0";

/// The block the VPC the paths run in is created with.
const VPC_CIDR: &str = "10.70.0.0/16";

/// The two subnets of that VPC: the source's and the destination's.
const SOURCE_SUBNET_CIDR: &str = "10.70.1.0/24";

/// The destination's subnet (see [`SOURCE_SUBNET_CIDR`]).
const DESTINATION_SUBNET_CIDR: &str = "10.70.2.0/24";

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// The network two instances are launched into: one VPC, two subnets, and a security group per
/// end — the destination's starting with no inbound rule at all, which is what a new group has.
struct Network {
    vpc_id: String,
    source_instance: String,
    destination_instance: String,
    source_group: String,
    destination_group: String,
    destination_subnet: String,
}

/// Build that network over the wire. `suffix` keeps each case's group names apart.
async fn network(case: &str, router: &MultiServiceRouter, suffix: &str) -> Network {
    let vpc = ok(case, router, "CreateVpc", &[param("CidrBlock", VPC_CIDR)]).await;
    let vpc_id = member(case, &vpc, "vpcId");
    let mut subnets = Vec::new();
    for cidr_block in [SOURCE_SUBNET_CIDR, DESTINATION_SUBNET_CIDR] {
        let subnet = ok(
            case,
            router,
            "CreateSubnet",
            &[param("VpcId", &vpc_id), param("CidrBlock", cidr_block)],
        )
        .await;
        subnets.push(member(case, &subnet, "subnetId"));
    }
    let mut groups = Vec::new();
    for end in ["source", "destination"] {
        let group = ok(
            case,
            router,
            "CreateSecurityGroup",
            &[
                param("VpcId", &vpc_id),
                param("GroupName", &format!("{end}-{suffix}")),
                param("GroupDescription", &format!("the {end} of the path")),
            ],
        )
        .await;
        groups.push(member(case, &group, "groupId"));
    }
    let mut instances = Vec::new();
    for (subnet_id, group_id) in subnets.iter().zip(groups.iter()) {
        let body = ok(
            case,
            router,
            "RunInstances",
            &[
                param("ImageId", AMI),
                param("MinCount", "1"),
                param("MaxCount", "1"),
                param("SubnetId", subnet_id),
                param("SecurityGroupId.1", group_id),
            ],
        )
        .await;
        instances.push(member(case, &body, "instanceId"));
    }
    Network {
        vpc_id,
        source_instance: instances[0].clone(),
        destination_instance: instances[1].clone(),
        source_group: groups[0].clone(),
        destination_group: groups[1].clone(),
        destination_subnet: subnets[1].clone(),
    }
}

/// The `NetworkAclAssociationId` of the association the subnet `subnet_id` has — the ONE
/// association of that subnet, read off the item that names it rather than off the first one
/// the selected ACL reports, since one ACL holds the associations of every subnet it filters.
async fn acl_association(case: &str, router: &MultiServiceRouter, subnet_id: &str) -> String {
    let body = ok(
        case,
        router,
        "DescribeNetworkAcls",
        &[
            param("Filter.1.Name", "association.subnet-id"),
            param("Filter.1.Value.1", subnet_id),
        ],
    )
    .await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .find(|node| child_text(*node, "subnetId").as_deref() == Some(subnet_id))
        .and_then(|node| child_text(node, "networkAclAssociationId"))
        .unwrap_or_else(|| panic!("case {case:?}: no association for {subnet_id} in {body}"))
}

/// One analysis as the assertions below read it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Analysis {
    status: String,
    network_path_found: Option<String>,
    status_message: Option<String>,
    explanations: usize,
    components: usize,
}

/// The path from `source` to `destination` for `protocol` and `port`, as its
/// `NetworkInsightsPathId`.
async fn path(
    case: &str,
    router: &MultiServiceRouter,
    source: &str,
    destination: &str,
    protocol: &str,
    port: i32,
    token: &str,
) -> String {
    let body = ok(
        case,
        router,
        "CreateNetworkInsightsPath",
        &[
            param("Source", source),
            param("Destination", destination),
            param("Protocol", protocol),
            param("DestinationPort", &port.to_string()),
            param("ClientToken", token),
        ],
    )
    .await;
    member(case, &body, "networkInsightsPathId")
}

/// Start an analysis of `path_id` and look again until it is no longer running, or a generous
/// deadline passes.
///
/// The condition is looked at AGAIN in a loop rather than waited out, so nothing here depends
/// on how fast the host runs; the deadline is a multiple of the service's own transition span,
/// far enough out that a loaded machine is never the reason it fails.
async fn analyse(case: &str, router: &MultiServiceRouter, path_id: &str, token: &str) -> Analysis {
    let started = ok(
        case,
        router,
        "StartNetworkInsightsAnalysis",
        &[
            param("NetworkInsightsPathId", path_id),
            param("ClientToken", token),
        ],
    )
    .await;
    assert_eq!(
        element(&started, "status").as_deref(),
        Some(ANALYSIS_STATUS_RUNNING),
        "case {case:?}: an analysis is running when the start answers, got {started}"
    );
    let analysis_id = member(case, &started, "networkInsightsAnalysisId");
    let deadline = std::time::Instant::now() + TRANSITION_DELAY * 40;
    loop {
        let body = ok(
            case,
            router,
            "DescribeNetworkInsightsAnalyses",
            &indexed("NetworkInsightsAnalysisId", &[analysis_id.as_str()]),
        )
        .await;
        let status = member(case, &body, "status");
        if status != ANALYSIS_STATUS_RUNNING {
            return Analysis {
                status,
                network_path_found: element(&body, "networkPathFound"),
                status_message: element(&body, "statusMessage").map(|text| unescaped(&text)),
                explanations: element(&body, "explanationSet")
                    .map(|set| elements(&set, "item").len())
                    .unwrap_or_default(),
                components: element(&body, "forwardPathComponentSet")
                    .map(|set| elements(&set, "sequenceNumber").len())
                    .unwrap_or_default(),
            };
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the analysis {analysis_id} must end, last read {status:?}"
        );
        tokio::time::sleep(TRANSITION_DELAY / 10).await;
    }
}

/// One row of the table: the packet the path describes, and the rule that admits it.
struct Row {
    case: &'static str,
    protocol: &'static str,
    port: i32,
    /// How the admitting rule names where the traffic comes from: `true` names the source's own
    /// security group, `false` an address range.
    by_group: bool,
}

/// The packets analysed: both protocols the modelled `Protocol` catalog carries
/// (`smithy_query(query="shape",shape="Protocol")` -> `["tcp", "udp"]`), well-known and
/// high ports, and both forms a rule's source takes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "tcp/22 admitted from the source's group",
            protocol: "tcp",
            port: 22,
            by_group: true,
        },
        Row {
            case: "tcp/443 admitted from an address range",
            protocol: "tcp",
            port: 443,
            by_group: false,
        },
        Row {
            case: "udp/53 admitted from the source's group",
            protocol: "udp",
            port: 53,
            by_group: true,
        },
        Row {
            case: "tcp/8080 admitted from an address range",
            protocol: "tcp",
            port: 8080,
            by_group: false,
        },
    ]
}

/// The rule: the analysis succeeds either way, and the verdict follows the destination's
/// inbound rules — unreachable until one admits the packet, reachable once one does.
#[tokio::test]
async fn the_verdict_follows_the_destinations_inbound_rules() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let router = super::ec2_query::emulator().0;
        let network = network(case, &router, &index.to_string()).await;
        let path_id = path(
            case,
            &router,
            &network.source_instance,
            &network.destination_instance,
            row.protocol,
            row.port,
            &format!("path-{index}"),
        )
        .await;

        // Nothing admits the packet yet: a new security group has no inbound rule.
        let blocked = analyse(case, &router, &path_id, &format!("blocked-{index}")).await;
        assert_eq!(
            (blocked.status.as_str(), blocked.network_path_found.as_deref()),
            (ANALYSIS_STATUS_SUCCEEDED, Some("false")),
            "case {case:?}: expected a succeeded analysis reporting an unreachable path, got \
             {blocked:?}"
        );
        assert_eq!(
            blocked.status_message, None,
            "case {case:?}: an analysis that produced a result reports no failure, got \
             {blocked:?}"
        );
        assert!(
            blocked.explanations >= 1 && blocked.components >= 1,
            "case {case:?}: expected the components walked and the explanation of where the \
             packet stopped, got {blocked:?}"
        );

        // The rule a caller authorizes to let it in, named either way.
        let mut permission = vec![
            param("IpPermissions.1.IpProtocol", row.protocol),
            param("IpPermissions.1.FromPort", &row.port.to_string()),
            param("IpPermissions.1.ToPort", &row.port.to_string()),
            param("GroupId", &network.destination_group),
        ];
        if row.by_group {
            permission.push(param(
                "IpPermissions.1.Groups.1.GroupId",
                &network.source_group,
            ));
        } else {
            permission.push(param("IpPermissions.1.IpRanges.1.CidrIp", SOURCE_SUBNET_CIDR));
        }
        ok(
            case,
            &router,
            "AuthorizeSecurityGroupIngress",
            &permission,
        )
        .await;

        let admitted = analyse(case, &router, &path_id, &format!("admitted-{index}")).await;
        assert_eq!(
            (
                admitted.status.as_str(),
                admitted.network_path_found.as_deref()
            ),
            (ANALYSIS_STATUS_SUCCEEDED, Some("true")),
            "case {case:?}: expected the same path reachable once a rule admits it, got \
             {admitted:?}"
        );
        assert_eq!(
            admitted.explanations, 0,
            "case {case:?}: a packet that got through has nothing to explain, got {admitted:?}"
        );

        // A rule for ONE port admits that port and no other: the packet of a path on the port
        // beside it is still stopped.
        let other_port = if row.port == 22 { 23 } else { row.port + 1 };
        let other_path = path(
            case,
            &router,
            &network.source_instance,
            &network.destination_instance,
            row.protocol,
            other_port,
            &format!("other-path-{index}"),
        )
        .await;
        let other = analyse(case, &router, &other_path, &format!("other-{index}")).await;
        assert_eq!(
            (other.status.as_str(), other.network_path_found.as_deref()),
            (ANALYSIS_STATUS_SUCCEEDED, Some("false")),
            "case {case:?}: expected the packet on port {other_port} still stopped, got {other:?}"
        );
    }
}

/// The same rule one component further along the path: a network ACL that denies the packet
/// makes it unreachable even though the destination's own group admits it, and an entry that
/// allows it makes the same path reachable.
#[tokio::test]
async fn a_network_acl_on_the_path_decides_the_verdict_too() {
    let case = "a network ACL that denies the packet";
    let router = super::ec2_query::emulator().0;
    let network = network(case, &router, "acl").await;
    // The destination's own group admits the packet, so the ACL is the only thing left that can
    // stop it.
    ok(
        case,
        &router,
        "AuthorizeSecurityGroupIngress",
        &[
            param("GroupId", &network.destination_group),
            param("IpPermissions.1.IpProtocol", "tcp"),
            param("IpPermissions.1.FromPort", "22"),
            param("IpPermissions.1.ToPort", "22"),
            param("IpPermissions.1.IpRanges.1.CidrIp", "0.0.0.0/0"),
        ],
    )
    .await;
    let path_id = path(
        case,
        &router,
        &network.source_instance,
        &network.destination_instance,
        "tcp",
        22,
        "acl-path",
    )
    .await;
    let reachable = analyse(case, &router, &path_id, "acl-before").await;
    assert_eq!(
        (
            reachable.status.as_str(),
            reachable.network_path_found.as_deref()
        ),
        (ANALYSIS_STATUS_SUCCEEDED, Some("true")),
        "case {case:?}: the path must be reachable before the ACL is replaced, got {reachable:?}"
    );

    // A network ACL a caller creates "denies all inbound and outbound traffic until you add
    // rules", so putting it in front of the destination's subnet stops the packet.
    let acl = ok(
        case,
        &router,
        "CreateNetworkAcl",
        &[param("VpcId", &network.vpc_id)],
    )
    .await;
    let network_acl_id = member(case, &acl, "networkAclId");
    let association_id = acl_association(case, &router, &network.destination_subnet).await;
    ok(
        case,
        &router,
        "ReplaceNetworkAclAssociation",
        &[
            param("AssociationId", &association_id),
            param("NetworkAclId", &network_acl_id),
        ],
    )
    .await;

    let denied = analyse(case, &router, &path_id, "acl-denied").await;
    assert_eq!(
        (denied.status.as_str(), denied.network_path_found.as_deref()),
        (ANALYSIS_STATUS_SUCCEEDED, Some("false")),
        "case {case:?}: an ACL that denies the packet makes the path unreachable, got {denied:?}"
    );
    assert!(
        denied.explanations >= 1,
        "case {case:?}: expected the ACL that stopped the packet explained, got {denied:?}"
    );

    // An entry that allows it puts the path back.
    ok(
        case,
        &router,
        "CreateNetworkAclEntry",
        &[
            param("NetworkAclId", &network_acl_id),
            param("RuleNumber", "100"),
            param("Protocol", "6"),
            param("RuleAction", "allow"),
            param("Egress", "false"),
            param("CidrBlock", "0.0.0.0/0"),
            param("PortRange.From", "22"),
            param("PortRange.To", "22"),
        ],
    )
    .await;
    let allowed = analyse(case, &router, &path_id, "acl-allowed").await;
    assert_eq!(
        (
            allowed.status.as_str(),
            allowed.network_path_found.as_deref()
        ),
        (ANALYSIS_STATUS_SUCCEEDED, Some("true")),
        "case {case:?}: an ACL entry that allows the packet puts the path back, got {allowed:?}"
    );
}

/// A path whose ends are not two resources of one VPC is traced by no component of this
/// emulator, so the analysis still ends `failed` carrying the reason rather than a verdict
/// nothing computed.
#[tokio::test]
async fn a_path_this_emulator_traces_no_components_for_still_fails_with_its_reason() {
    let case = "a path from an internet gateway";
    let router = super::ec2_query::emulator().0;
    let network = network(case, &router, "untraced").await;
    let gateway = ok(case, &router, "CreateInternetGateway", &[]).await;
    let internet_gateway_id = member(case, &gateway, "internetGatewayId");
    let path_id = path(
        case,
        &router,
        &internet_gateway_id,
        &network.destination_instance,
        "tcp",
        443,
        "untraced-path",
    )
    .await;
    let ended = analyse(case, &router, &path_id, "untraced").await;
    assert_eq!(
        (ended.status.as_str(), ended.network_path_found.as_deref()),
        (ANALYSIS_STATUS_FAILED, None),
        "case {case:?}: expected the honest failure for a path nothing traced, got {ended:?}"
    );
    assert_eq!(
        ended.status_message.as_deref(),
        Some(PATH_ANALYSIS_UNSUPPORTED_STATUS_MESSAGE),
        "case {case:?}: expected the reason no result was produced, got {ended:?}"
    );
}
