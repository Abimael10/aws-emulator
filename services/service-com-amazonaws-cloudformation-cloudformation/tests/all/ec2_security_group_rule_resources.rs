// SPDX-License-Identifier: CC0-1.0

//! The security-group RULES a stack declares as resources of their own, read back through
//! AMAZON EC2's own wire.
//!
//! RULE: an `AWS::EC2::SecurityGroupIngress` or `AWS::EC2::SecurityGroupEgress` resource IS one
//! rule of the security group it names — the same permission an inline rule of
//! `AWS::EC2::SecurityGroup` declares, under the same member names — so a stack that reports
//! `CREATE_COMPLETE` has added that rule in Amazon EC2: `DescribeSecurityGroups` reports it
//! among the group's permissions on the side the declaration put it, the stack publishes the
//! `sgr-…` identifier that service minted for the rule it added, `DescribeSecurityGroupRules`
//! reports that rule under that identifier, and deleting the stack takes the rule away again.
//!
//! Unlike an inline OUTBOUND declaration on the group — which REPLACES the all-traffic rule
//! Amazon EC2 gives a new group — a rule resource adds to what the group holds, so a group
//! whose outbound rule is declared this way still carries that default rule beside it.
//!
//! The rows differ along what the rule generalises over: the direction, which kind of source or
//! destination the rule names (an IPv4 block, an IPv6 block, a group created outside the stack
//! and named by an EC2-typed parameter), whether the rule carries a description, whether the
//! protocol takes a port range at all, how many rules one group is given, and the template
//! spelling (YAML with short-form intrinsics, JSON).

use service_com_amazonaws_ec2_amazonec2::state::INVALID_GROUP_NOT_FOUND;
use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{
    acknowledging_iam, call, create_stack, ec2, is_service_assigned_id, member, output_value,
    router, settled_stack, xml_blocks, xml_text, xml_texts, SERVICE_ID_HEX_DIGITS,
};

/// One rule a row expects on one side of the group, as Amazon EC2 reports a permission: the
/// protocol, the port bounds (none for a protocol that takes no port range), the IPv4 and IPv6
/// blocks it grants, whether it references the peer group the row created outside the stack,
/// and the description it carries.
#[derive(Debug, PartialEq, Eq)]
struct Permission {
    protocol: &'static str,
    ports: Option<(&'static str, &'static str)>,
    ipv4: Option<&'static str>,
    ipv6: Option<&'static str>,
    references_the_peer: bool,
    description: Option<&'static str>,
}

/// The one outbound rule Amazon EC2 gives every new security group: the all-protocols rule over
/// every IPv4 address. A rule RESOURCE adds to the group's rules, so this one stays where it is
/// — which is what the inline `SecurityGroupEgress` property of the group would have replaced.
const THE_ALL_TRAFFIC_RULE: Permission = Permission {
    protocol: "-1",
    ports: None,
    ipv4: Some("0.0.0.0/0"),
    ipv6: None,
    references_the_peer: false,
    description: None,
};

/// What a row expects of the rules the stack declared.
struct RuleCase {
    case: &'static str,
    stack: &'static str,
    /// The template, with `PEER` where the row's peer-group parameter is referenced.
    template: &'static str,
    /// The logical ids of the rule resources, in the order their `Outputs` publish them.
    rules: &'static [&'static str],
    /// The inbound rules the group must hold, in protocol-then-port order.
    inbound: &'static [Permission],
    /// The outbound rules it must hold, read the same way.
    outbound: &'static [Permission],
}

