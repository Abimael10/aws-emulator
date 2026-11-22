// SPDX-License-Identifier: CC0-1.0

//! A handler that changes a SECURITY GROUP's own rules through the AWS SDK its managed runtime
//! supplies.
//!
//! RULE: the AWS SDK a managed runtime puts on a handler's import path reaches every service
//! this emulator serves, Amazon EC2 among them — so a handler's `AuthorizeSecurityGroup*` and
//! `RevokeSecurityGroup*` call is really MADE and really served: the permission it authorizes is
//! one the group EC2 itself then reports, the permission it revokes is one EC2 no longer
//! reports, and every other rule of the group is left exactly as it was. That holds whichever
//! SDK the handler calls (the JavaScript runtime's `@aws-sdk/client-ec2`, whose
//! `<Operation>Command` classes are the service's own operations, and Python's
//! `boto3.client("ec2")`), whichever direction the rule is in, and whatever the rule names as
//! its source or destination.
//!
//! That is what a provider function of a CloudFormation custom resource over a VPC does for a
//! living: its `Create` revokes the rules the VPC's default group came with and its `Delete`
//! authorizes them again. A runtime whose SDK cannot reach this service answers such a request
//! with a failure instead, and the stack that declared the resource rolls back.
//!
//! Every group here is MINTED by Amazon EC2 itself through its own `CreateSecurityGroup`, every
//! starting rule is put on it through that service's own API, and what the group holds afterwards
//! is read back through its own `DescribeSecurityGroups` — so a handler that answered without
//! making the call fails here. The two services run on ONE shared engine, exactly as the router
//! mounts them.
//!
//! The rows differ along what the rule generalises over: the runtime and SDK the handler calls,
//! whether the call AUTHORIZES or REVOKES, the direction of the rule (inbound, outbound), what
//! the rule names (an IPv4 range, an IPv6 range, another security group, several ranges at once)
//! and what it allows (every protocol, one TCP port, one UDP port).

use std::collections::BTreeSet;
use std::io::Write as _;
use std::time::Duration;

use aws_sdk_ec2::types::{IpPermission, IpRange, Ipv6Range, UserIdGroupPair};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, zip_file, ACCOUNT, REGION, ROLE,
};
use crate::vpc_attachment_interfaces::{emulator, Emulator};

/// Every IPv4 address and every IPv6 address, as a CIDR block: what the rule a VPC's default
/// group comes with allows outbound traffic to.
const EVERY_IPV4: &str = "0.0.0.0/0";
const EVERY_IPV6: &str = "::/0";

/// "Use `-1` to specify all protocols" (`IpPermission$IpProtocol`), which is the protocol of
/// every rule a default group carries.
const EVERY_PROTOCOL: &str = "-1";

/// How long one invocation of these handlers is given, as the function's own `Timeout`: well
/// past what a run needs, because what is under test is the call the handler makes.
const TIMEOUT: u64 = 60;

/// How long this suite waits for a function to report `Active`, and how often it looks again.
/// The transition is the CLOCK's and this suite makes it due rather than waiting for it, so the
/// deadline is only the guard against a host that never got there.
const ACTIVE_BUDGET: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// Which SDK the handler calls, which is also which runtime it is written for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sdk {
    /// The JavaScript runtime's bundled `@aws-sdk/client-ec2`.
    JavaScript,
    /// Python's `boto3.client("ec2")`.
    Python,
}

impl Sdk {
    /// The managed runtime this SDK is supplied by.
    fn runtime(self) -> &'static str {
        match self {
            Self::JavaScript => "nodejs22.x",
            Self::Python => "python3.12",
        }
    }

    /// The `Handler` the function declares, and so the module the package carries its source as.
    fn handler(self) -> &'static str {
        match self {
            Self::JavaScript => "index.handler",
            Self::Python => "index.handler",
        }
    }

    /// The one file of the deployment package.
    fn source_file(self) -> &'static str {
        match self {
            Self::JavaScript => "index.js",
            Self::Python => "index.py",
        }
    }

    /// The handler's own source.
    fn source(self) -> &'static str {
        match self {
            Self::JavaScript => JAVASCRIPT_HANDLER,
            Self::Python => PYTHON_HANDLER,
        }
    }
}

/// The JavaScript handler: the client package's own command classes, one per operation of the
/// service, named by the call the event asks for.
///
/// The destructuring `require` is the first half of the point: a package that exported its client
/// alone would bind `undefined` here, and the command the handler constructs would be no
/// constructor at all.
const JAVASCRIPT_HANDLER: &str = r#"
'use strict';

