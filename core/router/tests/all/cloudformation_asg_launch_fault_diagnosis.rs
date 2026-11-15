// SPDX-License-Identifier: CC0-1.0

//! The walk a client takes to DIAGNOSE why an Auto Scaling group a stack deployed launches
//! nothing: the group the stack published, then the launch-template version it launches
//! from, then the KMS key that version's root volume is encrypted under.
//!
//! The rule under test: **a stack that reaches a successful terminal state has really
//! created every resource it declares in the service that OWNS it, so each identifier it
//! publishes resolves there — the name of an Auto Scaling group resolves to exactly one
//! group, carrying the sizes its declaration stated and a launch-template specification
//! naming the stack's own template at the declared version, and the launch template and the
//! key that group's instances would use resolve in Amazon EC2 and in KMS the same way.**
//!
//! An Auto Scaling group is the type that makes the rule visible end to end: it is created
//! by one service, launches from a second service's resource and encrypts that resource's
//! volumes with a third's key, so a stack that reported it created and published its name
//! has to be a stack whose name reaches it. A read of that name answering an EMPTY group
//! list is the whole diagnosis gone: there is nothing to ask about the group's bounds, its
//! launch template or its key.
//!
//! `cloudformation_creates_an_ecs_capacity_provider.rs` also deploys a group through
//! CloudFormation, for what a capacity provider does with one; what this suite is about is
//! the group's own read — its declared sizes, the id/name completion of its launch-template
//! specification, that template's version, and the key that version's root volume is
//! encrypted under.
//!
//! Four crates have to agree for that to hold (CloudFormation makes the provisioning calls,
//! Amazon EC2 Auto Scaling holds the group, Amazon EC2 holds the launch template and its
//! versions, KMS holds the key, and IAM holds the role the instances run as), so the suite
//! lives here rather than in any one of them.
//!
//! The rows differ along what the rule generalises over: the group's declared sizes
//! (including a group declared at zero minimum and zero desired capacity, which launches
//! nothing, and groups sized above zero), how the declaration NAMES its launch template (by
//! the id Amazon EC2 minted, or by the name the template declares — the half the request
//! does not state is the one the owning service resolves and reports), how the version is
//! written (an `Fn::GetAtt` of the template's latest version number, or a literal version),
//! whether the root volume is encrypted under the stack's own key at all, and what the
//! declaration says about that key being enabled (`false`, `true`, or nothing at all).
//!
//! Every expected value is computed here from what the row declares, never read back out of
//! the answer being checked — except the identities the owning services mint (the group's
//! name, the launch template's id, the key's id and ARN), which are checked by being the
//! SAME value everywhere the stack and those services report them.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{
    block_of, elements, form_encode, items_of, opt_element, outputs_of, physical_ids_of,
};

/// The Region every request here is signed for, and so the Region the stack's resources and
/// their ARNs belong to.
const REGION: &str = "us-east-1";

/// How long a poll of a stack's status waits between reads, and how many times it reads: a
/// stack is owed its terminal status once the services that own its resources report them
/// ready, which only a read asks them about, so a client polls — which is what this is. The
/// bound is generous, because what the rows are about is that the stack settles at all, not
/// how quickly the host gets there.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const POLL_ATTEMPTS: usize = 80;

/// The AMI the launch template's data names, in Amazon EC2's own id form (`ami-` plus 17
/// hexadecimal characters).
const IMAGE_ID: &str = "ami-00000000000000001";

/// The instance type the launch template's data names.
const INSTANCE_TYPE: &str = "t3.micro";

/// The root device the launch template's block device mapping names, and the size and type
/// of the volume behind it.
const ROOT_DEVICE: &str = "/dev/xvda";
const ROOT_VOLUME_SIZE: u64 = 8;
const ROOT_VOLUME_TYPE: &str = "gp3";

/// The network the estate is deployed into.
const VPC_CIDR: &str = "10.198.0.0/16";
const SUBNET_CIDR: &str = "10.198.1.0/24";
const ZONE: &str = "us-east-1a";

/// The version a launch template created by a stack is on: `CreateLaunchTemplate` creates
/// the template with its first version, and no row creates a second one — so both version
/// forms a row writes resolve to this, and it is what the group must report.
const FIRST_VERSION: &str = "1";