const RULE_CASES: &[RuleCase] = &[
    RuleCase {
        case: "an inbound rule from an IPv4 block, carrying a description, in YAML",
        stack: "rule-from-a-block",
        template: concat!(
            "Parameters:\n",
            "  Peer:\n",
            "    Type: AWS::EC2::SecurityGroup::Id\n",
            "Resources:\n",
            "  Net:\n",
            "    Type: AWS::EC2::VPC\n",
            "    Properties:\n",
            "      CidrBlock: 10.80.0.0/16\n",
            "  Primary:\n",
            "    Type: AWS::EC2::SecurityGroup\n",
            "    Properties:\n",
            "      GroupDescription: the primary group\n",
            "      VpcId: !Ref Net\n",
            "  Https:\n",
            "    Type: AWS::EC2::SecurityGroupIngress\n",
            "    Properties:\n",
            "      GroupId: !GetAtt Primary.GroupId\n",
            "      IpProtocol: tcp\n",
            "      FromPort: 443\n",
            "      ToPort: 443\n",
            "      CidrIp: 10.80.0.0/16\n",
            "      Description: https from the network\n",
            "Outputs:\n",
            "  GroupId:\n",
            "    Value: !Ref Primary\n",
            "  HttpsRef:\n",
            "    Value: !Ref Https\n",
            "  HttpsId:\n",
            "    Value: !GetAtt Https.Id\n",
        ),
        rules: &["Https"],
        inbound: &[Permission {
            protocol: "tcp",
            ports: Some(("443", "443")),
            ipv4: Some("10.80.0.0/16"),
            ipv6: None,
            references_the_peer: false,
            description: Some("https from the network"),
        }],
        outbound: &[THE_ALL_TRAFFIC_RULE],
    },
    RuleCase {
        case: "an inbound rule referencing a group created outside the stack, named by an \
               EC2-typed parameter, in JSON",
        stack: "rule-from-the-peer",
        template: r#"{"Parameters":{"Peer":{"Type":"AWS::EC2::SecurityGroup::Id"}},
            "Resources":{
              "Primary":{"Type":"AWS::EC2::SecurityGroup","Properties":{
                "GroupDescription":"the primary group"}},
              "FromPeer":{"Type":"AWS::EC2::SecurityGroupIngress","Properties":{
                "GroupId":{"Fn::GetAtt":["Primary","GroupId"]},
                "IpProtocol":"tcp","FromPort":8080,"ToPort":8080,
                "SourceSecurityGroupId":{"Ref":"Peer"}}}},
            "Outputs":{
              "GroupId":{"Value":{"Ref":"Primary"}},
              "FromPeerRef":{"Value":{"Ref":"FromPeer"}},
              "FromPeerId":{"Value":{"Fn::GetAtt":["FromPeer","Id"]}}}}"#,
        rules: &["FromPeer"],
        inbound: &[Permission {
            protocol: "tcp",
            ports: Some(("8080", "8080")),
            ipv4: None,
            ipv6: None,
            references_the_peer: true,
            description: None,
        }],
        outbound: &[THE_ALL_TRAFFIC_RULE],
    },
    RuleCase {
        case: "an all-protocol inbound rule from an IPv6 block, which takes no port range, \
               beside an outbound rule to an IPv4 block, in YAML",
        stack: "rule-both-directions",
        template: concat!(
            "Parameters:\n",
            "  Peer:\n",
            "    Type: AWS::EC2::SecurityGroup::Id\n",
            "Resources:\n",
            "  Net:\n",
            "    Type: AWS::EC2::VPC\n",
            "    Properties:\n",
            "      CidrBlock: 10.81.0.0/16\n",
            "  Edge:\n",
            "    Type: AWS::EC2::SecurityGroup\n",
            "    Properties:\n",
            "      GroupDescription: the edge group\n",
            "      VpcId: !Ref Net\n",
            "  AnyV6:\n",
            "    Type: AWS::EC2::SecurityGroupIngress\n",
            "    Properties:\n",
            "      GroupId: !GetAtt Edge.GroupId\n",
            "      IpProtocol: \"-1\"\n",
            "      CidrIpv6: 2001:db8::/32\n",
            "  DnsOut:\n",
            "    Type: AWS::EC2::SecurityGroupEgress\n",
            "    Properties:\n",
            "      GroupId: !GetAtt Edge.GroupId\n",
            "      IpProtocol: udp\n",
            "      FromPort: 53\n",
            "      ToPort: 53\n",
            "      CidrIp: 10.82.0.0/16\n",
            "      Description: dns out of the network\n",
            "Outputs:\n",
            "  GroupId:\n",
            "    Value: !Ref Edge\n",
            "  AnyV6Ref:\n",
            "    Value: !Ref AnyV6\n",
            "  AnyV6Id:\n",
            "    Value: !GetAtt AnyV6.Id\n",
            "  DnsOutRef:\n",
            "    Value: !Ref DnsOut\n",
            "  DnsOutId:\n",
            "    Value: !GetAtt DnsOut.Id\n",
        ),
        rules: &["AnyV6", "DnsOut"],
        inbound: &[Permission {
            protocol: "-1",
            ports: None,
            ipv4: None,
            ipv6: Some("2001:db8::/32"),
            references_the_peer: false,
            description: None,
        }],
        outbound: &[
            THE_ALL_TRAFFIC_RULE,
            Permission {
                protocol: "udp",
                ports: Some(("53", "53")),
                ipv4: Some("10.82.0.0/16"),
                ipv6: None,
                references_the_peer: false,
                description: Some("dns out of the network"),
            },
        ],
    },
    RuleCase {
        case: "two inbound rules on one group, one per source kind, in JSON",
        stack: "rules-two-sources",
        // The group is left in the Region's default VPC, which is where the peer group this
        // row references was created: "you can't reference a security group of another VPC".
        template: r#"{"Parameters":{"Peer":{"Type":"AWS::EC2::SecurityGroup::Id"}},
            "Resources":{
              "Core":{"Type":"AWS::EC2::SecurityGroup","Properties":{
                "GroupDescription":"the core group"}},
              "FromBlock":{"Type":"AWS::EC2::SecurityGroupIngress","Properties":{
                "GroupId":{"Fn::GetAtt":["Core","GroupId"]},
                "IpProtocol":"tcp","FromPort":22,"ToPort":22,"CidrIp":"10.83.0.0/16"}},
              "FromPeer":{"Type":"AWS::EC2::SecurityGroupIngress","Properties":{
                "GroupId":{"Fn::GetAtt":["Core","GroupId"]},
                "IpProtocol":"tcp","FromPort":9000,"ToPort":9100,
                "SourceSecurityGroupId":{"Ref":"Peer"},
                "Description":"the workers of the peer"}}},
            "Outputs":{
              "GroupId":{"Value":{"Ref":"Core"}},
              "FromBlockRef":{"Value":{"Ref":"FromBlock"}},
              "FromBlockId":{"Value":{"Fn::GetAtt":["FromBlock","Id"]}},
              "FromPeerRef":{"Value":{"Ref":"FromPeer"}},
              "FromPeerId":{"Value":{"Fn::GetAtt":["FromPeer","Id"]}}}}"#,
        rules: &["FromBlock", "FromPeer"],
        inbound: &[
            Permission {
                protocol: "tcp",
                ports: Some(("22", "22")),
                ipv4: Some("10.83.0.0/16"),
                ipv6: None,
                references_the_peer: false,
                description: None,
            },
            Permission {
                protocol: "tcp",
                ports: Some(("9000", "9100")),
                ipv4: None,
                ipv6: None,
                references_the_peer: true,
                description: Some("the workers of the peer"),
            },
        ],
    outbound: &[THE_ALL_TRAFFIC_RULE],
    },
];