const {
    EC2Client,
    AuthorizeSecurityGroupIngressCommand,
    AuthorizeSecurityGroupEgressCommand,
    RevokeSecurityGroupIngressCommand,
    RevokeSecurityGroupEgressCommand,
} = require('@aws-sdk/client-ec2');

const COMMANDS = {
    AuthorizeIngress: AuthorizeSecurityGroupIngressCommand,
    AuthorizeEgress: AuthorizeSecurityGroupEgressCommand,
    RevokeIngress: RevokeSecurityGroupIngressCommand,
    RevokeEgress: RevokeSecurityGroupEgressCommand,
};

exports.handler = async (event) => {
    const ec2 = new EC2Client({ region: event.region });
    const command = COMMANDS[event.action + event.direction];
    const answered = await ec2.send(new command({
        GroupId: event.groupId,
        IpPermissions: event.permissions,
    }));
    const rules = answered.SecurityGroupRules || answered.RevokedSecurityGroupRules || [];
    return {
        carriedOut: answered.Return === true,
        rules: rules.length,
        command: command.name,
    };
};
"#;

/// The Python handler: the same calls through the SDK that runtime supplies, named by the
/// method each operation is bound to.
const PYTHON_HANDLER: &str = r#"
import boto3

METHODS = {
    "AuthorizeIngress": "authorize_security_group_ingress",
    "AuthorizeEgress": "authorize_security_group_egress",
    "RevokeIngress": "revoke_security_group_ingress",
    "RevokeEgress": "revoke_security_group_egress",
}


def handler(event, context):
    ec2 = boto3.client("ec2", region_name=event["region"])
    method = METHODS[event["action"] + event["direction"]]
    answered = getattr(ec2, method)(
        GroupId=event["groupId"], IpPermissions=event["permissions"]
    )
    rules = answered.get("SecurityGroupRules") or answered.get(
        "RevokedSecurityGroupRules"
    ) or []
    return {
        "carriedOut": answered.get("Return") is True,
        "rules": len(rules),
        "command": method,
    }
"#;