/// How the declaration names the launch template its group launches from. "You must specify
/// either a LaunchTemplateId or a LaunchTemplateName" (`LaunchTemplateSpecification`), and
/// the group reports BOTH either way, the other half being the one the owning service
/// resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reference {
    ById,
    ByName,
}

/// How the declaration writes the version of that template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VersionForm {
    /// `Fn::GetAtt` of the template's `LatestVersionNumber`, which Amazon EC2 keeps.
    LatestVersionAttribute,
    /// The version written out as the declaration's own literal.
    Literal,
}

/// One row: the estate one stack declares, and what the walk over it must find.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `LaunchTemplateName` the template declares, which is also the name the group's
    /// own read must report for it.
    launch_template_name: &'static str,
    /// `(MinSize, MaxSize, DesiredCapacity)` as the declaration states them.
    sizes: (&'static str, &'static str, &'static str),
    reference: Reference,
    version: VersionForm,
    /// Whether the root volume is declared encrypted under the stack's own key.
    encrypted_root: bool,
    /// What the key's declaration says about `Enabled`, or `None` where it states nothing —
    /// "Enabled … Default: true".
    declared_enabled: Option<bool>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a group at zero minimum and zero desired capacity, launching from a template \
               named by id whose root volume is encrypted under a key the stack created \
               disabled",
        stack: "asg-fault-zero-capacity",
        launch_template_name: "asg-fault-zero-capacity-lt",
        sizes: ("0", "1", "0"),
        reference: Reference::ById,
        version: VersionForm::LatestVersionAttribute,
        encrypted_root: true,
        declared_enabled: Some(false),
    },
    Row {
        case: "a group sized above zero, launching from a template named by NAME at a literal \
               version, encrypted under a key the stack created enabled",
        stack: "asg-fault-named-template",
        launch_template_name: "asg-fault-named-template-lt",
        sizes: ("2", "5", "3"),
        reference: Reference::ByName,
        version: VersionForm::Literal,
        encrypted_root: true,
        declared_enabled: Some(true),
    },
    Row {
        case: "a group whose root volume is not encrypted at all, beside a key whose \
               declaration says nothing about being enabled",
        stack: "asg-fault-plain-volume",
        launch_template_name: "asg-fault-plain-volume-lt",
        sizes: ("1", "4", "2"),
        reference: Reference::ById,
        version: VersionForm::Literal,
        encrypted_root: false,
        declared_enabled: None,
    },
];

/// The `KeyState` and the `Enabled` flag KMS must report for a key declared as `declared`.
fn expected_key_state(declared: Option<bool>) -> (&'static str, bool) {
    if declared == Some(false) {
        ("Disabled", false)
    } else {
        ("Enabled", true)
    }
}

/// The emulator with the five services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way
/// (the credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_autoscaling_autoscaling_2011_01_01::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
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
    host: &str,
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
        .header("host", host)
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
    query(
        router,
        "cloudformation",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "2010-05-15",
        action,
        members,
    )
    .await
}

/// Call one Amazon EC2 Auto Scaling operation over its awsQuery wire.
async fn autoscaling(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(
        router,
        "autoscaling",
        &format!("autoscaling.{REGION}.amazonaws.com"),
        "2011-01-01",
        action,
        members,
    )
    .await
}

/// Call one Amazon EC2 operation over its ec2Query wire.
async fn ec2(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(
        router,
        "ec2",
        &format!("ec2.{REGION}.amazonaws.com"),
        "2016-11-15",
        action,
        members,
    )
    .await
}

/// Call one KMS operation over its awsJson1_1 wire.
async fn kms(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("kms.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TrentService.{operation}"))
        .header("authorization", authorization("kms"))
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The block device mapping one row declares for the instances' ROOT volume: encrypted under
/// the stack's own key where the row says so, and a plain volume otherwise.
fn root_volume(row: &Row) -> Value {
    let mut ebs = json!({
        "VolumeSize": ROOT_VOLUME_SIZE,
        "VolumeType": ROOT_VOLUME_TYPE,
        "DeleteOnTermination": true,
    });
    if row.encrypted_root {
        ebs["Encrypted"] = json!(true);
        ebs["KmsKeyId"] = json!({ "Ref": "VolumeKey" });
    }
    json!([{ "DeviceName": ROOT_DEVICE, "Ebs": ebs }])
}

