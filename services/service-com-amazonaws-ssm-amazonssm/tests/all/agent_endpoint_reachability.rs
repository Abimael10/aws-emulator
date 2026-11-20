// SPDX-License-Identifier: CC0-1.0

//! Whether a managed node's AGENT CAN REACH this service — and so whether the node is
//! reported `Online` — driven end to end over each service's own wire, with Systems Manager,
//! Amazon EC2 and IAM on one engine ([`super::ec2_instance_managed_nodes`]).
//!
//! RULE under test: the agent registers, and goes on reporting itself, by CALLING Systems
//! Manager over HTTPS, so the instance's security groups have to let that call out ("make
//! sure that the security group … allows HTTPS (port 443) outbound traffic to the Systems
//! Manager endpoints"). An instance whose groups admit no such call never becomes a managed
//! node; a node that HAS registered whose groups stop admitting it reports `ConnectionLost`
//! and is still reported, and one whose groups admit it again reports `Online`.
//!
//! The rows differ along what the rule generalises over: which outbound rules the instance's
//! group carries (the all-traffic rule a new group is created with, a rule naming exactly the
//! agent's protocol and port, a range covering it, each bound of that range, a rule one port
//! away, a rule on another protocol, and no outbound rule at all) and whether the instance
//! runs in a group of its own or in the one its VPC gives it.
//!
//! Nothing here waits a fixed time: the registration span is a stored due instant, which a
//! case moves into the past through the store the way this crate's other clock-owed suites do
//! ([`Emulator::launched_ago`]), and every read LOOKS AGAIN until the condition holds or a
//! generous deadline passes.

use std::time::{Duration, Instant};

use serde_json::json;

use super::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, Grant, AL2023_NAME_PATTERN, REGION, SSM_CORE_POLICY_ARN,
};

/// How long a case looks for a node to reach a reported state, far longer than any span the
/// clock owes one, so the case does not depend on how fast the host runs.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long one look-again cycle pauses before reading again.
const POLL_PAUSE: Duration = Duration::from_millis(50);

/// The all-traffic outbound rule a security group is created with, as the fields of a
/// `RevokeSecurityGroupEgress` that takes it away: "a security group has an outbound rule
/// that allows all outbound traffic" (Amazon VPC User Guide), which on the wire is the `-1`
/// protocol over `0.0.0.0/0`.
const ALL_TRAFFIC_RULE: &[(&str, &str)] = &[
    ("IpPermissions.1.IpProtocol", "-1"),
    ("IpPermissions.1.IpRanges.1.CidrIp", "0.0.0.0/0"),
];

/// One outbound rule a case authorizes on its group, beside the protocol and ports it names.
#[derive(Debug, Clone, Copy)]
struct OutboundRule {
    /// The protocol the rule names, as `IpPermission$IpProtocol` carries it.
    protocol: &'static str,
    /// Its port range, for a protocol that carries one.
    ports: Option<(u16, u16)>,
}

/// One case: what the instance's group allows, and whether its agent reaches the service.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// What the case is.
    case: &'static str,
    /// Whether the all-traffic rule the group is created with is left in place.
    keeps_all_traffic: bool,
    /// The outbound rules the case authorizes on the group.
    authorizes: &'static [OutboundRule],
    /// Whether the node is expected to be reported `Online`.
    online: bool,
}

