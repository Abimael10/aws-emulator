// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-deployed patch-management estate, on the whole assembled emulator a
//! client talks to: the patch baseline a stack declares, the patch group it registers that
//! baseline for, the instances of that group, and the compliance Patch Manager reports for
//! them once the stack's State Manager association has applied the patch-baseline document.
//!
//! RULE under test: a stack's resources are not CloudFormation's own. An
//! `AWS::SSM::PatchBaseline` is a patch baseline in AWS Systems Manager, created through that
//! service's own API by the operation that deploys the template, and the stack's physical
//! resource id — what `Ref` resolves to — is the `pb-…` id THAT service minted; the
//! `PatchGroups` the declaration names are registered with it there. What follows from that
//! registration is the rest of the estate: the baseline that should be used for a patch group
//! is the one registered for it (and the default baseline of the node's operating system for
//! a group with none), a node's group is the value of its `Patch Group` or `PatchGroup` tag,
//! and the run of `AWS-RunPatchBaseline` the stack's association applies to the nodes its
//! targets select is performed against that baseline — so the patch reads and the compliance
//! read report, for each node, the baseline its own group holds.
//!
//! This suite drives the ASSEMBLED router — every service mounted on one engine, as
//! `core/router/src/lib.rs` assembles it — because that is the emulator a client reaches and
//! because the estate is derived across four of those services: CloudFormation deploys the
//! template, IAM holds the instance profile, Amazon EC2 launches the instances from an image
//! resolved through a Systems Manager public parameter, and Systems Manager registers the
//! nodes, holds the baseline and reports the compliance. Nothing reaches into a state struct;
//! every fact is written and read through the service that owns it. Assembling the whole
//! router costs stack (see `boot_serve.rs`), so it is built — and the requests driven — on a
//! sized thread.
//!
//! The rows differ along what the rule generalises over: which of the two tag spellings names
//! a node's patch group, whether that group is registered with a baseline of the stack's,
//! and a node carrying no patch-group tag at all. The second baseline the template declares
//! carries NO `Name`, which is the other axis: a declaration that names none has one
//! generated for it.
//!
//! Nothing here waits a fixed time: every span this suite crosses — Amazon EC2's move to
//! `running`, the managed-node registration, the application of the association and the
//! compliance its run reports — is a stored due instant, so a case LOOKS AGAIN until the
//! condition holds or [`DEADLINE`] passes, and no assertion is made about elapsed time.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The form encoder and the XML readers an awsQuery suite needs live ONCE in the test crate,
// in `query_wire`.
use crate::query_wire::{elements, form_encode, members_of, outputs_of};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// How long a case is given for a condition another component's clock owes.
///
/// Generous on purpose: each span is a stored due instant and nothing sleeps for one, so a
/// case looks again until the condition holds. All this bound is for is that a move that
/// never comes fails instead of hanging; nothing asserts on elapsed time.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before polling again. Not a bound on anything.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The stack this suite deploys.
const STACK_NAME: &str = "cfn-patch-estate";

/// The patch group the stack's baseline is registered for.
const REGISTERED_GROUP: &str = "cfn-patch-estate-production";

/// A patch group nothing registers a baseline for, which one instance belongs to.
const UNREGISTERED_GROUP: &str = "cfn-patch-estate-staging";

/// The name the template gives its first baseline.
const DECLARED_BASELINE_NAME: &str = "cfn-patch-estate-baseline";

/// The operating system the instances of this estate are patched against: they are launched
/// from the image the Amazon Linux 2023 public parameter resolves to.
const OPERATING_SYSTEM: &str = "AMAZON_LINUX_2023";

/// The compliance type Patch Manager reports its items under, and the one resource type the
/// compliance reads support.
const COMPLIANCE_TYPE_PATCH: &str = "Patch";
const RESOURCE_TYPE_MANAGED_INSTANCE: &str = "ManagedInstance";

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request and where its Region comes from; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=deadbeef"
    )
}