/// The `LaunchTemplate` specification one row's group is declared with: the half it states
/// ([`Reference`]) and the version in the form it writes ([`VersionForm`]).
fn launch_template_specification(row: &Row) -> Value {
    let version = match row.version {
        VersionForm::LatestVersionAttribute => {
            json!({ "Fn::GetAtt": ["LaunchTemplate", "LatestVersionNumber"] })
        }
        VersionForm::Literal => json!(FIRST_VERSION),
    };
    match row.reference {
        Reference::ById => json!({
            "LaunchTemplateId": { "Ref": "LaunchTemplate" },
            "Version": version,
        }),
        Reference::ByName => json!({
            "LaunchTemplateName": row.launch_template_name,
            "Version": version,
        }),
    }
}

/// The estate one row deploys: the network the group launches into, the two security groups
/// and the instance profile its instances carry, the key its root volume is encrypted under,
/// the launch template that holds all of it, and the group itself.
fn template(row: &Row) -> Value {
    let mut key_properties = json!({ "Description": "the key the root volume is encrypted under" });
    if let Some(enabled) = row.declared_enabled {
        key_properties["Enabled"] = json!(enabled);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Vpc": {
                "Type": "AWS::EC2::VPC",
                "Properties": { "CidrBlock": VPC_CIDR },
            },
            "Subnet": {
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": { "Ref": "Vpc" },
                    "CidrBlock": SUBNET_CIDR,
                    "AvailabilityZone": ZONE,
                },
            },
            "AppSecurityGroup": {
                "Type": "AWS::EC2::SecurityGroup",
                "Properties": {
                    "GroupDescription": "the group holding the fleet instances",
                    "VpcId": { "Ref": "Vpc" },
                },
            },
            "DataSecurityGroup": {
                "Type": "AWS::EC2::SecurityGroup",
                "Properties": {
                    "GroupDescription": "the group reaching the data store",
                    "VpcId": { "Ref": "Vpc" },
                },
            },
            "InstanceRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "ec2.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                },
            },
            "InstanceProfile": {
                "Type": "AWS::IAM::InstanceProfile",
                "Properties": { "Roles": [{ "Ref": "InstanceRole" }] },
            },
            "VolumeKey": {
                "Type": "AWS::KMS::Key",
                "Properties": key_properties,
            },
            "LaunchTemplate": {
                "Type": "AWS::EC2::LaunchTemplate",
                "Properties": {
                    "LaunchTemplateName": row.launch_template_name,
                    "LaunchTemplateData": {
                        "ImageId": IMAGE_ID,
                        "InstanceType": INSTANCE_TYPE,
                        "IamInstanceProfile": { "Name": { "Ref": "InstanceProfile" } },
                        "SecurityGroupIds": [
                            { "Fn::GetAtt": ["AppSecurityGroup", "GroupId"] },
                            { "Fn::GetAtt": ["DataSecurityGroup", "GroupId"] },
                        ],
                        "BlockDeviceMappings": root_volume(row),
                    },
                },
            },
            "Fleet": {
                "Type": "AWS::AutoScaling::AutoScalingGroup",
                "Properties": {
                    "MinSize": row.sizes.0,
                    "MaxSize": row.sizes.1,
                    "DesiredCapacity": row.sizes.2,
                    "VPCZoneIdentifier": [{ "Ref": "Subnet" }],
                    "LaunchTemplate": launch_template_specification(row),
                },
            },
        },
        "Outputs": {
            "AutoScalingGroupName": { "Value": { "Ref": "Fleet" } },
            "LaunchTemplateId": { "Value": { "Ref": "LaunchTemplate" } },
            "LaunchTemplateName": { "Value": row.launch_template_name },
            "LaunchTemplateVersion": {
                "Value": { "Fn::GetAtt": ["LaunchTemplate", "LatestVersionNumber"] }
            },
            "KeyId": { "Value": { "Ref": "VolumeKey" } },
            "KeyArn": { "Value": { "Fn::GetAtt": ["VolumeKey", "Arn"] } },
        },
    })
}

