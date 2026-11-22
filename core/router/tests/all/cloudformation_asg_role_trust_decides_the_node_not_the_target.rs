// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that puts TWO launch-template Auto Scaling groups behind ONE Elastic
//! Load Balancing v2 target group, where the two groups differ in NOTHING but the trust policy
//! of the IAM role their instances run as: the stack converges to a state a client can observe,
//! and what the trust policy decides is the Systems Manager MANAGED NODE — never the group's own
//! convergence.
//!
//! RULE under test, in two halves a client reads through four different services:
//!
//! * a stack's resources are created through the owning services' own operations, so a stack
//!   that reaches a terminal `CREATE_COMPLETE` publishes outputs that resolve there, and a group
//!   it declared maintains its `DesiredCapacity` (`CreateAutoScalingGroup$TargetGroupARNs`: "the
//!   Amazon Resource Names (ARN) of the target groups to associate with the Auto Scaling group.
//!   Instances are registered as targets in a target group, and traffic is routed to the target
//!   group"), drives each launched instance to the `InService` lifecycle state and registers it
//!   in every attached target group, whose own health checks then settle each registration's
//!   `TargetHealth$State`. None of that is derived from the IAM role the instances run as: the
//!   registration is the GROUP's, made for an instance of the group;
//! * an instance becomes a managed node of Systems Manager only where the role its instance
//!   profile carries is one Amazon EC2 may act as — "the IAM role … must trust the Amazon EC2
//!   service to assume it", which is what lets an agent inside the machine obtain credentials at
//!   all — and `DescribeInstanceInformation` then reports that node, under the NAME of the role
//!   whose credentials its agent registered with (`InstanceInformation$IamRole`). A role whose
//!   trust policy withholds that assume leaves the instance unmanaged, and absent from every
//!   page of that report.
//!
//! So the two groups of one stack are the contrast: both converge, both are registered, and only
//! one of them has a node. A suite that pinned the convergence alone could not tell a stack that
//! converges from one whose instances happen to be managed, and a suite that pinned the node
//! alone could not tell "no node" from "nothing launched".
//!
//! Six crates have to agree for that to hold, which is why this suite lives here: CloudFormation
//! makes every create, IAM holds the two roles and their instance profiles, Amazon EC2 holds the
//! VPC, the security group, the launch templates and the instances, Amazon EC2 Auto Scaling
//! launches into the groups and attaches the target group, Elastic Load Balancing v2 holds the
//! target group, the load balancer and the health verdict, and Systems Manager decides the
//! registration. Every assertion reads what one of those services persisted, through the
//! operation a client polls. Assembling the whole router costs stack (see `boot_serve.rs`), so it
//! is built — and the requests driven — on a sized thread.
//!
//! The rows differ along what the rule generalises over: the FORM of each role's trust document,
//! as IAM reads one (`Principal.Service` and `Action` each one string or a list of them, a
//! wildcard covering the action, an explicit `Deny` winning over an `Allow` of the same assume,
//! and a document that parses but carries no statement at all), how many instances each group is
//! asked for, and the cadence and threshold the shared target group is checked at. The row whose
//! withholding role simply names another service is the recorded one; the others are the same
//! withholding written differently.
//!
//! Nothing here waits a fixed time: every span crossed — the stack's own creates, the instances'
//! move to `running`, their registration as targets, the checks that settle a verdict, and the
//! managed-node registration — is a stored due instant, so a case LOOKS AGAIN until the
//! condition holds or [`DEADLINE`] passes, and no assertion is made about elapsed time.
//!
//! What makes the ABSENCE of a node an answer rather than a race is the stack's own ordering:
//! the withholding half is declared first and the admitting half made to depend on it, so the
//! withholding group — and therefore its launch — precedes the admitting one's. A registration
//! falls due a span after the instance was launched, whatever that span is, so by the time the
//! LATER instance's node has been observed, the registration the EARLIER one would have been
//! owed has fallen due too. Nothing here reads or encodes that span.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The form encoder and the XML readers an awsQuery suite needs live ONCE in the test crate, in
// `query_wire`: "Nothing new should write another one — call this module."
use crate::query_wire::{block_of, elements, form_encode, members_of, outputs_of};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request below is signed for, and so the Region every resource belongs to.
const REGION: &str = "us-east-1";