/// The emulator this suite drives: the whole router over one engine, and the runtime its
/// requests are blocked on.
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
            // (`core/router/src/main.rs`): the services this suite drives own clock-owed
            // work that wakes on a timer.
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
            .header("host", host)
            .header("authorization", authorization(signing_name));
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

    /// One awsQuery CloudFormation action: `POST /` with the members form-encoded.
    fn cloudformation(&self, action: &str, members: &[(&str, &str)]) -> (u16, String) {
        let mut body = format!("Action={action}&Version=2010-05-15");
        for (key, value) in members {
            body.push('&');
            body.push_str(&form_encode(key));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        self.send(
            "cloudformation",
            &[("content-type", "application/x-www-form-urlencoded")],
            body.into_bytes(),
        )
    }

    /// One Systems Manager operation over its awsJson1_1 wire, as `(status, body)`.
    fn ssm(&self, operation: &str, body: &Value) -> (u16, Value) {
        let (status, text) = self.send(
            "ssm",
            &[
                ("content-type", "application/x-amz-json-1.1"),
                ("x-amz-target", &format!("AmazonSSM.{operation}")),
            ],
            body.to_string().into_bytes(),
        );
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// One Systems Manager operation that must be served, as its parsed body.
    fn ssm_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.ssm(operation, body);
        assert_eq!(
            status, 200,
            "case {case:?}: Systems Manager {operation} must be served, got {status} {parsed}"
        );
        parsed
    }
}

/// The template this suite deploys: an SSM-managed estate of four instances, a patch baseline
/// registered for one patch group, a second baseline the template gives no name, and the
/// State Manager association that applies the published patch-baseline document to every
/// managed node of the account.
const TEMPLATE: &str = r#"{
 "Parameters": {
  "ImageId": {
   "Type": "AWS::SSM::Parameter::Value<AWS::EC2::Image::Id>",
   "Default": "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64"
  }
 },
 "Resources": {
  "Role": {
   "Type": "AWS::IAM::Role",
   "Properties": {
    "AssumeRolePolicyDocument": {
     "Version": "2012-10-17",
     "Statement": [
      {
       "Effect": "Allow",
       "Principal": { "Service": "ec2.amazonaws.com" },
       "Action": "sts:AssumeRole"
      }
     ]
    },
    "ManagedPolicyArns": ["arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"]
   }
  },
  "Profile": {
   "Type": "AWS::IAM::InstanceProfile",
   "Properties": { "Roles": [ { "Ref": "Role" } ] }
  },
  "SpacedTagInstance": {
   "Type": "AWS::EC2::Instance",
   "Properties": {
    "ImageId": { "Ref": "ImageId" },
    "InstanceType": "t3.micro",
    "IamInstanceProfile": { "Ref": "Profile" },
    "Tags": [ { "Key": "Patch Group", "Value": "cfn-patch-estate-production" } ]
   }
  },
  "CompactTagInstance": {
   "Type": "AWS::EC2::Instance",
   "Properties": {
    "ImageId": { "Ref": "ImageId" },
    "InstanceType": "t3.micro",
    "IamInstanceProfile": { "Ref": "Profile" },
    "Tags": [ { "Key": "PatchGroup", "Value": "cfn-patch-estate-production" } ]
   }
  },
  "UnregisteredGroupInstance": {
   "Type": "AWS::EC2::Instance",
   "Properties": {
    "ImageId": { "Ref": "ImageId" },
    "InstanceType": "t3.micro",
    "IamInstanceProfile": { "Ref": "Profile" },
    "Tags": [ { "Key": "Patch Group", "Value": "cfn-patch-estate-staging" } ]
   }
  },
  "UngroupedInstance": {
   "Type": "AWS::EC2::Instance",
   "Properties": {
    "ImageId": { "Ref": "ImageId" },
    "InstanceType": "t3.micro",
    "IamInstanceProfile": { "Ref": "Profile" },
    "Tags": [ { "Key": "Name", "Value": "cfn-patch-estate-ungrouped" } ]
   }
  },
  "Baseline": {
   "Type": "AWS::SSM::PatchBaseline",
   "Properties": {
    "Name": "cfn-patch-estate-baseline",
    "OperatingSystem": "AMAZON_LINUX_2023",
    "Description": "the estate baseline",
    "ApprovalRules": {
     "PatchRules": [
      {
       "PatchFilterGroup": {
        "PatchFilters": [ { "Key": "PRODUCT", "Values": [ "*" ] } ]
       },
       "ApproveAfterDays": 0,
       "ComplianceLevel": "HIGH",
       "EnableNonSecurity": true
      }
     ]
    },
    "PatchGroups": [ "cfn-patch-estate-production" ],
    "Tags": [ { "Key": "estate", "Value": "patch" } ]
   }
  },
  "UnnamedBaseline": {
   "Type": "AWS::SSM::PatchBaseline",
   "Properties": {
    "OperatingSystem": "AMAZON_LINUX_2023",
    "ApprovedPatches": [ "kernel" ]
   }
  },
  "PatchAssociation": {
   "Type": "AWS::SSM::Association",
   "Properties": {
    "Name": "AWS-RunPatchBaseline",
    "Targets": [ { "Key": "InstanceIds", "Values": [ "*" ] } ],
    "Parameters": { "Operation": [ "Scan" ] },
    "ScheduleExpression": "rate(30 minutes)"
   }
  }
 },
 "Outputs": {
  "SpacedTagInstanceId": { "Value": { "Ref": "SpacedTagInstance" } },
  "CompactTagInstanceId": { "Value": { "Ref": "CompactTagInstance" } },
  "UnregisteredGroupInstanceId": { "Value": { "Ref": "UnregisteredGroupInstance" } },
  "UngroupedInstanceId": { "Value": { "Ref": "UngroupedInstance" } },
  "BaselineId": { "Value": { "Ref": "Baseline" } },
  "UnnamedBaselineId": { "Value": { "Fn::GetAtt": [ "UnnamedBaseline", "Id" ] } },
  "AssociationId": { "Value": { "Ref": "PatchAssociation" } }
 }
}"#;