/// One row: the call one handler makes, and what the group must hold once it has.
struct Row {
    case: &'static str,
    /// The SDK, and so the runtime, the handler is written for.
    sdk: Sdk,
    /// `Authorize` or `Revoke` — which of the two the handler calls.
    action: &'static str,
    /// `Ingress` or `Egress` — the direction of the rule the call acts on.
    direction: &'static str,
    /// The rules put on the group through EC2's own API before the handler runs, each with the
    /// direction it goes in. Built from the group's own minted id, which is what a rule naming
    /// the group itself carries.
    given: fn(&str) -> Vec<(&'static str, Value)>,
    /// The `IpPermissions` the handler's own call carries.
    acts_on: fn(&str) -> Vec<Value>,
    /// The inbound and outbound rules the group must hold afterwards, as [`fingerprints`]
    /// renders them.
    expected_ingress: fn(&str) -> Vec<String>,
    expected_egress: fn(&str) -> Vec<String>,
}

/// An `IpPermission` document naming one IPv4 range.
fn ipv4(protocol: &str, ports: Option<(i64, i64)>, cidrs: &[&str]) -> Value {
    let mut permission = json!({
        "IpProtocol": protocol,
        "IpRanges": cidrs.iter().map(|cidr| json!({"CidrIp": cidr})).collect::<Vec<Value>>(),
    });
    if let Some((from, to)) = ports {
        permission["FromPort"] = json!(from);
        permission["ToPort"] = json!(to);
    }
    permission
}

/// The same for one IPv6 range.
fn ipv6(protocol: &str, ports: Option<(i64, i64)>, cidr: &str) -> Value {
    let mut permission = json!({
        "IpProtocol": protocol,
        "Ipv6Ranges": [{"CidrIpv6": cidr}],
    });
    if let Some((from, to)) = ports {
        permission["FromPort"] = json!(from);
        permission["ToPort"] = json!(to);
    }
    permission
}

/// The same for a rule naming ANOTHER security group — here the group itself, which is the rule
/// a VPC's default group comes with inbound.
fn group_pair(protocol: &str, group_id: &str) -> Value {
    json!({
        "IpProtocol": protocol,
        "UserIdGroupPairs": [{"GroupId": group_id, "UserId": ACCOUNT}],
    })
}

/// The fingerprint of one rule: what it allows and the one source or destination it names, so a
/// rule can be compared without reading the answer being checked for the comparison.
fn fingerprint(protocol: &str, ports: Option<(i64, i64)>, source: &str) -> String {
    match ports {
        Some((from, to)) => format!("{protocol}:{from}-{to} {source}"),
        None => format!("{protocol} {source}"),
    }
}

fn rows() -> Vec<Row> {
    vec![
        // The call a CDK custom resource's provider makes on its `Create`: the outbound rule
        // the group came with, revoked.
        Row {
            case: "the JavaScript SDK revoking the outbound rule a group comes with",
            sdk: Sdk::JavaScript,
            action: "Revoke",
            direction: "Egress",
            given: |_| Vec::new(),
            acts_on: |_| vec![ipv4(EVERY_PROTOCOL, None, &[EVERY_IPV4])],
            expected_ingress: |_| Vec::new(),
            expected_egress: |_| Vec::new(),
        },
        // ...and the one it makes on its `Delete`: the same rule, authorized again.
        Row {
            case: "the JavaScript SDK authorizing an inbound rule naming the group itself",
            sdk: Sdk::JavaScript,
            action: "Authorize",
            direction: "Ingress",
            given: |_| Vec::new(),
            acts_on: |group| vec![group_pair(EVERY_PROTOCOL, group)],
            expected_ingress: |group| {
                vec![fingerprint(
                    EVERY_PROTOCOL,
                    None,
                    &format!("group:{group}"),
                )]
            },
            expected_egress: |_| vec![fingerprint(EVERY_PROTOCOL, None, EVERY_IPV4)],
        },
        // The second call that provider's `Create` makes: the self-referencing inbound rule,
        // revoked, with the outbound rule the group came with left alone.
        Row {
            case: "the JavaScript SDK revoking an inbound rule naming the group itself",
            sdk: Sdk::JavaScript,
            action: "Revoke",
            direction: "Ingress",
            given: |group| vec![("Ingress", group_pair(EVERY_PROTOCOL, group))],
            acts_on: |group| vec![group_pair(EVERY_PROTOCOL, group)],
            expected_ingress: |_| Vec::new(),
            expected_egress: |_| vec![fingerprint(EVERY_PROTOCOL, None, EVERY_IPV4)],
        },
        // One TCP port to one IPv4 range, outbound, through the other SDK: the rule is added
        // beside the one the group came with rather than replacing it.
        Row {
            case: "the Python SDK authorizing one TCP port outbound to an IPv4 range",
            sdk: Sdk::Python,
            action: "Authorize",
            direction: "Egress",
            given: |_| Vec::new(),
            acts_on: |_| vec![ipv4("tcp", Some((443, 443)), &["10.0.0.0/8"])],
            expected_ingress: |_| Vec::new(),
            expected_egress: |_| {
                vec![
                    fingerprint(EVERY_PROTOCOL, None, EVERY_IPV4),
                    fingerprint("tcp", Some((443, 443)), "10.0.0.0/8"),
                ]
            },
        },
        // A revocation removes the rule it names and nothing else: the IPv6 rule the group was
        // given stays, the all-protocols IPv4 rule it came with goes.
        Row {
            case: "the Python SDK revoking one outbound rule and leaving an IPv6 rule",
            sdk: Sdk::Python,
            action: "Revoke",
            direction: "Egress",
            given: |_| vec![("Egress", ipv6("tcp", Some((443, 443)), EVERY_IPV6))],
            acts_on: |_| vec![ipv4(EVERY_PROTOCOL, None, &[EVERY_IPV4])],
            expected_ingress: |_| Vec::new(),
            expected_egress: |_| vec![fingerprint("tcp", Some((443, 443)), EVERY_IPV6)],
        },
        // SEVERAL ranges in one permission, inbound: each becomes a rule of the group.
        Row {
            case: "the Python SDK authorizing one UDP port inbound from two IPv4 ranges",
            sdk: Sdk::Python,
            action: "Authorize",
            direction: "Ingress",
            given: |_| Vec::new(),
            acts_on: |_| {
                vec![ipv4(
                    "udp",
                    Some((53, 53)),
                    &["10.1.0.0/16", "10.2.0.0/16"],
                )]
            },
            expected_ingress: |_| {
                vec![
                    fingerprint("udp", Some((53, 53)), "10.1.0.0/16"),
                    fingerprint("udp", Some((53, 53)), "10.2.0.0/16"),
                ]
            },
            expected_egress: |_| vec![fingerprint(EVERY_PROTOCOL, None, EVERY_IPV4)],
        },
    ]
}

/// A .zip deployment package holding `source` as the one entry `file` names.
fn package(file: &str, source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer
            .start_file(file, options)
            .expect("the archive entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the archive entry's bytes are written");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Create the function one row's handler runs in, and make the activation the clock owes due so
/// the invocation below reaches it.
async fn given_function(emulator: &Emulator, case: &str, name: &str, sdk: Sdk) {
    let created = create_function(
        &emulator.router,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": sdk.runtime(),
            "Handler": sdk.handler(),
            "Timeout": TIMEOUT,
            "Code": {"ZipFile": zip_file(&package(sdk.source_file(), sdk.source()))},
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected HTTP 201 from CreateFunction, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    make_activation_due(&emulator.engine);
    let deadline = std::time::Instant::now() + ACTIVE_BUDGET;
    loop {
        let read = json_call(
            &emulator.router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &Value::Null,
        )
        .await;
        let reported: Value = serde_json::from_slice(&read.body).unwrap_or(Value::Null);
        if reported["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the function {name:?} never became Active, got {reported}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `Invoke` the function with `event`, answering what the run produced and the function-error
/// signal the answer carried — which an invocation that ran to completion carries none of.
async fn invoked(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    event: &Value,
) -> (Option<String>, Value) {
    let answer = json_call(
        router,
        "POST",
        &format!("/2015-03-31/functions/{name}/invocations"),
        event,
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    let signal = answer
        .headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let payload: Value = serde_json::from_slice(&answer.body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&answer.body).to_string()));
    (signal, payload)
}

/// One security group of its own VPC, minted by Amazon EC2 itself, and the id it was given.
async fn given_group(ec2: &aws_sdk_ec2::Client, case: &str, prefix: u8) -> String {
    let created = ec2
        .create_vpc()
        .cidr_block(format!("10.{prefix}.0.0/16"))
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: EC2 must create the VPC: {error:?}"));
    let vpc_id = created
        .vpc()
        .and_then(aws_sdk_ec2::types::Vpc::vpc_id)
        .unwrap_or_else(|| panic!("case {case:?}: CreateVpc must report the VPC it minted"))
        .to_string();
    let created = ec2
        .create_security_group()
        .vpc_id(&vpc_id)
        .group_name(format!("rule-probe-{prefix}"))
        .description("the group a handlers own call changes the rules of")
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: EC2 must create the group: {error:?}"));
    created
        .group_id()
        .unwrap_or_else(|| panic!("case {case:?}: CreateSecurityGroup must report the group"))
        .to_string()
}

/// The `IpPermission` the SDK's own type holds for `document`, which is how a row's starting
/// rules are put on the group through EC2's own API.
fn permission_of(document: &Value) -> IpPermission {
    let mut permission = IpPermission::builder()
        .set_ip_protocol(document["IpProtocol"].as_str().map(str::to_string))
        .set_from_port(document["FromPort"].as_i64().and_then(|port| i32::try_from(port).ok()))
        .set_to_port(document["ToPort"].as_i64().and_then(|port| i32::try_from(port).ok()));
    for range in document["IpRanges"].as_array().unwrap_or(&Vec::new()) {
        permission = permission.ip_ranges(
            IpRange::builder()
                .set_cidr_ip(range["CidrIp"].as_str().map(str::to_string))
                .build(),
        );
    }
    for range in document["Ipv6Ranges"].as_array().unwrap_or(&Vec::new()) {
        permission = permission.ipv6_ranges(
            Ipv6Range::builder()
                .set_cidr_ipv6(range["CidrIpv6"].as_str().map(str::to_string))
                .build(),
        );
    }
    for pair in document["UserIdGroupPairs"].as_array().unwrap_or(&Vec::new()) {
        permission = permission.user_id_group_pairs(
            UserIdGroupPair::builder()
                .set_group_id(pair["GroupId"].as_str().map(str::to_string))
                .set_user_id(pair["UserId"].as_str().map(str::to_string))
                .build(),
        );
    }
    permission.build()
}

/// Put one row's starting rules on the group through Amazon EC2's own API.
async fn given_rules(
    ec2: &aws_sdk_ec2::Client,
    case: &str,
    group_id: &str,
    rules: &[(&str, Value)],
) {
    for (direction, document) in rules {
        let permission = permission_of(document);
        match *direction {
            "Ingress" => {
                ec2.authorize_security_group_ingress()
                    .group_id(group_id)
                    .ip_permissions(permission)
                    .send()
                    .await
                    .map(|_| ())
                    .unwrap_or_else(|error| {
                        panic!("case {case:?}: EC2 must authorize the starting rule: {error:?}")
                    });
            }
            _ => {
                ec2.authorize_security_group_egress()
                    .group_id(group_id)
                    .ip_permissions(permission)
                    .send()
                    .await
                    .map(|_| ())
                    .unwrap_or_else(|error| {
                        panic!("case {case:?}: EC2 must authorize the starting rule: {error:?}")
                    });
            }
        }
    }
}

/// What the group holds right now, read through Amazon EC2's own `DescribeSecurityGroups`: the
/// inbound and the outbound rules, each rendered as one fingerprint per source or destination it
/// names.
async fn held_rules(
    ec2: &aws_sdk_ec2::Client,
    case: &str,
    group_id: &str,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let described = ec2
        .describe_security_groups()
        .group_ids(group_id)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: EC2 must report the group {group_id}: {error:?}")
        });
    let group = described
        .security_groups()
        .iter()
        .find(|group| group.group_id() == Some(group_id))
        .unwrap_or_else(|| panic!("case {case:?}: EC2 must report the group {group_id}"))
        .clone();
    (
        fingerprints(group.ip_permissions()),
        fingerprints(group.ip_permissions_egress()),
    )
}

/// Every rule `permissions` holds, as one fingerprint per source or destination named.
fn fingerprints(permissions: &[IpPermission]) -> BTreeSet<String> {
    let mut held = BTreeSet::new();
    for permission in permissions {
        let protocol = permission.ip_protocol().unwrap_or_default();
        let ports = permission
            .from_port()
            .zip(permission.to_port())
            .map(|(from, to)| (i64::from(from), i64::from(to)));
        for range in permission.ip_ranges() {
            held.insert(fingerprint(
                protocol,
                ports,
                range.cidr_ip().unwrap_or_default(),
            ));
        }
        for range in permission.ipv6_ranges() {
            held.insert(fingerprint(
                protocol,
                ports,
                range.cidr_ipv6().unwrap_or_default(),
            ));
        }
        for pair in permission.user_id_group_pairs() {
            held.insert(fingerprint(
                protocol,
                ports,
                &format!("group:{}", pair.group_id().unwrap_or_default()),
            ));
        }
    }
    held
}

#[tokio::test]
async fn a_handlers_own_sdk_call_changes_the_security_groups_rules() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let emulator = emulator();
        let prefix = u8::try_from(index + 20).expect("the row count fits a CIDR label");
        let group_id = given_group(&emulator.ec2, case, prefix).await;
        given_rules(&emulator.ec2, case, &group_id, &(row.given)(&group_id)).await;

        let name = format!("rule-writer-{index}");
        given_function(&emulator, case, &name, row.sdk).await;
        let event = json!({
            "region": REGION,
            "groupId": group_id,
            "action": row.action,
            "direction": row.direction,
            "permissions": (row.acts_on)(&group_id),
        });
        let (signal, payload) = invoked(&emulator.router, case, &name, &event).await;
        assert_eq!(
            signal, None,
            "case {case:?}: expected the handler's call to go through, got the error document \
             {payload}"
        );
        assert_eq!(
            payload["carriedOut"],
            json!(true),
            "case {case:?}: expected Amazon EC2 to report the request carried out, got {payload}"
        );

        // What the GROUP holds, read through EC2's own API: the call the handler reported making
        // is a call that service really served.
        let (ingress, egress) = held_rules(&emulator.ec2, case, &group_id).await;
        let expected_ingress: BTreeSet<String> =
            (row.expected_ingress)(&group_id).into_iter().collect();
        let expected_egress: BTreeSet<String> =
            (row.expected_egress)(&group_id).into_iter().collect();
        assert_eq!(
            ingress, expected_ingress,
            "case {case:?}: expected the group's inbound rules to be {expected_ingress:?}, got \
             {ingress:?}"
        );
        assert_eq!(
            egress, expected_egress,
            "case {case:?}: expected the group's outbound rules to be {expected_egress:?}, got \
             {egress:?}"
        );
    }
}