/// The API versions each awsQuery/ec2Query request below carries.
const CFN_VERSION: &str = "2010-05-15";
const ASG_VERSION: &str = "2011-01-01";
const ELB_VERSION: &str = "2015-12-01";
const EC2_VERSION: &str = "2016-11-15";

/// The public Systems Manager parameter a client resolves the Amazon Linux 2023 image id
/// through, which is the image every launch template here launches from — the platform whose
/// "SSM Agent is preinstalled".
const AL2023_PARAMETER: &str =
    "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64";

/// The instance type every launch template here names.
const INSTANCE_TYPE: &str = "t3.micro";

/// The AWS-managed policy a client attaches to an instance profile's role so an agent on the
/// instance may register its node. BOTH roles of every row carry it, so the only difference
/// between them is the trust document.
const SSM_CORE_POLICY: &str = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore";

/// The service principal a role has to admit for Amazon EC2 to act as it on an instance's
/// behalf.
const EC2_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The stack statuses a create is still in flight in, and the one it must finish in.
const IN_PROGRESS_SUFFIX: &str = "_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `Instance$LifecycleState` of an instance a group has finished putting into service.
const IN_SERVICE: &str = "InService";

/// The `TargetHealth$State` a registration the load balancer's checks reach settles on.
const HEALTHY: &str = "healthy";

/// The `InstanceState$Name` Amazon EC2 reports for an instance that came up.
const RUNNING: &str = "running";

/// The `PingStatus` of a node whose agent is responding, and the `ResourceType` of a node that
/// is an Amazon EC2 instance.
const ONLINE: &str = "Online";
const RESOURCE_TYPE_EC2: &str = "EC2Instance";

/// How long a row waits for the clock-owed moves it depends on, and how often it looks again.
///
/// Generous on purpose: every span is a stored due instant and nothing sleeps for one, so a row
/// looks again until its condition holds. All this bound is for is that a move which never comes
/// fails instead of hanging; nothing asserts on elapsed time.
const DEADLINE: Duration = Duration::from_secs(240);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The emulator one row drives: the whole router over one engine, and the runtime its requests
/// are blocked on.
struct Emulator {
    router: MultiServiceRouter,
    runtime: tokio::runtime::Runtime,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let mut router = build_router(engine);
        // Signatures are not what this suite is about; the router still reads the SigV4
        // credential scope, which is where each call's Region comes from.
        router.set_request_authentication(false);
        Self {
            router,
            // Timers enabled, as the emulator's own boot path builds its runtime
            // (`core/router/src/main.rs`): the services this suite drives own clock-owed work
            // that wakes on a timer.
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("a current-thread runtime"),
        }
    }

    /// One request at `signing_name`'s endpoint, as `(status, body)`.
    fn send(&self, signing_name: &str, headers: &[(&str, &str)], body: Vec<u8>) -> (u16, String) {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let mut builder = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", &host)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                     aws4_request, SignedHeaders=host, Signature=deadbeef"
                ),
            );
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(bytes::Bytes::from(body))
            .expect("a well-formed request");
        let response = self.runtime.block_on(self.router.dispatch(request));
        let status = response.status().as_u16();
        (
            status,
            String::from_utf8_lossy(&response.into_body()).into_owned(),
        )
    }

    /// One form-encoded action of a query-protocol service that must be served, as its XML body.
    fn query(
        &self,
        case: &str,
        signing_name: &str,
        version: &str,
        action: &str,
        members: &[(&str, &str)],
    ) -> String {
        let mut body = format!("Action={action}&Version={version}");
        for (key, value) in members {
            body.push('&');
            body.push_str(&form_encode(key));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let (status, text) = self.send(
            signing_name,
            &[("content-type", "application/x-www-form-urlencoded")],
            body.into_bytes(),
        );
        assert_eq!(
            status, 200,
            "case {case:?}: {action} must be served, got {status} with body {text}"
        );
        text
    }

    fn cloudformation(&self, case: &str, action: &str, members: &[(&str, &str)]) -> String {
        self.query(case, "cloudformation", CFN_VERSION, action, members)
    }

    fn autoscaling(&self, case: &str, action: &str, members: &[(&str, &str)]) -> String {
        self.query(case, "autoscaling", ASG_VERSION, action, members)
    }

    fn elbv2(&self, case: &str, action: &str, members: &[(&str, &str)]) -> String {
        self.query(case, "elasticloadbalancing", ELB_VERSION, action, members)
    }

    fn ec2(&self, case: &str, action: &str, members: &[(&str, &str)]) -> String {
        self.query(case, "ec2", EC2_VERSION, action, members)
    }

    /// One Systems Manager operation over its awsJson1_1 wire that must be served.
    fn ssm(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, text) = self.send(
            "ssm",
            &[
                ("content-type", "application/x-amz-json-1.1"),
                ("x-amz-target", &format!("AmazonSSM.{operation}")),
            ],
            body.to_string().into_bytes(),
        );
        let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        assert_eq!(
            status, 200,
            "case {case:?}: Systems Manager {operation} must be served, got {status} {parsed}"
        );
        parsed
    }
}