/// What the rule says about one instance of the estate: the group it belongs to, and whether
/// the baseline in force for it is the stack's own or the default of its operating system.
#[derive(Debug, Clone, Copy)]
enum Expected {
    /// The baseline the stack declared and registered for the instance's group.
    StackBaseline,
    /// The default baseline of the instance's operating system, which is what a group with no
    /// registration of its own is patched against.
    DefaultBaseline,
}

/// One row of the table: an instance of the estate, the stack output carrying its id, the
/// patch group it belongs to, and which baseline its run must have been performed against.
#[derive(Debug, Clone, Copy)]
struct Case {
    case: &'static str,
    output: &'static str,
    patch_group: &'static str,
    expected: Expected,
}

/// The rows, one per instance the template declares.
fn rows() -> Vec<Case> {
    vec![
        Case {
            case: "an instance whose `Patch Group` tag names the registered group",
            output: "SpacedTagInstanceId",
            patch_group: REGISTERED_GROUP,
            expected: Expected::StackBaseline,
        },
        Case {
            case: "an instance whose `PatchGroup` tag names the same group",
            output: "CompactTagInstanceId",
            patch_group: REGISTERED_GROUP,
            expected: Expected::StackBaseline,
        },
        Case {
            case: "an instance in a group nothing registered a baseline for",
            output: "UnregisteredGroupInstanceId",
            patch_group: UNREGISTERED_GROUP,
            expected: Expected::DefaultBaseline,
        },
        Case {
            case: "an instance carrying no patch-group tag at all",
            output: "UngroupedInstanceId",
            patch_group: "",
            expected: Expected::DefaultBaseline,
        },
    ]
}

