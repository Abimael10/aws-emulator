// SPDX-License-Identifier: CC0-1.0

//! The DNS a managed node's script resolves a name through, driven end to end with Systems
//! Manager, Amazon EC2, IAM, Route 53 and Route 53 Resolver on one engine.
//!
//! RULE under test: A QUERY SENT FROM INSIDE A VPC IS ANSWERED OUT OF THE PRIVATE HOSTED
//! ZONES ASSOCIATED WITH THAT VPC — at the VPC's own resolver address and at the addresses of
//! an inbound Resolver endpoint in it — and a query to an endpoint address is delivered only
//! where one of that endpoint's security groups admits the query's protocol and port from the
//! querying address; a query no rule admits is DROPPED, so the client is answered with nothing
//! at all rather than with a refusal.
//!
//! Every case goes through the front door: the VPC, its subnets, its security group and the
//! launch over EC2's `ec2Query` wire, the role and instance profile over IAM's `awsQuery`
//! wire, the private hosted zone and its records over Route 53's `restXml` wire, the inbound
//! endpoint over Route 53 Resolver's `awsJson1_1` wire, and the query itself as a script
//! `AmazonSSM.SendCommand` delivers to the node, read back with
//! `AmazonSSM.GetCommandInvocation`.
//!
//! The rows span what the rule generalises over: WHICH address the query is sent to (the
//! link-local resolver, the VPC's own range plus two, each of the endpoint's two addresses,
//! and an address nothing answers at), WHICH name and type is asked for (a name the zone
//! holds, an alias chased to its address, an `AAAA`, a `TXT`, a name the zone holds nothing
//! at, and a name under no zone of the VPC), WHICH protocol carries it, and what the security
//! group admits as a rule is revoked and authorized again.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, Grant, AL2023_NAME_PATTERN, REGION, SSM_CORE_POLICY_ARN,
};
use super::run_command_in_the_guest::finish_due;

/// The published command document that runs a shell script on a Linux node.
const RUN_SHELL_SCRIPT: &str = "AWS-RunShellScript";

/// The VPC the whole suite runs in.
const VPC_CIDR: &str = "10.91.0.0/16";

/// Its two subnets, which is what an inbound Resolver endpoint needs: "you must specify at
/// least two IP addresses, in two different Availability Zones"
/// (`CreateResolverEndpointRequest$IpAddresses`, length 2..20).
const SUBNET_CIDRS: [&str; 2] = ["10.91.1.0/24", "10.91.2.0/24"];

/// The addresses the endpoint is created on, one per subnet.
const ENDPOINT_IPS: [&str; 2] = ["10.91.1.10", "10.91.2.10"];

/// The address every VPC's own resolver answers at.
const LINK_LOCAL_RESOLVER: &str = "169.254.169.253";

/// The same resolver at the base of this VPC's range plus two, which is the address its DHCP
/// option set publishes.
const VPC_RANGE_RESOLVER: &str = "10.91.0.2";

/// The private zone the records live in.
const ZONE: &str = "example.internal";

/// The address the zone's own record resolves to, from the documentation range RFC 5737
/// reserves — so it names no host of anything.
const RECORD_ADDRESS: &str = "192.0.2.10";

/// The `AAAA` record's address, from the documentation range RFC 3849 reserves.
const RECORD_ADDRESS_V6: &str = "2001:db8::10";

/// The `TXT` record's value, as a zone holds one (quoted character strings).
const RECORD_TEXT: &str = "owner=platform";

/// How long a case looks for a clock-owed move, far longer than the span it is owed after.
const DEADLINE: Duration = Duration::from_secs(60);

/// How often it looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The `CommandInvocationStatus` values that are terminal, from the modelled enum
/// (`smithy_query(query="shape",shape="CommandInvocationStatus")`).
const TERMINAL: &[&str] = &["Success", "Cancelled", "Failed", "TimedOut"];

/// The status `dig` exits with when nothing answered it (dig(1), Return Codes).
const NO_SERVERS_REACHED: i64 = 9;