/// The peer security group every row names through its EC2-typed parameter: a group created
/// OUTSIDE the stack, through Amazon EC2's own wire, so a rule that references it references a
/// group the stack did not make.
async fn peer_group(router: &MultiServiceRouter, case: &str) -> String {
    let (status, body) = ec2(
        router,
        "CreateSecurityGroup",
        &[
            ("GroupName", "the-peer-group"),
            ("GroupDescription", "a group created outside the stack"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSecurityGroup must answer 200, got {status} {body}"
    );
    xml_text(&body, "groupId").unwrap_or_default()
}

/// Every permission Amazon EC2 reports on one side of the described group, as the rows state
/// them: the aggregate `ipPermissions` / `ipPermissionsEgress` member is what a client reads a
/// group's rules off, and a rule that was dropped, mis-directed or stripped of its source is
/// missing from it.
fn reported_permissions(described: &str, direction: &str, peer: &str) -> Vec<ReportedPermission> {
    let mut reported: Vec<ReportedPermission> = xml_blocks(described, direction)
        .iter()
        .flat_map(|block| xml_blocks(block, "item"))
        .map(|item| ReportedPermission {
            protocol: xml_text(&item, "ipProtocol").unwrap_or_default(),
            ports: xml_text(&item, "fromPort")
                .zip(xml_text(&item, "toPort")),
            ipv4: xml_text(&item, "cidrIp"),
            ipv6: xml_text(&item, "cidrIpv6"),
            references_the_peer: xml_texts(&item, "groupId").iter().any(|id| id == peer),
            description: xml_text(&item, "description"),
        })
        .collect();
    reported.sort_by(|left, right| {
        (&left.protocol, &left.ports).cmp(&(&right.protocol, &right.ports))
    });
    reported
}

/// One permission as the group reports it, compared against the row's own [`Permission`].
#[derive(Debug, PartialEq, Eq)]
struct ReportedPermission {
    protocol: String,
    ports: Option<(String, String)>,
    ipv4: Option<String>,
    ipv6: Option<String>,
    references_the_peer: bool,
    description: Option<String>,
}

/// The row's expectation in the form the group reports one, sorted the same way.
fn expected_permissions(expected: &[Permission]) -> Vec<ReportedPermission> {
    let mut rows: Vec<ReportedPermission> = expected
        .iter()
        .map(|permission| ReportedPermission {
            protocol: permission.protocol.to_string(),
            ports: permission
                .ports
                .map(|(from, to)| (from.to_string(), to.to_string())),
            ipv4: permission.ipv4.map(ToString::to_string),
            ipv6: permission.ipv6.map(ToString::to_string),
            references_the_peer: permission.references_the_peer,
            description: permission.description.map(ToString::to_string),
        })
        .collect();
    rows.sort_by(|left, right| {
        (&left.protocol, &left.ports).cmp(&(&right.protocol, &right.ports))
    });
    rows
}

#[tokio::test]
async fn a_declared_security_group_rule_is_authorized_in_ec2() {
    for row in RULE_CASES {
        let case = row.case;
        let router = router();
        let peer = peer_group(&router, case).await;
        let (status, body) = create_stack(
            &router,
            row.stack,
            row.template,
            &acknowledging_iam(&[
                member("Parameters.member.1.ParameterKey", "Peer"),
                member("Parameters.member.1.ParameterValue", &peer),
            ]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack answered {status} with {body}"
        );
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {described}"
        );

        // Each rule resource publishes the identifier Amazon EC2 minted for the rule it added,
        // under its `Ref` and its `Id` attribute alike, and that service reports the rule
        // under it — the rule exists where rules live rather than only in the stack's record.
        let group_id = output_value(&described, "GroupId").unwrap_or_default();
        for logical_id in row.rules {
            let published = output_value(&described, &format!("{logical_id}Ref"))
                .unwrap_or_default();
            assert!(
                is_service_assigned_id(&published, "sgr"),
                "case {case:?}: expected {logical_id}'s Ref to carry sgr- plus \
                 {SERVICE_ID_HEX_DIGITS} hexadecimal characters, got {published:?}"
            );
            assert_eq!(
                output_value(&described, &format!("{logical_id}Id")).as_deref(),
                Some(published.as_str()),
                "case {case:?}: expected {logical_id}'s Id attribute to carry the id its Ref \
                 answered, got {described}"
            );
            let (status, rule) = ec2(
                &router,
                "DescribeSecurityGroupRules",
                &[("SecurityGroupRuleId.1", &published)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: EC2 must report the rule {published:?} the stack added, got \
                 {status} {rule}"
            );
            assert_eq!(
                xml_text(&rule, "groupId").as_deref(),
                Some(group_id.as_str()),
                "case {case:?}: expected the rule on the stack's group {group_id:?}, got {rule}"
            );
        }

        // ...and the group itself reports those rules, on the side each declaration put it.
        let (status, group) =
            ec2(&router, "DescribeSecurityGroups", &[("GroupId.1", &group_id)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: EC2 must report the group {group_id:?}, got {status} {group}"
        );
        for (direction, expected) in [
            ("ipPermissions", row.inbound),
            ("ipPermissionsEgress", row.outbound),
        ] {
            let reported = reported_permissions(&group, direction, &peer);
            let wanted = expected_permissions(expected);
            assert_eq!(
                reported, wanted,
                "case {case:?}: expected the {direction} the declarations asked for \
                 ({wanted:?}), got {reported:?} from {group}"
            );
        }

        // Deleting the stack revokes each rule and takes the group away, which is what the
        // stack having created them means: a delete the owning service refused would leave the
        // stack DELETE_FAILED with the group still there.
        let (status, body) = call(
            &router,
            "DeleteStack",
            &[member("StackName", row.stack)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteStack must answer 200, got {status} {body}"
        );
        let deleted = settled_stack(&router, case, row.stack).await;
        assert!(
            deleted.is_empty() || xml_text(&deleted, "StackStatus").as_deref()
                == Some("DELETE_COMPLETE"),
            "case {case:?}: expected the delete to complete, got {deleted}"
        );
        let (status, refused) =
            ec2(&router, "DescribeSecurityGroups", &[("GroupId.1", &group_id)]).await;
        assert_eq!(
            (status, xml_text(&refused, "Code")),
            (400, Some(INVALID_GROUP_NOT_FOUND.to_string())),
            "case {case:?}: expected EC2 to refuse the deleted group {group_id:?}, got \
             {status} {refused}"
        );
    }
}