/// One row: the two trust documents the stack's two roles are declared with, the capacity each
/// of its two groups is asked for, the cadence and threshold the shared target group is checked
/// at, and the name the row's own stack takes so two rows cannot collide.
struct Row {
    case: &'static str,
    /// The `AssumeRolePolicyDocument` of the role whose group must end up MANAGED.
    admitting: Value,
    /// The `AssumeRolePolicyDocument` of the role whose group must end up unmanaged: a document
    /// that withholds the assume from Amazon EC2.
    withholding: Value,
    /// `MinSize`/`MaxSize`/`DesiredCapacity` of each group.
    desired: usize,
    /// `CreateTargetGroup$HealthCheckIntervalSeconds` — "the range is 5-300".
    interval: u32,
    /// `CreateTargetGroup$HealthyThresholdCount` — "the range is 2-10".
    healthy_threshold: u32,
    stack: &'static str,
}

impl Row {
    fn trusted_group(&self) -> String {
        format!("{}-managed", self.stack)
    }

    fn untrusted_group(&self) -> String {
        format!("{}-unmanaged", self.stack)
    }

    fn target_group(&self) -> String {
        format!("{}-tg", self.stack)
    }

    fn load_balancer(&self) -> String {
        format!("{}-alb", self.stack)
    }
}

/// A trust policy with one `Allow` of `actions` for the service principals `principals` — each
/// of the two written as one string or as a list, which is how IAM reads either.
fn trust(principals: Value, actions: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principals },
            "Action": actions,
        }],
    })
}

/// The recorded row: the withholding role simply names another service, which is the shape a
/// client that attached the wrong trust policy ends up with.
fn another_service_row() -> Row {
    Row {
        case: "a withholding role whose trust policy names another service",
        admitting: trust(json!(EC2_PRINCIPAL), json!("sts:AssumeRole")),
        withholding: trust(json!("lambda.amazonaws.com"), json!("sts:AssumeRole")),
        desired: 1,
        interval: 5,
        healthy_threshold: 2,
        stack: "cfnrt-other",
    }
}

/// The same withholding written as a document that names Amazon EC2 for an action that is not
/// the assume, over two instances per group and a slower cadence; the admitting role names
/// several services, Amazon EC2 among them.
fn other_action_row() -> Row {
    Row {
        case: "a withholding role whose trust policy names this service for another action",
        admitting: trust(
            json!(["ssm.amazonaws.com", EC2_PRINCIPAL]),
            json!("sts:AssumeRole"),
        ),
        withholding: trust(json!(EC2_PRINCIPAL), json!("sts:TagSession")),
        desired: 2,
        interval: 10,
        healthy_threshold: 2,
        stack: "cfnrt-action",
    }
}

/// The withholding written as an explicit `Deny` beside an `Allow` of the very same assume,
/// which IAM resolves in the `Deny`'s favour; the admitting role names the assume in a LIST of
/// actions.
fn explicit_deny_row() -> Row {
    Row {
        case: "a withholding role whose trust policy denies this service the assume it allows",
        admitting: trust(
            json!(EC2_PRINCIPAL),
            json!(["sts:AssumeRole", "sts:TagSession"]),
        ),
        withholding: json!({
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Effect": "Allow",
                    "Principal": { "Service": EC2_PRINCIPAL },
                    "Action": "sts:AssumeRole",
                },
                {
                    "Effect": "Deny",
                    "Principal": { "Service": EC2_PRINCIPAL },
                    "Action": "sts:AssumeRole",
                },
            ],
        }),
        desired: 1,
        interval: 5,
        healthy_threshold: 3,
        stack: "cfnrt-deny",
    }
}