/// RULE: an instance registers as a managed node, and is reported `Online`, exactly when its
/// security groups let the agent's HTTPS call out.
#[tokio::test]
async fn a_node_is_online_only_where_its_groups_let_the_agent_s_call_out() {
    let rows: &[Row] = &[
        Row {
            case: "the all-traffic rule a new group carries",
            keeps_all_traffic: true,
            authorizes: &[],
            online: true,
        },
        Row {
            case: "a rule naming exactly HTTPS",
            keeps_all_traffic: false,
            authorizes: &[OutboundRule {
                protocol: "tcp",
                ports: Some((443, 443)),
            }],
            online: true,
        },
        Row {
            case: "a range that covers HTTPS",
            keeps_all_traffic: false,
            authorizes: &[OutboundRule {
                protocol: "tcp",
                ports: Some((400, 500)),
            }],
            online: true,
        },
        Row {
            case: "a range whose low bound is HTTPS",
            keeps_all_traffic: false,
            authorizes: &[OutboundRule {
                protocol: "tcp",
                ports: Some((443, 8443)),
            }],
            online: true,
        },
        Row {
            case: "a range whose high bound is HTTPS",
            keeps_all_traffic: false,
            authorizes: &[OutboundRule {
                protocol: "tcp",
                ports: Some((80, 443)),
            }],
            online: true,
        },
        Row {
            case: "a range ending one port below HTTPS",
            keeps_all_traffic: false,
            authorizes: &[OutboundRule {
                protocol: "tcp",
                ports: Some((80, 442)),
            }],
            online: false,
        },
        Row {
            case: "a range starting one port above HTTPS",
            keeps_all_traffic: false,
            authorizes: &[OutboundRule {
                protocol: "tcp",
                ports: Some((444, 8443)),
            }],
            online: false,
        },
        Row {
            case: "another protocol on the agent's port",
            keeps_all_traffic: false,
            authorizes: &[OutboundRule {
                protocol: "udp",
                ports: Some((443, 443)),
            }],
            online: false,
        },
        Row {
            case: "every protocol, so every port",
            keeps_all_traffic: false,
            authorizes: &[OutboundRule {
                protocol: "-1",
                ports: None,
            }],
            online: true,
        },
        Row {
            case: "two rules, the second of which names HTTPS",
            keeps_all_traffic: false,
            authorizes: &[
                OutboundRule {
                    protocol: "tcp",
                    ports: Some((80, 80)),
                },
                OutboundRule {
                    protocol: "tcp",
                    ports: Some((443, 443)),
                },
            ],
            online: true,
        },
        Row {
            case: "no outbound rule at all",
            keeps_all_traffic: false,
            authorizes: &[],
            online: false,
        },
    ];
    for row in rows {
        let emulator = Emulator::new();
        let image = emulator.image(row.case, AL2023_NAME_PATTERN).await;
        let profile = emulator
            .profile(row.case, "reach-role", &Grant::Attached(SSM_CORE_POLICY_ARN))
            .await;
        let group = group_allowing(&emulator, row).await;
        let instance = launch_in_group(&emulator, row.case, &image, &profile, &group).await;
        emulator.launched_ago(&instance, launched_long_enough_ago());

        let reported = ping_status(&emulator, row.case, &instance, row.online).await;
        if row.online {
            assert_eq!(
                reported.as_deref(),
                Some("Online"),
                "case {:?}: expected the node reported Online, got {reported:?}",
                row.case
            );
        } else {
            assert_eq!(
                reported, None,
                "case {:?}: expected no managed node at all, got {reported:?}",
                row.case
            );
        }
    }
}

/// RULE: the ping status of a node that HAS registered follows whether the agent can still
/// reach the service — so taking the agent's outbound rule away leaves the node reported with
/// its connection lost rather than reported `Online`, and giving it back leaves it `Online`.
#[tokio::test]
async fn a_registered_node_loses_its_connection_when_its_group_stops_letting_the_agent_out() {
    let case = "the group that stops allowing HTTPS";
    let emulator = Emulator::new();
    let image = emulator.image(case, AL2023_NAME_PATTERN).await;
    let profile = emulator
        .profile(case, "blocked-role", &Grant::Attached(SSM_CORE_POLICY_ARN))
        .await;
    let group = group_allowing(
        &emulator,
        &Row {
            case,
            keeps_all_traffic: true,
            authorizes: &[],
            online: true,
        },
    )
    .await;
    let instance = launch_in_group(&emulator, case, &image, &profile, &group).await;
    emulator.launched_ago(&instance, launched_long_enough_ago());
    let registered = ping_status(&emulator, case, &instance, true).await;
    assert_eq!(
        registered.as_deref(),
        Some("Online"),
        "case {case:?}: the node must register before its group is narrowed, got {registered:?}"
    );

    // The one change: the rule that let the agent's HTTPS call out is taken away.
    emulator
        .ec2(
            case,
            "RevokeSecurityGroupEgress",
            &with_group(&group, ALL_TRAFFIC_RULE),
        )
        .await;
    let blocked = look_until(
        &emulator,
        case,
        &instance,
        |status| status.as_deref() != Some("Online"),
        "the node to stop being reported Online",
    )
    .await;
    assert_eq!(
        blocked.as_deref(),
        Some("ConnectionLost"),
        "case {case:?}: expected the registered node reported ConnectionLost, got {blocked:?}"
    );

    // And back: a group that lets the call out again leaves the agent reporting.
    let restored: Vec<(String, String)> = vec![
        ("GroupId".to_string(), group.clone()),
        ("IpPermissions.1.IpProtocol".to_string(), "tcp".to_string()),
        ("IpPermissions.1.FromPort".to_string(), "443".to_string()),
        ("IpPermissions.1.ToPort".to_string(), "443".to_string()),
        (
            "IpPermissions.1.IpRanges.1.CidrIp".to_string(),
            "0.0.0.0/0".to_string(),
        ),
    ];
    emulator
        .ec2(case, "AuthorizeSecurityGroupEgress", &borrowed(&restored))
        .await;
    let again = look_until(
        &emulator,
        case,
        &instance,
        |status| status.as_deref() == Some("Online"),
        "the node to be reported Online again",
    )
    .await;
    assert_eq!(
        again.as_deref(),
        Some("Online"),
        "case {case:?}: expected the node reported Online again, got {again:?}"
    );
}