/// Create a stack, poll it to a terminal status the way a client does, and answer its
/// `DescribeStacks` body.
async fn deploy(router: &MultiServiceRouter, case: &str, stack: &str, body: &Value) -> String {
    let body = body.to_string();
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &body),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
    );
    poll_to_terminal(router, case, stack).await
}

/// Read a stack until it reports a status no longer in progress, and answer that read's
/// body. The last read stands where the stack is still in progress after the bound, so the
/// caller's own assertion reports what it was waiting for.
async fn poll_to_terminal(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let mut described = String::new();
    for _ in 0..POLL_ATTEMPTS {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
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

/// The reason the stack's own events carry for every resource that failed, so a stack that
/// did not settle where a row expects says WHICH resource its owning service refused.
async fn failures(router: &MultiServiceRouter, stack: &str) -> String {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    super::query_wire::members_of(&events)
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

/// One output of the settled stack, asserted PRESENT and non-empty: an output a client
/// cannot read is a deployment it cannot operate on.
fn published(case: &str, described: &str, key: &str) -> String {
    let outputs = outputs_of(described);
    let value = outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no output named {key:?}, got {outputs:?}"));
    assert!(
        !value.is_empty(),
        "case {case:?}: expected the output {key:?} to carry a value, got {outputs:?}"
    );
    value
}

#[tokio::test]
async fn a_deployed_group_is_diagnosable_from_the_name_the_stack_published() {
    for row in ROWS {
        let case = row.case;
        let router = build_router();
        let described = deploy(&router, case, row.stack, &template(row)).await;
        let settled = opt_element(&described, "StackStatus").unwrap_or_default();
        if settled != "CREATE_COMPLETE" {
            panic!(
                "case {case:?}: expected the stack to settle at CREATE_COMPLETE, got \
                 {settled:?}; its failed resources: {}",
                failures(&router, row.stack).await
            );
        }

        let group_name = published(case, &described, "AutoScalingGroupName");
        let launch_template_id = published(case, &described, "LaunchTemplateId");
        let launch_template_name = published(case, &described, "LaunchTemplateName");
        let launch_template_version = published(case, &described, "LaunchTemplateVersion");
        let key_id = published(case, &described, "KeyId");
        let key_arn = published(case, &described, "KeyArn");
        // The physical id the stack REPORTS for each of those resources is the identifier its
        // owning service assigned, which is what the output carries: a client that reads the
        // stack's resources instead of its outputs reaches the same group, template and key.
        let (status, resources) =
            cloudformation(&router, "DescribeStackResources", &[("StackName", row.stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             body {resources}"
        );
        let reported_ids = physical_ids_of(&resources);
        for (logical_id, published_value) in [
            ("Fleet", &group_name),
            ("LaunchTemplate", &launch_template_id),
            ("VolumeKey", &key_id),
        ] {
            assert_eq!(
                reported_ids.get(logical_id),
                Some(published_value),
                "case {case:?}: expected the stack to report {published_value:?} as the physical \
                 id of {logical_id}, got {reported_ids:?}"
            );
        }
        assert_eq!(
            (
                launch_template_name.as_str(),
                launch_template_version.as_str()
            ),
            (row.launch_template_name, FIRST_VERSION),
            "case {case:?}: expected the template's declared name and its first version, got \
             {launch_template_name:?} at {launch_template_version:?}"
        );

        // Step one of the diagnosis: the name the stack published resolves to exactly ONE
        // group in Amazon EC2 Auto Scaling.
        let (status, groups) = autoscaling(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", &group_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeAutoScalingGroups({group_name}), got \
             {status} with body {groups}"
        );
        assert_eq!(
            elements(&groups, "AutoScalingGroupName"),
            vec![group_name.clone()],
            "case {case:?}: expected Auto Scaling to report exactly the group {group_name:?} \
             the stack reports created, got {groups}"
        );
        // ...carrying the sizes the declaration stated, which is what the diagnosis reads
        // first about a group that launched nothing.
        let reported_sizes = (
            opt_element(&groups, "MinSize").unwrap_or_default(),
            opt_element(&groups, "MaxSize").unwrap_or_default(),
            opt_element(&groups, "DesiredCapacity").unwrap_or_default(),
        );
        assert_eq!(
            (
                reported_sizes.0.as_str(),
                reported_sizes.1.as_str(),
                reported_sizes.2.as_str()
            ),
            row.sizes,
            "case {case:?}: expected the declared (MinSize, MaxSize, DesiredCapacity), got \
             {reported_sizes:?}"
        );
        // ...and a launch-template specification naming the stack's own template by BOTH
        // halves at the declared version, whichever half the declaration stated.
        let specification = block_of(&groups, "LaunchTemplate").unwrap_or_default();
        let reported_template = (
            opt_element(specification, "LaunchTemplateId").unwrap_or_default(),
            opt_element(specification, "LaunchTemplateName").unwrap_or_default(),
            opt_element(specification, "Version").unwrap_or_default(),
        );
        assert_eq!(
            (
                reported_template.0.as_str(),
                reported_template.1.as_str(),
                reported_template.2.as_str()
            ),
            (
                launch_template_id.as_str(),
                row.launch_template_name,
                FIRST_VERSION
            ),
            "case {case:?}: expected the group to launch from the stack's template at version \
             {FIRST_VERSION}, got {reported_template:?} in {groups}"
        );

        // Step two: that version of the launch template resolves in Amazon EC2, and its root
        // volume is the one the declaration stated — encrypted under the stack's own key
        // where the row declared it so.
        let (status, versions) = ec2(
            &router,
            "DescribeLaunchTemplateVersions",
            &[
                ("LaunchTemplateId", &launch_template_id),
                ("LaunchTemplateVersion.1", FIRST_VERSION),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeLaunchTemplateVersions\
             ({launch_template_id}), got {status} with body {versions}"
        );
        assert_eq!(
            elements(&versions, "versionNumber"),
            vec![FIRST_VERSION.to_string()],
            "case {case:?}: expected Amazon EC2 to report version {FIRST_VERSION} of the \
             stack's template, got {versions}"
        );
        let mappings = block_of(&versions, "blockDeviceMappingSet").unwrap_or_default();
        let mapping = items_of(mappings).first().copied().unwrap_or_default();
        assert_eq!(
            opt_element(mapping, "deviceName").as_deref(),
            Some(ROOT_DEVICE),
            "case {case:?}: expected the declared root device in the template's version, got \
             {versions}"
        );
        let volume = block_of(mapping, "ebs").unwrap_or_default();
        let reported_volume = (
            opt_element(volume, "volumeSize"),
            opt_element(volume, "volumeType"),
        );
        assert_eq!(
            reported_volume,
            (
                Some(ROOT_VOLUME_SIZE.to_string()),
                Some(ROOT_VOLUME_TYPE.to_string())
            ),
            "case {case:?}: expected the declared root volume's size and type, got \
             {reported_volume:?} in {versions}"
        );
        let reported_encryption = (
            opt_element(volume, "encrypted"),
            opt_element(volume, "kmsKeyId"),
        );
        let expected_encryption = if row.encrypted_root {
            (Some("true".to_string()), Some(key_id.clone()))
        } else {
            (None, None)
        };
        assert_eq!(
            reported_encryption, expected_encryption,
            "case {case:?}: expected the root volume's declared encryption, got \
             {reported_encryption:?} in {versions}"
        );

        // Step three: the key that volume is encrypted under resolves in KMS, in the state
        // the declaration asked for.
        let (status, answered) = kms(&router, "DescribeKey", &json!({ "KeyId": &key_id })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from KMS DescribeKey({key_id}), got {status} with \
             {answered}"
        );
        let metadata = answered
            .get("KeyMetadata")
            .cloned()
            .unwrap_or(Value::Null);
        let (expected_state, expected_enabled) = expected_key_state(row.declared_enabled);
        let reported_key = (
            metadata.get("KeyId").and_then(Value::as_str),
            metadata.get("Arn").and_then(Value::as_str),
            metadata.get("KeyState").and_then(Value::as_str),
            metadata.get("Enabled").and_then(Value::as_bool),
        );
        assert_eq!(
            reported_key,
            (
                Some(key_id.as_str()),
                Some(key_arn.as_str()),
                Some(expected_state),
                Some(expected_enabled)
            ),
            "case {case:?}: expected KMS to report the stack's own key {key_id:?} as \
             {expected_state}, got {answered}"
        );
    }
}