/// The one line a script branches on when nothing came back.
const NO_SERVERS_LINE: &str = ";; no servers could be reached";

/// The status a shell answers for a command it could not run, which is what a guest answers for
/// an invocation of a program written in a form that program does not perform (POSIX, and
/// `service_common::posix_shell_session::Statuses::could_not_run`).
const COULD_NOT_RUN: i64 = 127;

/// The estate every case of this suite queries from: the node, the endpoint's addresses and
/// the security group gating them.
struct Estate {
    emulator: Emulator,
    vpc: String,
    instance: String,
    security_group: String,
}

/// Build it: a VPC with two subnets, a security group admitting DNS from the VPC, a node
/// whose profile lets its agent register, a private hosted zone with records, and an inbound
/// Resolver endpoint on [`ENDPOINT_IPS`].
async fn estate(case: &str) -> Estate {
    let emulator = Emulator::new();
    let vpc = Emulator::answered(
        &emulator
            .ec2(case, "CreateVpc", &[("CidrBlock", VPC_CIDR)])
            .await,
        "vpcId",
    )
    .unwrap_or_else(|| panic!("case {case:?}: CreateVpc must mint a VPC"));
    let mut subnets = Vec::new();
    for (index, cidr) in SUBNET_CIDRS.iter().enumerate() {
        let zone = format!("{REGION}{}", ["a", "b"][index]);
        let answered = emulator
            .ec2(
                case,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", cidr),
                    ("AvailabilityZone", zone.as_str()),
                ],
            )
            .await;
        subnets.push(
            Emulator::answered(&answered, "subnetId")
                .unwrap_or_else(|| panic!("case {case:?}: CreateSubnet must mint a subnet")),
        );
    }
    let security_group = Emulator::answered(
        &emulator
            .ec2(
                case,
                "CreateSecurityGroup",
                &[
                    ("GroupName", "resolver-endpoint"),
                    ("GroupDescription", "DNS to the inbound Resolver endpoint"),
                    ("VpcId", vpc.as_str()),
                ],
            )
            .await,
        "groupId",
    )
    .unwrap_or_else(|| panic!("case {case:?}: CreateSecurityGroup must mint a group"));
    for protocol in ["udp", "tcp"] {
        authorize(&emulator, case, &security_group, protocol).await;
    }

    // The node: an instance in the first subnet whose instance profile's role lets its agent
    // register, moved past the registration span so the suite is not spent waiting it out.
    let image = emulator.image(case, AL2023_NAME_PATTERN).await;
    let profile = emulator
        .profile(case, "guest-dns-role", &Grant::Attached(SSM_CORE_POLICY_ARN))
        .await;
    let instance = Emulator::answered(
        &emulator
            .ec2(
                case,
                "RunInstances",
                &[
                    ("ImageId", image.as_str()),
                    ("InstanceType", "t3.micro"),
                    ("MinCount", "1"),
                    ("MaxCount", "1"),
                    ("SubnetId", subnets[0].as_str()),
                    ("SecurityGroupId.1", security_group.as_str()),
                    ("IamInstanceProfile.Name", profile.as_str()),
                ],
            )
            .await,
        "instanceId",
    )
    .unwrap_or_else(|| panic!("case {case:?}: RunInstances must mint an instance"));
    emulator.launched_ago(&instance, launched_long_enough_ago());
    let reported = emulator.nodes(case, REGION).await;
    assert!(
        reported.contains_key(&instance),
        "case {case:?}: the launched instance must become a managed node first, got {:?}",
        reported.keys().collect::<Vec<_>>()
    );

    // The zone and its records, created over Route 53's own front door.
    let created = emulator
        .route53(
            case,
            "POST",
            "/hostedzone",
            &format!(
                "<CreateHostedZoneRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
                 <Name>{ZONE}</Name>\
                 <VPC><VPCId>{vpc}</VPCId><VPCRegion>{REGION}</VPCRegion></VPC>\
                 <CallerReference>guest-dns-suite</CallerReference>\
                 <HostedZoneConfig><PrivateZone>true</PrivateZone></HostedZoneConfig>\
                 </CreateHostedZoneRequest>"
            ),
            201,
        )
        .await;
    let zone_id = Emulator::answered(&created, "Id")
        .unwrap_or_else(|| panic!("case {case:?}: the create must answer the zone id: {created}"));
    let zone_id = zone_id.rsplit('/').next().unwrap_or_default().to_string();
    let changes = [
        ("app", "A", 300, RECORD_ADDRESS.to_string()),
        ("ipv6", "AAAA", 300, RECORD_ADDRESS_V6.to_string()),
        ("txt", "TXT", 60, format!("\"{RECORD_TEXT}\"")),
        ("www", "CNAME", 60, format!("app.{ZONE}")),
    ]
    .iter()
    .map(|(label, record_type, ttl, value)| {
        format!(
            "<Change><Action>CREATE</Action><ResourceRecordSet>\
             <Name>{label}.{ZONE}</Name><Type>{record_type}</Type><TTL>{ttl}</TTL>\
             <ResourceRecords><ResourceRecord><Value>{value}</Value></ResourceRecord>\
             </ResourceRecords></ResourceRecordSet></Change>"
        )
    })
    .collect::<String>();
    emulator
        .route53(
            case,
            "POST",
            &format!("/hostedzone/{zone_id}/rrset"),
            &format!(
                "<ChangeResourceRecordSetsRequest \
                 xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
                 <ChangeBatch><Comment>the zone's own records</Comment>\
                 <Changes>{changes}</Changes></ChangeBatch>\
                 </ChangeResourceRecordSetsRequest>"
            ),
            200,
        )
        .await;

    // The inbound endpoint, on an address of each subnet, behind the group above.
    let endpoint = emulator
        .resolver(
            case,
            "CreateResolverEndpoint",
            &json!({
                "CreatorRequestId": "guest-dns-suite",
                "Name": "inbound",
                "Direction": "INBOUND",
                "SecurityGroupIds": [security_group],
                "IpAddresses": [
                    {"SubnetId": subnets[0], "Ip": ENDPOINT_IPS[0]},
                    {"SubnetId": subnets[1], "Ip": ENDPOINT_IPS[1]},
                ],
            }),
        )
        .await;
    let endpoint_id = endpoint["ResolverEndpoint"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create must mint an endpoint: {endpoint}"))
        .to_string();
    // An endpoint answers nothing until it is OPERATIONAL: the suite waits for that status
    // rather than for a span of the clock.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let read = emulator
            .resolver(
                case,
                "GetResolverEndpoint",
                &json!({"ResolverEndpointId": endpoint_id}),
            )
            .await;
        let status = read["ResolverEndpoint"]["Status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == "OPERATIONAL" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the endpoint must reach OPERATIONAL, still {status:?}: {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    Estate {
        emulator,
        vpc,
        instance,
        security_group,
    }
}