/// Deploy the template and look again until the stack reaches a terminal status, which must
/// be `CREATE_COMPLETE`; answer its outputs.
fn deploy(emulator: &Emulator) -> BTreeMap<String, String> {
    let (status, body) = emulator.cloudformation(
        "CreateStack",
        &[
            ("StackName", STACK_NAME),
            ("TemplateBody", TEMPLATE),
            // The template declares an IAM role and an instance profile, which
            // CloudFormation creates only under an acknowledgement.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    );
    assert_eq!(
        status, 200,
        "CreateStack must be served, got {status} with body {body}"
    );

    let until = Instant::now() + DEADLINE;
    loop {
        let (status, body) = emulator.cloudformation("DescribeStacks", &[("StackName", STACK_NAME)]);
        assert_eq!(
            status, 200,
            "DescribeStacks must be served, got {status} with body {body}"
        );
        let reported = elements(&body, "StackStatus")
            .first()
            .cloned()
            .unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            let (_, events) =
                emulator.cloudformation("DescribeStackEvents", &[("StackName", STACK_NAME)]);
            assert_eq!(
                reported, "CREATE_COMPLETE",
                "the estate's stack must reach CREATE_COMPLETE, got {reported}; what failed \
                 was {:?}",
                failures(&events)
            );
            return outputs_of(&body);
        }
        assert!(
            Instant::now() < until,
            "the estate's stack must reach a terminal status, and it reported {reported}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The baseline id a reference names: its own id for a baseline the account created, and the
/// last segment of the ARN for one Systems Manager predefined — which is what a patch report
/// names the baseline it was gathered against by.
fn bare_id(reference: &str) -> String {
    reference
        .rsplit('/')
        .next()
        .unwrap_or(reference)
        .to_string()
}

/// What a stack event says FAILED: the resource and the reason its own provider reported,
/// for the one assertion a stack that did not complete has to name.
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

/// One stack output, or a panic naming what the stack reported.
fn output(outputs: &BTreeMap<String, String>, key: &str) -> String {
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("the stack must report the output {key}, got {outputs:?}"))
}

/// Look again until Systems Manager reports a patch state for `instance_id`, which is the
/// compliance the association's run brings back.
fn patch_state_when_reported(emulator: &Emulator, case: &str, instance_id: &str) -> Value {
    let until = Instant::now() + DEADLINE;
    loop {
        let reported = emulator.ssm_ok(
            case,
            "DescribeInstancePatchStates",
            &json!({ "InstanceIds": [instance_id] }),
        );
        let states = reported["InstancePatchStates"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(state) = states.first() {
            return state.clone();
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected the patching of {instance_id} to be reported, and \
             DescribeInstancePatchStates reported none"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[test]
fn a_cfn_declared_patch_baseline_patches_the_nodes_of_the_group_it_is_registered_for() {
    std::thread::Builder::new()
        .name("cfn-ssm-patch-estate".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let emulator = Emulator::new();
            let outputs = deploy(&emulator);
            let baseline_id = output(&outputs, "BaselineId");
            let unnamed_baseline_id = output(&outputs, "UnnamedBaselineId");

            // The id the stack publishes is one Systems Manager really minted: the baseline
            // reads back there, carrying what the declaration asked for — the patch group
            // among them.
            let case = "the baseline the stack declared";
            let read = emulator.ssm_ok(
                case,
                "GetPatchBaseline",
                &json!({ "BaselineId": baseline_id }),
            );
            assert_eq!(
                (
                    read["BaselineId"].as_str(),
                    read["Name"].as_str(),
                    read["OperatingSystem"].as_str(),
                    read["Description"].as_str()
                ),
                (
                    Some(baseline_id.as_str()),
                    Some(DECLARED_BASELINE_NAME),
                    Some(OPERATING_SYSTEM),
                    Some("the estate baseline")
                ),
                "case {case:?}: expected the declared baseline, got {read}"
            );
            assert_eq!(
                read["PatchGroups"].as_array().map(Vec::as_slice),
                Some([json!(REGISTERED_GROUP)].as_slice()),
                "case {case:?}: expected the declared patch group to be registered with it, \
                 got {read}"
            );
            assert!(
                read["ApprovalRules"]["PatchRules"]
                    .as_array()
                    .is_some_and(|rules| rules.len() == 1),
                "case {case:?}: expected the declared approval rule, got {read}"
            );

            // A declaration that names no baseline NAME has one generated for it, in the
            // form CloudFormation generates a name in and within what the owning model
            // admits for one.
            let case = "the baseline the template does not name";
            let read = emulator.ssm_ok(
                case,
                "GetPatchBaseline",
                &json!({ "BaselineId": unnamed_baseline_id }),
            );
            let generated = read["Name"].as_str().unwrap_or_default().to_string();
            assert!(
                generated.starts_with(STACK_NAME)
                    && generated.contains("UnnamedBaseline")
                    && (3..=128).contains(&generated.len())
                    && generated.chars().all(|character| {
                        character.is_ascii_alphanumeric()
                            || character == '_'
                            || character == '-'
                            || character == '.'
                    }),
                "case {case:?}: expected a generated name naming the stack and the resource, \
                 got {generated:?}"
            );
            assert_eq!(
                read["PatchGroups"].as_array().map(Vec::len),
                Some(0),
                "case {case:?}: expected a baseline no group was registered with, got {read}"
            );

            // The baseline that should be used for each group: the stack's own for the group
            // it registered, and the default of the operating system for the other.
            let default_baseline = emulator.ssm_ok(
                "the account's default baseline",
                "GetDefaultPatchBaseline",
                &json!({ "OperatingSystem": OPERATING_SYSTEM }),
            )["BaselineId"]
                .as_str()
                .expect("a default baseline is reported for every operating system")
                .to_string();
            for (case, group, expected) in [
                (
                    "the group the stack registered its baseline for",
                    REGISTERED_GROUP,
                    &baseline_id,
                ),
                (
                    "a group nothing registered a baseline for",
                    UNREGISTERED_GROUP,
                    &default_baseline,
                ),
            ] {
                let reported = emulator.ssm_ok(
                    case,
                    "GetPatchBaselineForPatchGroup",
                    &json!({ "PatchGroup": group, "OperatingSystem": OPERATING_SYSTEM }),
                );
                assert_eq!(
                    reported["BaselineId"].as_str(),
                    Some(expected.as_str()),
                    "case {case:?}: expected the baseline {expected}, got {reported}"
                );
            }

            // Each instance of the estate: the association applies the published
            // patch-baseline document to it, and what the run reports is the compliance of
            // the baseline its own group holds.
            let table = rows();
            for row in &table {
                let instance_id = output(&outputs, row.output);
                let state = patch_state_when_reported(&emulator, row.case, &instance_id);
                let expected = match row.expected {
                    Expected::StackBaseline => baseline_id.clone(),
                    // A baseline Systems Manager predefines is NAMED by its full ARN, which
                    // is the spelling a default read reports it under; a patch report names
                    // the baseline by its own id, the last segment of that ARN.
                    Expected::DefaultBaseline => bare_id(&default_baseline),
                };
                assert_eq!(
                    state["BaselineId"].as_str(),
                    Some(expected.as_str()),
                    "case {:?}: expected the run against the baseline {expected}, got {state}",
                    row.case
                );
                assert_eq!(
                    state["PatchGroup"].as_str().unwrap_or_default(),
                    row.patch_group,
                    "case {:?}: expected the report to carry the group {:?}, got {state}",
                    row.case,
                    row.patch_group
                );
                assert_eq!(
                    state["Operation"].as_str(),
                    Some("Scan"),
                    "case {:?}: expected the operation the association asked for, got {state}",
                    row.case
                );

                // The same compliance, read through the compliance API: one item per patch
                // the run reported, each carrying the baseline it was gathered against.
                let reported = emulator.ssm_ok(
                    row.case,
                    "ListComplianceItems",
                    &json!({
                        "ResourceIds": [instance_id],
                        "ResourceTypes": [RESOURCE_TYPE_MANAGED_INSTANCE],
                        "Filters": [{ "Key": "ComplianceType", "Values": [COMPLIANCE_TYPE_PATCH] }],
                        "MaxResults": 50,
                    }),
                );
                let items = reported["ComplianceItems"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                assert!(
                    !items.is_empty(),
                    "case {:?}: expected the run's patches to be reported as compliance items, \
                     got {reported}",
                    row.case
                );
                assert!(
                    items.iter().all(|item| {
                        item["ComplianceType"].as_str() == Some(COMPLIANCE_TYPE_PATCH)
                            && item["ResourceType"].as_str()
                                == Some(RESOURCE_TYPE_MANAGED_INSTANCE)
                            && item["ResourceId"].as_str() == Some(instance_id.as_str())
                            && item["Details"]["PatchBaselineId"].as_str()
                                == Some(expected.as_str())
                    }),
                    "case {:?}: expected every item to be this node's patch compliance against \
                     {expected}, got {items:?}",
                    row.case
                );
            }

            // The group read reports the nodes of that group and no other, which is what a
            // client polling a patch-group deployment reads.
            let case = "the registered group's own nodes";
            let reported = emulator.ssm_ok(
                case,
                "DescribeInstancePatchStatesForPatchGroup",
                &json!({ "PatchGroup": REGISTERED_GROUP }),
            );
            let mut reported_nodes: Vec<String> = reported["InstancePatchStates"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|state| state["InstanceId"].as_str().map(str::to_string))
                .collect();
            reported_nodes.sort();
            let mut expected_nodes: Vec<String> = table
                .iter()
                .filter(|row| row.patch_group == REGISTERED_GROUP)
                .map(|row| output(&outputs, row.output))
                .collect();
            expected_nodes.sort();
            assert_eq!(
                reported_nodes, expected_nodes,
                "case {case:?}: expected the group's own nodes, got {reported}"
            );
        })
        .expect("a sized thread")
        .join()
        .expect("the suite thread");
}