/// The withholding written as a document that parses but carries no statement at all, so it
/// grants nothing to anyone; the admitting role grants the whole namespace the assume is in.
fn no_statement_row() -> Row {
    Row {
        case: "a withholding role whose trust policy carries no statement at all",
        admitting: trust(json!(EC2_PRINCIPAL), json!("sts:*")),
        withholding: json!({ "Version": "2012-10-17", "Statement": [] }),
        desired: 1,
        interval: 5,
        healthy_threshold: 2,
        stack: "cfnrt-empty",
    }
}

/// The template one row deploys: the network and the one security group whose self-referencing
/// inbound rule admits the load balancer's checks, the target group, the internal Application
/// Load Balancer and its listener, and then TWICE over — one role, one instance profile, one
/// launch template and one Auto Scaling group attached to that same target group — so the two
/// halves differ in nothing but the trust document their role was declared with.
fn template(row: &Row) -> String {
    let mut resources = json!({
        "Vpc": {
            "Type": "AWS::EC2::VPC",
            "Properties": { "CidrBlock": "10.0.0.0/16" },
        },
        "SubnetA": {
            "Type": "AWS::EC2::Subnet",
            "Properties": {
                "VpcId": { "Ref": "Vpc" },
                "CidrBlock": "10.0.1.0/24",
                "AvailabilityZone": format!("{REGION}a"),
            },
        },
        "SubnetB": {
            "Type": "AWS::EC2::Subnet",
            "Properties": {
                "VpcId": { "Ref": "Vpc" },
                "CidrBlock": "10.0.2.0/24",
                "AvailabilityZone": format!("{REGION}b"),
            },
        },
        "InstanceSecurityGroup": {
            "Type": "AWS::EC2::SecurityGroup",
            "Properties": {
                "GroupDescription": "the instances of both groups, and the load balancer",
                "VpcId": { "Ref": "Vpc" },
            },
        },
        // A health check is a connection the load balancer opens to the target, so it arrives
        // only where a rule names both its port and that load balancer as a source: the
        // balancer sits in the instances' own group, whose self-referencing rule admits it.
        "InstanceIngress": {
            "Type": "AWS::EC2::SecurityGroupIngress",
            "Properties": {
                "GroupId": { "Ref": "InstanceSecurityGroup" },
                "IpProtocol": "tcp",
                "FromPort": 80,
                "ToPort": 80,
                "SourceSecurityGroupId": { "Ref": "InstanceSecurityGroup" },
            },
        },
        "TargetGroup": {
            "Type": "AWS::ElasticLoadBalancingV2::TargetGroup",
            "Properties": {
                "Name": row.target_group(),
                "Port": 80,
                "Protocol": "HTTP",
                "TargetType": "instance",
                "VpcId": { "Ref": "Vpc" },
                "HealthCheckIntervalSeconds": row.interval,
                "HealthyThresholdCount": row.healthy_threshold,
                "UnhealthyThresholdCount": 2,
            },
        },
        "LoadBalancer": {
            "Type": "AWS::ElasticLoadBalancingV2::LoadBalancer",
            "Properties": {
                "Name": row.load_balancer(),
                "Scheme": "internal",
                "Type": "application",
                "SecurityGroups": [{ "Ref": "InstanceSecurityGroup" }],
                "Subnets": [{ "Ref": "SubnetA" }, { "Ref": "SubnetB" }],
            },
        },
        "Listener": {
            "Type": "AWS::ElasticLoadBalancingV2::Listener",
            "Properties": {
                "LoadBalancerArn": { "Ref": "LoadBalancer" },
                "Port": 80,
                "Protocol": "HTTP",
                "DefaultActions": [{ "Type": "forward", "TargetGroupArn": { "Ref": "TargetGroup" } }],
            },
        },
    });
    let map = resources
        .as_object_mut()
        .expect("the resource map is an object");
    // The withholding half is declared to be created FIRST, and the admitting half to wait for
    // it: an independent resource of a stack is created alongside its siblings, so the ONE thing
    // that fixes the order of the two launches is the dependency between the two groups — and it
    // is what makes the absence of a node an answer rather than a race (see the module docs).
    for (half, document, group, depends_on) in [
        (
            "Unmanaged",
            &row.withholding,
            row.untrusted_group(),
            json!("Listener"),
        ),
        (
            "Managed",
            &row.admitting,
            row.trusted_group(),
            json!(["Listener", "UnmanagedGroup"]),
        ),
    ] {
        map.insert(
            format!("{half}Role"),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": document,
                    // The same grant on both roles: what a row varies is the TRUST alone.
                    "ManagedPolicyArns": [SSM_CORE_POLICY],
                },
            }),
        );
        map.insert(
            format!("{half}Profile"),
            json!({
                "Type": "AWS::IAM::InstanceProfile",
                "Properties": { "Roles": [{ "Ref": format!("{half}Role") }] },
            }),
        );
        map.insert(
            format!("{half}LaunchTemplate"),
            json!({
                "Type": "AWS::EC2::LaunchTemplate",
                "Properties": {
                    "LaunchTemplateName": format!("{group}-lt"),
                    "LaunchTemplateData": {
                        "ImageId": { "Ref": "ImageId" },
                        "InstanceType": INSTANCE_TYPE,
                        "SecurityGroupIds": [{ "Ref": "InstanceSecurityGroup" }],
                        "IamInstanceProfile": { "Name": { "Ref": format!("{half}Profile") } },
                    },
                },
            }),
        );
        map.insert(
            format!("{half}Group"),
            json!({
                "Type": "AWS::AutoScaling::AutoScalingGroup",
                "DependsOn": depends_on,
                "Properties": {
                    "AutoScalingGroupName": group,
                    "MinSize": row.desired.to_string(),
                    "MaxSize": row.desired.to_string(),
                    "DesiredCapacity": row.desired.to_string(),
                    "VPCZoneIdentifier": [{ "Ref": "SubnetA" }, { "Ref": "SubnetB" }],
                    "LaunchTemplate": {
                        "LaunchTemplateId": { "Ref": format!("{half}LaunchTemplate") },
                        "Version": { "Fn::GetAtt": [format!("{half}LaunchTemplate"), "LatestVersionNumber"] },
                    },
                    "TargetGroupARNs": [{ "Ref": "TargetGroup" }],
                },
            }),
        );
    }
    json!({
        "Parameters": {
            "ImageId": {
                "Type": "AWS::SSM::Parameter::Value<AWS::EC2::Image::Id>",
                "Default": AL2023_PARAMETER,
            },
        },
        "Resources": resources,
        "Outputs": {
            "TargetGroupArn": { "Value": { "Ref": "TargetGroup" } },
            "ManagedGroupName": { "Value": { "Ref": "ManagedGroup" } },
            "UnmanagedGroupName": { "Value": { "Ref": "UnmanagedGroup" } },
            "ManagedRoleName": { "Value": { "Ref": "ManagedRole" } },
            "UnmanagedRoleName": { "Value": { "Ref": "UnmanagedRole" } },
            "ManagedProfileName": { "Value": { "Ref": "ManagedProfile" } },
            "UnmanagedProfileName": { "Value": { "Ref": "UnmanagedProfile" } },
        },
    })
    .to_string()
}