/// Authorize DNS on `protocol` from the whole VPC into `group`.
async fn authorize(emulator: &Emulator, case: &str, group: &str, protocol: &str) {
    emulator
        .ec2(
            case,
            "AuthorizeSecurityGroupIngress",
            &[
                ("GroupId", group),
                ("IpPermissions.1.IpProtocol", protocol),
                ("IpPermissions.1.FromPort", "53"),
                ("IpPermissions.1.ToPort", "53"),
                ("IpPermissions.1.IpRanges.1.CidrIp", VPC_CIDR),
            ],
        )
        .await;
}

/// Revoke that same rule, which is the one change the denial case makes.
async fn revoke(emulator: &Emulator, case: &str, group: &str, protocol: &str) {
    emulator
        .ec2(
            case,
            "RevokeSecurityGroupIngress",
            &[
                ("GroupId", group),
                ("IpPermissions.1.IpProtocol", protocol),
                ("IpPermissions.1.FromPort", "53"),
                ("IpPermissions.1.ToPort", "53"),
                ("IpPermissions.1.IpRanges.1.CidrIp", VPC_CIDR),
            ],
        )
        .await;
}

/// Run `script` in the node's guest and answer the finished invocation.
async fn ran(estate: &Estate, case: &str, script: &str) -> Value {
    let (status, answered) = estate
        .emulator
        .ssm(
            REGION,
            "SendCommand",
            &json!({
                "DocumentName": RUN_SHELL_SCRIPT,
                "InstanceIds": [estate.instance],
                "Parameters": {"commands": [script]},
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the send must be served, got {status}: {answered}"
    );
    let command_id = answered["Command"]["CommandId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the send must mint a command id: {answered}"))
        .to_string();
    finish_due(&estate.emulator, &command_id);
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, answered) = estate
            .emulator
            .ssm(
                REGION,
                "GetCommandInvocation",
                &json!({"CommandId": command_id, "InstanceId": estate.instance}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must be served, got {status}: {answered}"
        );
        let reached = answered["Status"].as_str().unwrap_or_default().to_string();
        if TERMINAL.contains(&reached.as_str()) {
            return answered;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the invocation to finish, still {reached:?}: {answered}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What the guest wrote, and the exit code its shell ended on.
async fn output(estate: &Estate, case: &str, script: &str) -> (String, i64) {
    let answered = ran(estate, case, script).await;
    let written = answered["StandardOutputContent"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let code = answered["ResponseCode"].as_i64().unwrap_or_default();
    (written, code)
}

/// RULE: every address the VPC's DNS answers at answers the VPC's own zone, and an address
/// nothing answers at answers nothing at all.
#[tokio::test]
async fn every_address_the_vpcs_dns_answers_at_answers_its_private_zone() {
    let case = "the address table";
    let estate = estate(case).await;
    // (case, the address the query is sent to, what comes back, the exit code)
    let rows: &[(&str, &str, &str, i64)] = &[
        (
            "the link-local resolver of the VPC",
            LINK_LOCAL_RESOLVER,
            RECORD_ADDRESS,
            0,
        ),
        (
            "the VPC's own range plus two",
            VPC_RANGE_RESOLVER,
            RECORD_ADDRESS,
            0,
        ),
        (
            "the endpoint's address in the first subnet",
            ENDPOINT_IPS[0],
            RECORD_ADDRESS,
            0,
        ),
        (
            "the endpoint's address in the second subnet",
            ENDPOINT_IPS[1],
            RECORD_ADDRESS,
            0,
        ),
        (
            "an address of the VPC nothing answers DNS at",
            "10.91.9.9",
            "",
            NO_SERVERS_REACHED,
        ),
    ];
    for (row, server, expected, code) in rows {
        let (written, answered_code) = output(
            &estate,
            row,
            &format!("dig +short @{server} app.{ZONE} A"),
        )
        .await;
        if expected.is_empty() {
            assert!(
                written.contains(NO_SERVERS_LINE),
                "case {row:?}: expected {NO_SERVERS_LINE:?} for an address nothing answers at, \
                 got {written:?}"
            );
        } else {
            assert_eq!(
                written,
                format!("{expected}\n"),
                "case {row:?}: expected the zone's own record, got {written:?}"
            );
        }
        assert_eq!(
            answered_code, *code,
            "case {row:?}: expected exit {code}, got {answered_code}"
        );
    }

    // The VPC's own resolver answers only while the VPC's DNS SUPPORT is on: turning the
    // attribute off takes both of its addresses away, and leaves the endpoint's own answering.
    estate
        .emulator
        .ec2(
            case,
            "ModifyVpcAttribute",
            &[
                ("VpcId", estate.vpc.as_str()),
                ("EnableDnsSupport.Value", "false"),
            ],
        )
        .await;
    for (row, server, answers) in [
        (
            "the link-local resolver with DNS support off",
            LINK_LOCAL_RESOLVER,
            false,
        ),
        (
            "the VPC's range plus two with DNS support off",
            VPC_RANGE_RESOLVER,
            false,
        ),
        (
            "the endpoint's address with DNS support off",
            ENDPOINT_IPS[0],
            true,
        ),
    ] {
        let (written, code) = output(
            &estate,
            row,
            &format!("dig +short @{server} app.{ZONE} A"),
        )
        .await;
        if answers {
            assert_eq!(
                written,
                format!("{RECORD_ADDRESS}\n"),
                "case {row:?}: expected the zone's own record, got {written:?}"
            );
            assert_eq!(code, 0, "case {row:?}: expected exit 0, got {code}");
        } else {
            assert!(
                written.contains(NO_SERVERS_LINE),
                "case {row:?}: expected a query nothing answered, got {written:?}"
            );
            assert_eq!(
                code, NO_SERVERS_REACHED,
                "case {row:?}: expected exit {NO_SERVERS_REACHED}, got {code}"
            );
        }
    }
}

/// RULE: the answer is what the zone holds for the name and type asked for — including an
/// alias chased to the address it names — and a name the zone holds nothing at, or that falls
/// under no zone of the VPC, is answered with the status a resolver gives for it.
#[tokio::test]
async fn the_answer_is_what_the_zone_holds_for_the_name_asked_for() {
    let case = "the name table";
    let estate = estate(case).await;
    /// One row: the name and type asked for, and the values that come back.
    struct NameRow {
        case: &'static str,
        name: String,
        record_type: &'static str,
        values: Vec<String>,
    }
    let rows = vec![
        NameRow {
            case: "a name the zone holds",
            name: format!("app.{ZONE}"),
            record_type: "A",
            values: vec![RECORD_ADDRESS.to_string()],
        },
        NameRow {
            case: "a name written fully qualified",
            name: format!("app.{ZONE}."),
            record_type: "A",
            values: vec![RECORD_ADDRESS.to_string()],
        },
        NameRow {
            case: "a name written in another case",
            name: format!("APP.{}", ZONE.to_uppercase()),
            record_type: "A",
            values: vec![RECORD_ADDRESS.to_string()],
        },
        NameRow {
            case: "an alias chased to the address it names",
            name: format!("www.{ZONE}"),
            record_type: "A",
            values: vec![format!("app.{ZONE}"), RECORD_ADDRESS.to_string()],
        },
        NameRow {
            case: "the alias itself",
            name: format!("www.{ZONE}"),
            record_type: "CNAME",
            values: vec![format!("app.{ZONE}")],
        },
        NameRow {
            case: "an IPv6 record",
            name: format!("ipv6.{ZONE}"),
            record_type: "AAAA",
            values: vec![RECORD_ADDRESS_V6.to_string()],
        },
        NameRow {
            case: "a text record",
            name: format!("txt.{ZONE}"),
            record_type: "TXT",
            values: vec![format!("\"{RECORD_TEXT}\"")],
        },
        NameRow {
            case: "a name the zone holds this type none of",
            name: format!("txt.{ZONE}"),
            record_type: "A",
            values: Vec::new(),
        },
        NameRow {
            case: "a name the zone holds nothing at",
            name: format!("absent.{ZONE}"),
            record_type: "A",
            values: Vec::new(),
        },
        NameRow {
            case: "a name under no zone of the VPC",
            name: "broker.example.com".to_string(),
            record_type: "A",
            values: Vec::new(),
        },
    ];
    for row in &rows {
        let (written, code) = output(
            &estate,
            row.case,
            &format!(
                "dig +short @{LINK_LOCAL_RESOLVER} {} {}",
                row.name, row.record_type
            ),
        )
        .await;
        let values: Vec<String> = written.lines().map(str::to_string).collect();
        assert_eq!(
            values, row.values,
            "case {:?}: expected {:?}, got {values:?}",
            row.case, row.values
        );
        assert_eq!(
            code, 0,
            "case {:?}: a query that was answered exits zero, got {code}",
            row.case
        );
    }

    // The status the full form reports for each of those three outcomes, which is what a
    // client reads when it is not asking for the values alone.
    let statuses: &[(&str, String, &str)] = &[
        ("a name the zone holds", format!("app.{ZONE}"), "NOERROR"),
        (
            "a name the zone holds nothing at",
            format!("absent.{ZONE}"),
            "NXDOMAIN",
        ),
        ("a name under no zone of the VPC", "broker.example.com".to_string(), "SERVFAIL"),
    ];
    for (row, name, status) in statuses {
        let (written, _code) = output(
            &estate,
            row,
            &format!("dig @{LINK_LOCAL_RESOLVER} {name} A"),
        )
        .await;
        assert!(
            written.contains(&format!("status: {status}")),
            "case {row:?}: expected the header to report status {status}, got {written:?}"
        );
        assert!(
            written.contains(&format!(";; SERVER: {LINK_LOCAL_RESOLVER}#53")),
            "case {row:?}: expected the server the query was answered by, got {written:?}"
        );
    }
}

/// RULE: a query to an endpoint address is delivered only where the endpoint's security groups
/// admit its protocol and port from the querying address — so revoking the rule denies the
/// query (nothing comes back), the protocol whose rule is still there keeps answering, the
/// VPC's own resolver is not gated at all, and authorizing the rule again admits it.
#[tokio::test]
async fn revoking_the_endpoints_dns_rule_denies_the_query_from_inside_the_vpc() {
    let case = "the security group gate";
    let estate = estate(case).await;
    let query = format!("dig +short @{} app.{ZONE} A", ENDPOINT_IPS[0]);
    let over_tcp = format!("dig +short +tcp @{} app.{ZONE} A", ENDPOINT_IPS[0]);
    let through_vpc_resolver = format!("dig +short @{LINK_LOCAL_RESOLVER} app.{ZONE} A");

    let (written, code) = output(&estate, "with the rule in place", &query).await;
    assert_eq!(
        written,
        format!("{RECORD_ADDRESS}\n"),
        "case \"with the rule in place\": expected the zone's own record, got {written:?}"
    );
    assert_eq!(
        code, 0,
        "case \"with the rule in place\": an answered query exits zero, got {code}"
    );

    revoke(&estate.emulator, case, &estate.security_group, "udp").await;

    let (written, code) = output(&estate, "the datagram query once the rule is revoked", &query).await;
    assert!(
        written.contains(NO_SERVERS_LINE)
            && written.contains(&format!(
                ";; communications error to {}#53: timed out",
                ENDPOINT_IPS[0]
            )),
        "case \"the datagram query once the rule is revoked\": expected a query that was never \
         answered, got {written:?}"
    );
    assert!(
        !written.contains(RECORD_ADDRESS),
        "case \"the datagram query once the rule is revoked\": a denied query carries no answer, \
         got {written:?}"
    );
    assert_eq!(
        code, NO_SERVERS_REACHED,
        "case \"the datagram query once the rule is revoked\": expected exit \
         {NO_SERVERS_REACHED}, got {code}"
    );

    let (written, code) = output(
        &estate,
        "the same query on the protocol whose rule is still there",
        &over_tcp,
    )
    .await;
    assert_eq!(
        written,
        format!("{RECORD_ADDRESS}\n"),
        "case \"the same query on the protocol whose rule is still there\": expected the record, \
         got {written:?}"
    );
    assert_eq!(
        code, 0,
        "case \"the same query on the protocol whose rule is still there\": expected exit 0, got \
         {code}"
    );

    let (written, _code) = output(
        &estate,
        "the VPC's own resolver, which no security group gates",
        &through_vpc_resolver,
    )
    .await;
    assert_eq!(
        written,
        format!("{RECORD_ADDRESS}\n"),
        "case \"the VPC's own resolver, which no security group gates\": expected the record, got \
         {written:?}"
    );

    authorize(&estate.emulator, case, &estate.security_group, "udp").await;

    let (written, code) = output(&estate, "the rule authorized again", &query).await;
    assert_eq!(
        written,
        format!("{RECORD_ADDRESS}\n"),
        "case \"the rule authorized again\": expected the record to be answered again, got \
         {written:?}"
    );
    assert_eq!(
        code, 0,
        "case \"the rule authorized again\": expected exit 0, got {code}"
    );
}

/// RULE: the lookup client of the guest reads the same DNS — it reports the server it asked,
/// the name and the address it was answered, and the resolver's own status for a name that
/// could not be found.
#[tokio::test]
async fn the_guests_lookup_client_reads_the_same_dns() {
    let case = "the lookup client";
    let estate = estate(case).await;
    let (written, code) = output(
        &estate,
        "a name the zone holds",
        &format!("nslookup app.{ZONE} {LINK_LOCAL_RESOLVER}"),
    )
    .await;
    for expected in [
        format!("Server:\t\t{LINK_LOCAL_RESOLVER}"),
        format!("Address:\t{LINK_LOCAL_RESOLVER}#53"),
        format!("Name:\tapp.{ZONE}"),
        format!("Address:\t{RECORD_ADDRESS}"),
    ] {
        assert!(
            written.contains(&expected),
            "case \"a name the zone holds\": expected {expected:?} in the lookup's output, got \
             {written:?}"
        );
    }
    assert_eq!(
        code, 0,
        "case \"a name the zone holds\": a lookup that was answered exits zero, got {code}"
    );

    let (written, code) = output(
        &estate,
        "a name the zone holds nothing at",
        &format!("nslookup absent.{ZONE} {LINK_LOCAL_RESOLVER}"),
    )
    .await;
    assert!(
        written.contains(&format!("** server can't find absent.{ZONE}: NXDOMAIN")),
        "case \"a name the zone holds nothing at\": expected the resolver's own status, got \
         {written:?}"
    );
    assert_eq!(
        code, 1,
        "case \"a name the zone holds nothing at\": a lookup that failed exits one, got {code}"
    );

    // The TYPE the lookup names is the type it asks for, not a type this client chose for it:
    // `-type=CNAME` answers the alias, `-type=TXT` the text record, and a setting this client
    // has no reading for performs nothing at all rather than answering another query.
    let (written, code) = output(
        &estate,
        "the type the lookup names",
        &format!("nslookup -type=CNAME www.{ZONE} {LINK_LOCAL_RESOLVER}"),
    )
    .await;
    assert!(
        written.contains(&format!("Value:\tapp.{ZONE}")),
        "case \"the type the lookup names\": expected the alias the zone holds, got {written:?}"
    );
    assert_eq!(
        code, 0,
        "case \"the type the lookup names\": expected exit 0, got {code}"
    );

    let (written, code) = output(
        &estate,
        "a text record the lookup names",
        &format!("nslookup -querytype=txt txt.{ZONE} {LINK_LOCAL_RESOLVER}"),
    )
    .await;
    assert!(
        written.contains(&format!("Value:\t\"{RECORD_TEXT}\"")),
        "case \"a text record the lookup names\": expected the text the zone holds, got \
         {written:?}"
    );
    assert_eq!(
        code, 0,
        "case \"a text record the lookup names\": expected exit 0, got {code}"
    );

    let answered = ran(
        &estate,
        "a setting the lookup client has no reading for",
        &format!("nslookup -norecurse app.{ZONE} {LINK_LOCAL_RESOLVER}"),
    )
    .await;
    // The agent reports the two streams apart, so a command this guest could not run writes
    // nothing and the shell's own diagnostic for it is the invocation's standard error.
    assert_eq!(
        (
            answered["StandardOutputContent"].as_str(),
            answered["StandardErrorContent"].as_str(),
            answered["ResponseCode"].as_i64(),
        ),
        (
            Some(""),
            Some("sh: nslookup: command not found\n"),
            Some(COULD_NOT_RUN),
        ),
        "case \"a setting the lookup client has no reading for\": expected the status a shell \
         answers for a command it could not run, and the diagnostic it writes for one, got \
         {answered}"
    );
}