/// The security group one row's instance runs in: created the way a client creates one — so
/// it carries the all-traffic outbound rule — with that rule revoked where the row says so
/// and the row's own rules authorized on it. Answers the group id EC2 minted.
async fn group_allowing(emulator: &Emulator, row: &Row) -> String {
    let name = format!(
        "agent-reach-{}",
        row.case
            .chars()
            .map(|character| if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            })
            .collect::<String>()
    );
    let created = emulator
        .ec2(
            row.case,
            "CreateSecurityGroup",
            &[
                ("GroupName", name.as_str()),
                ("GroupDescription", "the agent own group"),
            ],
        )
        .await;
    let group = Emulator::answered(&created, "groupId").unwrap_or_else(|| {
        panic!(
            "case {:?}: CreateSecurityGroup must mint a group: {created}",
            row.case
        )
    });
    if !row.keeps_all_traffic {
        emulator
            .ec2(
                row.case,
                "RevokeSecurityGroupEgress",
                &with_group(&group, ALL_TRAFFIC_RULE),
            )
            .await;
    }
    for rule in row.authorizes {
        let mut fields: Vec<(String, String)> = vec![("GroupId".to_string(), group.clone())];
        fields.push((
            "IpPermissions.1.IpProtocol".to_string(),
            rule.protocol.to_string(),
        ));
        if let Some((from, to)) = rule.ports {
            fields.push(("IpPermissions.1.FromPort".to_string(), from.to_string()));
            fields.push(("IpPermissions.1.ToPort".to_string(), to.to_string()));
        }
        fields.push((
            "IpPermissions.1.IpRanges.1.CidrIp".to_string(),
            "0.0.0.0/0".to_string(),
        ));
        emulator
            .ec2(row.case, "AuthorizeSecurityGroupEgress", &borrowed(&fields))
            .await;
    }
    group
}

/// `fields` with the group the request is about in front of them.
fn with_group<'a>(group: &'a str, fields: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut params = vec![("GroupId", group)];
    params.extend(fields.iter().copied());
    params
}

/// Owned form fields as the borrowed pairs the harness takes.
fn borrowed(fields: &[(String, String)]) -> Vec<(&str, &str)> {
    fields
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// Launch one instance running as `profile` in `group`, as the instance id EC2 minted.
async fn launch_in_group(
    emulator: &Emulator,
    case: &str,
    image_id: &str,
    profile: &str,
    group: &str,
) -> String {
    let body = emulator
        .ec2(
            case,
            "RunInstances",
            &[
                ("ImageId", image_id),
                ("InstanceType", "t3.micro"),
                ("MinCount", "1"),
                ("MaxCount", "1"),
                ("IamInstanceProfile.Name", profile),
                ("SecurityGroupId.1", group),
            ],
        )
        .await;
    Emulator::answered(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances must mint an instance: {body}"))
}

/// The `PingStatus` this service reports for `instance_id`, or `None` when it reports no node
/// for it at all.
async fn reported_status(emulator: &Emulator, case: &str, instance_id: &str) -> Option<String> {
    let (status, answered) = emulator
        .ssm(
            REGION,
            "DescribeInstanceInformation",
            &json!({"Filters": [{"Key": "InstanceIds", "Values": [instance_id]}]}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstanceInformation must be served, got {status}: {answered}"
    );
    answered["InstanceInformationList"]
        .as_array()
        .and_then(|reported| reported.first())
        .and_then(|node| node["PingStatus"].as_str())
        .map(str::to_string)
}

/// The status a row ends up reporting.
///
/// A row that expects the node ONLINE looks again until it is, because the registration is
/// owed by the clock. A row that expects no node reads once per cycle until the deadline and
/// answers the last reading: the instance's own registration instant is already in the past,
/// so a node that was going to appear has had every chance to.
async fn ping_status(
    emulator: &Emulator,
    case: &str,
    instance_id: &str,
    expect_online: bool,
) -> Option<String> {
    if expect_online {
        return look_until(
            emulator,
            case,
            instance_id,
            |status| status.as_deref() == Some("Online"),
            "the node to be reported Online",
        )
        .await;
    }
    // Read it twice with a pause between, so a registration that would have fallen due has
    // been materialised by a read before the absence is asserted.
    let first = reported_status(emulator, case, instance_id).await;
    tokio::time::sleep(POLL_PAUSE).await;
    let second = reported_status(emulator, case, instance_id).await;
    assert_eq!(
        first, second,
        "case {case:?}: two reads of an unregistered instance must agree"
    );
    second
}

/// Look at the node's reported ping status until `settled` holds, or [`DEADLINE`] passes.
async fn look_until(
    emulator: &Emulator,
    case: &str,
    instance_id: &str,
    settled: impl Fn(&Option<String>) -> bool,
    waiting_for: &str,
) -> Option<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = reported_status(emulator, case, instance_id).await;
        if settled(&reported) {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {waiting_for} with nothing but the clock running, \
             still {reported:?}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}