/// What a stack event says FAILED: the resource and the reason its own provider reported, for
/// the one assertion a stack that did not complete has to name.
fn failures(events: &str) -> Vec<(String, String)> {
    members_of(events)
        .into_iter()
        .filter(|event| {
            elements(event, "ResourceStatus")
                .iter()
                .any(|status| status.ends_with("_FAILED"))
        })
        .filter_map(|event| {
            let logical_id = elements(event, "LogicalResourceId").into_iter().next()?;
            let reason = elements(event, "ResourceStatusReason")
                .into_iter()
                .next()
                .unwrap_or_default();
            Some((logical_id, reason))
        })
        .collect()
}

/// Deploy one row's stack, look again until it reaches a terminal status — which must be
/// `CREATE_COMPLETE` — and answer the outputs it publishes.
fn deploy(emulator: &Emulator, row: &Row) -> BTreeMap<String, String> {
    let case = row.case;
    let body = template(row);
    emulator.cloudformation(
        case,
        "CreateStack",
        &[
            ("StackName", row.stack),
            ("TemplateBody", &body),
            // The template declares IAM roles and instance profiles, which CloudFormation
            // creates only under an acknowledgement.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    );
    let until = Instant::now() + DEADLINE;
    loop {
        let described =
            emulator.cloudformation(case, "DescribeStacks", &[("StackName", row.stack)]);
        let reported = elements(&described, "StackStatus")
            .first()
            .cloned()
            .unwrap_or_default();
        if !reported.ends_with(IN_PROGRESS_SUFFIX) {
            let events =
                emulator.cloudformation(case, "DescribeStackEvents", &[("StackName", row.stack)]);
            assert_eq!(
                reported, CREATE_COMPLETE,
                "case {case:?}: the stack must complete, got {reported:?}; what failed was {:?}",
                failures(&events)
            );
            return outputs_of(&described);
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: the stack must reach a terminal status, and it reported {reported:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// One stack output, or a panic naming what the stack reported.
fn output(case: &str, outputs: &BTreeMap<String, String>, key: &str) -> String {
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output, got {outputs:?}"))
}

/// The instance ids the group `group` reports in the `InService` lifecycle state, as a client
/// polling `DescribeAutoScalingGroups` reads them — each read off the member that carries BOTH
/// the id and the state, so an instance in another state is never mistaken for one in service.
fn in_service_instances(emulator: &Emulator, case: &str, group: &str) -> Vec<String> {
    let described = emulator.autoscaling(
        case,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    );
    let Some(held) = block_of(&described, "Instances") else {
        return Vec::new();
    };
    let mut found: Vec<String> = members_of(held)
        .into_iter()
        .filter(|member| {
            elements(member, "LifecycleState")
                .iter()
                .any(|state| state == IN_SERVICE)
        })
        .filter_map(|member| elements(member, "InstanceId").into_iter().next())
        .collect();
    found.sort();
    found
}

/// Look again until `group` holds the capacity the row declared in service, and answer the ids.
fn settled_capacity(emulator: &Emulator, row: &Row, group: &str) -> Vec<String> {
    let case = row.case;
    let until = Instant::now() + DEADLINE;
    loop {
        let found = in_service_instances(emulator, case, group);
        if found.len() >= row.desired {
            return found;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected {} instance(s) of {group} to reach {IN_SERVICE}, got {found:?}",
            row.desired
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The `(target id, TargetHealth$State)` pairs the target group `arn` reports right now — each
/// read off the `TargetHealthDescriptions` member that carries the target AND its health, so a
/// member that ever omitted half of it could not misalign the pairing.
fn target_health(emulator: &Emulator, case: &str, arn: &str) -> Vec<(String, String)> {
    let body = emulator.elbv2(case, "DescribeTargetHealth", &[("TargetGroupArn", arn)]);
    let Some(descriptions) = block_of(&body, "TargetHealthDescriptions") else {
        return Vec::new();
    };
    let mut found: Vec<(String, String)> = members_of(descriptions)
        .into_iter()
        .filter_map(|member| {
            let target = block_of(member, "Target")?;
            let health = block_of(member, "TargetHealth").unwrap_or_default();
            Some((
                elements(target, "Id").into_iter().next()?,
                elements(health, "State").into_iter().next()?,
            ))
        })
        .collect();
    found.sort();
    found
}

/// Look again until the target group `arn` holds `wanted` registrations and every one of them has
/// settled on the verdict the row's own rules make the checks reach, and answer them.
fn settled_health(
    emulator: &Emulator,
    row: &Row,
    arn: &str,
    wanted: usize,
) -> Vec<(String, String)> {
    let case = row.case;
    let until = Instant::now() + DEADLINE;
    loop {
        let found = target_health(emulator, case, arn);
        if found.len() >= wanted && found.iter().all(|(_, state)| state == HEALTHY) {
            return found;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected {wanted} registration(s) of {arn} to reach {HEALTHY}, got \
             {found:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The instance-profile ARN Amazon EC2 reports `instance_id` runs as, and an empty string when it
/// reports none.
fn instance_profile_arn(emulator: &Emulator, case: &str, instance_id: &str) -> String {
    let described = emulator.ec2(case, "DescribeInstances", &[("InstanceId.1", instance_id)]);
    block_of(&described, "iamInstanceProfile")
        .and_then(|reported| elements(reported, "arn").into_iter().next())
        .unwrap_or_default()
}

/// The `InstanceState$Name` Amazon EC2 reports for `instance_id`, and an empty string for an id
/// it holds no instance for.
fn instance_state(emulator: &Emulator, case: &str, instance_id: &str) -> String {
    let described = emulator.ec2(case, "DescribeInstances", &[("InstanceId.1", instance_id)]);
    block_of(&described, "instanceState")
        .and_then(|state| elements(state, "name").into_iter().next())
        .unwrap_or_default()
}

/// The `InstanceInformation` Systems Manager reports for `instance_id`, or `None` when it reports
/// none — read through the filter a client narrows the report with, following `NextToken` so
/// "absent" means absent from EVERY page.
fn node(emulator: &Emulator, case: &str, instance_id: &str) -> Option<Value> {
    let mut request = json!({ "Filters": [{ "Key": "InstanceIds", "Values": [instance_id] }] });
    loop {
        let reported = emulator.ssm(case, "DescribeInstanceInformation", &request);
        let listed = reported["InstanceInformationList"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            listed.len() <= 1,
            "case {case:?}: a query narrowed to {instance_id} reports at most one node, got \
             {reported}"
        );
        if let Some(found) = listed.first() {
            return Some(found.clone());
        }
        match reported["NextToken"].as_str() {
            Some(token) if !token.is_empty() => {
                request["NextToken"] = Value::String(token.to_string());
            }
            _ => return None,
        }
    }
}

/// Look again until Systems Manager reports `instance_id` as a managed node, and answer it.
fn node_when_registered(emulator: &Emulator, case: &str, instance_id: &str) -> Value {
    let until = Instant::now() + DEADLINE;
    loop {
        if let Some(reported) = node(emulator, case, instance_id) {
            return reported;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected {instance_id} to become a managed node, and \
             DescribeInstanceInformation reported none"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// One member of a reported node, as the text a client reads off it.
fn member(reported: &Value, name: &str) -> String {
    reported[name].as_str().unwrap_or_default().to_string()
}

/// Deploy one row's stack and assert both halves of the rule: the stack converges, both groups
/// put their declared capacity into service and are registered healthy in the ONE target group
/// the template attached them to, and only the group whose role admits the assume has a managed
/// node — reported under that role's own name.
fn deploys(row: &Row) {
    let case = row.case;
    let emulator = Emulator::new();
    let outputs = deploy(&emulator, row);

    // The stack publishes the handles the rest of the walk is taken through, and each is the
    // value the owning service holds the resource by.
    let target_group_arn = output(case, &outputs, "TargetGroupArn");
    assert!(
        target_group_arn.contains(&row.target_group()),
        "case {case:?}: expected the exported ARN to name the declared target group {}, got \
         {target_group_arn:?}",
        row.target_group()
    );
    for (key, expected) in [
        ("ManagedGroupName", row.trusted_group()),
        ("UnmanagedGroupName", row.untrusted_group()),
    ] {
        let reported = output(case, &outputs, key);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the stack to export {expected} under {key}, got {reported:?}"
        );
    }
    let managed_role = output(case, &outputs, "ManagedRoleName");
    let unmanaged_role = output(case, &outputs, "UnmanagedRoleName");
    assert_ne!(
        managed_role, unmanaged_role,
        "case {case:?}: the two halves must be two roles, got {managed_role:?} for both"
    );

    // Both groups launch their declared capacity and put every instance in service...
    let managed = settled_capacity(&emulator, row, &row.trusted_group());
    let unmanaged = settled_capacity(&emulator, row, &row.untrusted_group());
    for (half, ids) in [("managed", &managed), ("unmanaged", &unmanaged)] {
        assert_eq!(
            ids.len(),
            row.desired,
            "case {case:?}: expected the {half} group to hold {} instance(s) {IN_SERVICE}, got \
             {ids:?}",
            row.desired
        );
        // ...each of them an instance Amazon EC2 itself holds and reports running, running as
        // the instance profile its own half declared: the two halves differ in the trust
        // document alone, so both instances really do run as a profile.
        for id in ids {
            let state = instance_state(&emulator, case, id);
            assert_eq!(
                state, RUNNING,
                "case {case:?}: expected Amazon EC2 to hold {id} {RUNNING}, got {state:?}"
            );
        }
    }
    for (half, ids, key) in [
        ("managed", &managed, "ManagedProfileName"),
        ("unmanaged", &unmanaged, "UnmanagedProfileName"),
    ] {
        let profile = output(case, &outputs, key);
        for id in ids {
            let reported = instance_profile_arn(&emulator, case, id);
            assert!(
                reported.ends_with(&format!("/{profile}")),
                "case {case:?}: expected the {half} instance {id} to run as the profile \
                 {profile}, got {reported:?}"
            );
        }
    }

    // ...and the ONE target group both groups were attached to holds EXACTLY their instances,
    // every registration settling healthy: the registration is the group's, so what the role
    // trusts does not decide it.
    let mut expected: Vec<String> = managed.iter().chain(unmanaged.iter()).cloned().collect();
    expected.sort();
    let reported = settled_health(&emulator, row, &target_group_arn, expected.len());
    let registered: Vec<String> = reported.iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(
        registered, expected,
        "case {case:?}: expected the attached target group to hold exactly the two groups' \
         in-service instances {expected:?}, got {reported:?}"
    );
    for (id, state) in &reported {
        assert_eq!(
            state, HEALTHY,
            "case {case:?}: expected the registration of {id} to reach {HEALTHY}, got {state:?}"
        );
    }

    // The group whose role Amazon EC2 may act as has a managed node per instance, reported under
    // that role's NAME...
    for id in &managed {
        let reported = node_when_registered(&emulator, case, id);
        assert_eq!(
            member(&reported, "IamRole"),
            managed_role,
            "case {case:?}: expected the node of {id} to report the role {managed_role}, got \
             {reported}"
        );
        assert_eq!(
            member(&reported, "PingStatus"),
            ONLINE,
            "case {case:?}: expected the node of {id} to be {ONLINE}, got {reported}"
        );
        assert_eq!(
            member(&reported, "ResourceType"),
            RESOURCE_TYPE_EC2,
            "case {case:?}: expected the node of {id} to be an {RESOURCE_TYPE_EC2}, got {reported}"
        );
    }

    // ...and the twin whose role withholds that assume has none. The withholding group was
    // created before the admitting one, so its instance launched first and the registration it
    // would have been owed fell due before the node observed above did.
    for id in &unmanaged {
        let reported = node(&emulator, case, id);
        assert!(
            reported.is_none(),
            "case {case:?}: expected {id}, whose role {unmanaged_role} withholds the assume, to \
             be managed by nothing, got {reported:?}"
        );
    }
}

/// Drive one row on a thread sized for the whole router's assembly.
fn drive(row: fn() -> Row) {
    std::thread::Builder::new()
        .name("cfn-asg-role-trust".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || deploys(&row()))
        .expect("a sized thread")
        .join()
        .expect("the row runs to completion");
}

/// The recorded row: the withholding role names another service entirely.
#[test]
fn a_stack_declared_group_whose_role_names_another_service_is_registered_but_unmanaged() {
    drive(another_service_row);
}

/// The same withholding named for another action, over two instances per group.
#[test]
fn a_stack_declared_group_whose_role_is_trusted_for_another_action_is_registered_but_unmanaged() {
    drive(other_action_row);
}

/// The same withholding written as an explicit deny of the assume the document also allows.
#[test]
fn a_stack_declared_group_whose_role_denies_the_assume_is_registered_but_unmanaged() {
    drive(explicit_deny_row);
}

/// The same withholding written as a document that carries no statement at all.
#[test]
fn a_stack_declared_group_whose_role_states_nothing_is_registered_but_unmanaged() {
    drive(no_statement_row);
}
