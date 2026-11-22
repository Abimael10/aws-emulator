// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares `AWS::EFS::FileSystem`, `AWS::EFS::AccessPoint` and
//! `AWS::EFS::MountTarget` really creates each of them in Amazon EFS, and the handles the
//! stack publishes for them are the identifiers that service minted.
//!
//! The rule under test: **a resource of a type this emulator provisions is created in the
//! service that OWNS it, through that service's own public API; the identifier the stack
//! publishes — what `Ref`, the type's identifier attribute and
//! `DescribeStackResource.PhysicalResourceId` return — is the one that service ASSIGNED where
//! it assigns one; every attribute the type's "Return values" section publishes therefore
//! resolves, so every declared `Outputs` entry of a completed stack carries its
//! `OutputValue`; and a create that service REFUSES fails the resource with that refusal
//! rather than being reported as a resource nothing created.** For the three Amazon EFS types
//! those identifiers are the `fs-`, `fsap-` and `fsmt-` ids their creates mint: every member
//! of that service's API naming one is constrained to that form
//! (`FileSystemId` is
//! `^(arn:aws[-a-z]*:elasticfilesystem:[0-9a-z-:]+:file-system/fs-[0-9a-f]{8,40}|fs-[0-9a-f]{8,40})$`),
//! and none of the three types declares a name a rule could compute an identifier from.
//!
//! Three crates have to agree for that to hold — CloudFormation makes the calls, Amazon EFS
//! serves them, and Amazon EC2 owns the VPC, the subnet and the security group a mount target
//! is placed in — so the suite lives beside the router: a crate-local test can only ever see
//! one side of it.
//!
//! The rows differ along what the rule generalises over: how many of the three types one
//! stack declares, whether the template is written in JSON or YAML, whether the file system is
//! encrypted and which throughput mode it asks for, whether the mount target's address is
//! declared or left to Amazon EFS, and the Region the stack is deployed in. The refusal half
//! is two rows of the same rule: a throughput mode stated without the provisioned amount it
//! requires, and a second mount target in an Availability Zone the file system already has one
//! in — which CloudFormation reports on the resource as the provider's quoted refusal under the
//! `AlreadyExists` handler class.
//!
//! No expected value is read out of the answer being checked: each is either what the row's own
//! template declares, or what the OWNING service reports for the resource. The minted ids are
//! never written down — what is asserted about one is its FORM, that Amazon EFS resolves it,
//! and that it is not the `<stack>-<logical id>-<suffix>` name CloudFormation computes for a
//! resource whose own service names it.
//!
//! The awsQuery form encoder and the XML element readers are this test crate's own
//! ([`crate::query_wire`]), not another copy.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{element, elements, members_of, opt_element, outputs_of};

/// How long one poll waits before the next, and how many are made.
///
/// A stack's own status transitions and the `creating` -> `available` transition Amazon EFS
/// owes each resource are due on their clocks, so the suite waits for the CONDITION rather
/// than for a span, with a deadline generous enough for a loaded host.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLLS: usize = 1_200;

/// Amazon EFS's signing name, which is what routes a request to it (that model's `sigv4Name`
/// and `awsService.endpointPrefix`).
const EFS_SIGNING_NAME: &str = "elasticfilesystem";

/// The URI prefix every operation of that service is bound under: its API version.
const EFS_PREFIX: &str = "/2015-02-01";

/// The sentence Amazon EFS refuses a mount target in an Availability Zone the file system
/// already has one in with — "there can be only one mount target per Availability Zone" —
/// which is what CloudFormation quotes on the failed resource.
const ZONE_CONFLICT: &str = "mount target already exists in this AZ";

/// The `HandlerErrorCode` class that refusal is reported under: a mount target ALREADY EXISTS
/// in the zone the declaration asks for one in.
const ALREADY_EXISTS: &str = "AlreadyExists";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
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
        service_com_amazonaws_efs_magnolioapiservice_v20150201::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which
/// is what routes the request and what each handler reads its Region from.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&super::query_wire::form_encode(key));
        body.push('=');
        body.push_str(&super::query_wire::form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one Amazon EFS operation over the `restJson1` binding its model declares, signed for
/// `region`.
async fn efs(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://{EFS_SIGNING_NAME}.{region}.amazonaws.com{path}"
        ))
        .header("host", format!("{EFS_SIGNING_NAME}.{region}.amazonaws.com"))
        .header("authorization", authorization(EFS_SIGNING_NAME, region));
    let payload = match &body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Bytes::from(serde_json::to_vec(value).expect("a serializable body"))
        }
        None => Bytes::new(),
    };
    let request = builder.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, answered)
}

/// Wait for `stack` to report `wanted`, looking again until it does or the deadline passes. A
/// stack that reaches another TERMINAL status fails the suite at once, quoting the answer: the
/// reason a resource failed is in it.
async fn wait_for_stack_status(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
    wanted: &str,
) -> String {
    let mut body = String::new();
    for _ in 0..POLLS {
        let (status, reported) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {reported}"
        );
        body = reported;
        let reached = opt_element(&body, "StackStatus").unwrap_or_default();
        if reached == wanted {
            return body;
        }
        assert!(
            reached.ends_with("_IN_PROGRESS"),
            "stack {stack:?}: expected {wanted:?}, got {reached:?} with answer {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("stack {stack:?} never reached {wanted:?}; last answer was {body}");
}

/// Wait for `stack` to reach any TERMINAL status, which is what a deployment that fails ends
/// in, and answer that status with the answer it was read from.
async fn wait_for_terminal_status(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
) -> (String, String) {
    let mut body = String::new();
    for _ in 0..POLLS {
        let (status, reported) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {reported}"
        );
        body = reported;
        let reached = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reached.ends_with("_IN_PROGRESS") && !reached.is_empty() {
            return (reached, body);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("stack {stack:?} never reached a terminal status; last answer was {body}");
}

/// Wait until `stack` is gone, which is what a `DeleteStack` owes.
async fn wait_until_stack_is_gone(router: &MultiServiceRouter, region: &str, stack: &str) {
    let mut body = String::new();
    for _ in 0..POLLS {
        let (status, reported) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        if status == 400 {
            return;
        }
        body = reported;
        let reached = opt_element(&body, "StackStatus").unwrap_or_default();
        assert_ne!(
            reached, "DELETE_FAILED",
            "stack {stack:?}: expected its deletion to go through, got {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("stack {stack:?} was never deleted; last answer was {body}");
}

/// One output of a `DescribeStacks` answer, or a panic naming the case and what was there —
/// which is what a missing `OutputValue` reads as, since the map is built from the pairs that
/// carry both members.
fn output(outputs: &BTreeMap<String, String>, key: &str, case: &str) -> String {
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an output {key:?}, got {outputs:?}"))
}

/// The physical resource id `DescribeStackResource` reports for `logical_id`.
async fn physical_resource_id(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        region,
        "DescribeStackResource",
        &[("StackName", stack), ("LogicalResourceId", logical_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeStackResource {logical_id:?}: expected HTTP 200, got {status} with body {body}"
    );
    element(&body, "PhysicalResourceId")
}

/// The physical resource id the stack reports for each of its resources, as
/// `DescribeStackResources` answers them.
async fn physical_resource_ids(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
) -> BTreeMap<String, String> {
    let (status, body) = cloudformation(
        router,
        region,
        "DescribeStackResources",
        &[("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeStackResources {stack:?}: expected HTTP 200, got {status} with body {body}"
    );
    super::query_wire::physical_ids_of(&body)
}

/// Whether `id` has the shape of an identifier Amazon EFS mints: the type's own prefix, then
/// 8 to 40 lowercase hexadecimal digits — the form every member of that service's API naming
/// one of its resources is constrained to.
fn is_minted_id(prefix: &str, id: &str) -> bool {
    let Some(digits) = id.strip_prefix(prefix) else {
        return false;
    };
    (8..=40).contains(&digits.chars().count())
        && digits
            .chars()
            .all(|digit| digit.is_ascii_digit() || ('a'..='f').contains(&digit))
}

/// The one file system Amazon EFS reports for `file_system_id`, through its own read.
async fn described_file_system(
    router: &MultiServiceRouter,
    region: &str,
    file_system_id: &str,
    case: &str,
) -> Value {
    let (status, answered) = efs(
        router,
        region,
        "GET",
        &format!("{EFS_PREFIX}/file-systems?FileSystemId={file_system_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeFileSystems on the id the stack \
         published ({file_system_id}), got {status} with body {answered}"
    );
    answered["FileSystems"][0].clone()
}

/// The access points Amazon EFS reports for `access_point_id`.
async fn described_access_point(
    router: &MultiServiceRouter,
    region: &str,
    access_point_id: &str,
    case: &str,
) -> Value {
    let (status, answered) = efs(
        router,
        region,
        "GET",
        &format!("{EFS_PREFIX}/access-points?AccessPointId={access_point_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeAccessPoints on the id the stack \
         published ({access_point_id}), got {status} with body {answered}"
    );
    answered["AccessPoints"][0].clone()
}

/// The mount targets Amazon EFS reports for the file system `file_system_id`.
async fn described_mount_targets(
    router: &MultiServiceRouter,
    region: &str,
    file_system_id: &str,
    case: &str,
) -> Vec<Value> {
    let (status, answered) = efs(
        router,
        region,
        "GET",
        &format!("{EFS_PREFIX}/mount-targets?FileSystemId={file_system_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeMountTargets, got {status} with body \
         {answered}"
    );
    answered["MountTargets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Every file system the account holds in `region`, which is what says a refused create left
/// nothing behind.
async fn all_file_systems(router: &MultiServiceRouter, region: &str) -> Vec<Value> {
    let (status, answered) =
        efs(router, region, "GET", &format!("{EFS_PREFIX}/file-systems"), None).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from DescribeFileSystems, got {status} with body {answered}"
    );
    answered["FileSystems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The `ResourceStatusReason` of the event `logical_id` reached `reported` in, looked for
/// again until the stack has recorded it or the polls run out.
async fn failure_reason(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
    logical_id: &str,
    reported: &str,
) -> String {
    for _ in 0..POLLS {
        let (status, body) =
            cloudformation(router, region, "DescribeStackEvents", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStackEvents {stack:?}: expected HTTP 200, got {status} with body {body}"
        );
        let found = members_of(&body).into_iter().find_map(|member| {
            let named = elements(member, "LogicalResourceId")
                .into_iter()
                .next()
                .is_some_and(|named| named == logical_id);
            let at = elements(member, "ResourceStatus")
                .into_iter()
                .next()
                .is_some_and(|status| status == reported);
            (named && at)
                .then(|| elements(member, "ResourceStatusReason").into_iter().next())
                .flatten()
                .filter(|reason| !reason.is_empty())
        });
        if let Some(reason) = found {
            return reason;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("stack {stack:?}: {logical_id} recorded no {reported} reason");
}

/// Every `ResourceStatusReason` the stack recorded for an event of `logical_id` at
/// `reported`, which is what says a refused resource is reported ONCE.
async fn failure_reasons(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
    logical_id: &str,
    reported: &str,
) -> Vec<String> {
    let (status, body) =
        cloudformation(router, region, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "DescribeStackEvents {stack:?}: expected HTTP 200, got {status} with body {body}"
    );
    members_of(&body)
        .into_iter()
        .filter(|member| {
            elements(member, "LogicalResourceId")
                .into_iter()
                .next()
                .is_some_and(|named| named == logical_id)
                && elements(member, "ResourceStatus")
                    .into_iter()
                    .next()
                    .is_some_and(|status| status == reported)
        })
        .filter_map(|member| elements(member, "ResourceStatusReason").into_iter().next())
        .collect()
}

/// The provider exception a `ResourceStatusReason` quotes, and the `HandlerErrorCode` class it
/// reports beside it — the one envelope CloudFormation reports a resource provider's answer in.
fn quoted_and_class(case: &str, reason: &str) -> (String, String) {
    const OPENS: &str = "Resource handler returned message: \"";
    const TOKEN: &str = "\" (RequestToken: ";
    const CLASS: &str = "HandlerErrorCode: ";
    let rest = reason.strip_prefix(OPENS).unwrap_or_else(|| {
        panic!("case {case:?}: expected the reason to open with {OPENS:?}, got {reason:?}")
    });
    let (quoted, tail) = rest.split_once(TOKEN).unwrap_or_else(|| {
        panic!("case {case:?}: expected a quoted exception and a RequestToken, got {reason:?}")
    });
    let class = tail
        .split_once(CLASS)
        .and_then(|(_, class)| class.strip_suffix(')'))
        .unwrap_or_else(|| panic!("case {case:?}: expected a HandlerErrorCode, got {reason:?}"));
    (quoted.to_string(), class.to_string())
}

/// What the rule says Amazon EFS must hold for one row's estate.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the estate is created in.
    region: &'static str,
    /// The stack name the row deploys under.
    stack: &'static str,
    /// The template body, in the language the row writes it in.
    template: &'static str,
    /// What the file system's declaration states, as Amazon EFS reports it back.
    encrypted: bool,
    performance_mode: &'static str,
    throughput_mode: &'static str,
    /// The `Name` tag the declaration's own tag property states, which is what that service
    /// reports as the file system's `Name`.
    name_tag: &'static str,
    /// Whether the row declares an access point and a mount target beside the file system.
    declares_access_point: bool,
    declares_mount_target: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a file system alone, encrypted and bursting, in JSON",
        region: "us-east-1",
        stack: "efs-file-system-stack",
        template: r#"{
          "AWSTemplateFormatVersion": "2010-09-09",
          "Resources": {
            "Fs": {
              "Type": "AWS::EFS::FileSystem",
              "Properties": {
                "Encrypted": true,
                "PerformanceMode": "generalPurpose",
                "ThroughputMode": "bursting",
                "FileSystemTags": [{"Key": "Name", "Value": "shared-state"}]
              }
            }
          },
          "Outputs": {
            "FsRef": {"Value": {"Ref": "Fs"}},
            "FsId": {"Value": {"Fn::GetAtt": ["Fs", "FileSystemId"]}},
            "FsArn": {"Value": {"Fn::GetAtt": ["Fs", "Arn"]}}
          }
        }"#,
        encrypted: true,
        performance_mode: "generalPurpose",
        throughput_mode: "bursting",
        name_tag: "shared-state",
        declares_access_point: false,
        declares_mount_target: false,
    },
    Row {
        case: "the whole estate — a VPC, a subnet, a security group, an unencrypted \
               provisioned file system, an access point and a mount target — in YAML",
        region: "eu-west-1",
        stack: "efs-estate-stack",
        template: concat!(
            "Resources:\n",
            "  Vpc:\n",
            "    Type: AWS::EC2::VPC\n",
            "    Properties:\n",
            "      CidrBlock: 10.40.0.0/16\n",
            "  Subnet:\n",
            "    Type: AWS::EC2::Subnet\n",
            "    Properties:\n",
            "      VpcId: !Ref Vpc\n",
            "      CidrBlock: 10.40.1.0/24\n",
            "      AvailabilityZone: eu-west-1a\n",
            "  Sg:\n",
            "    Type: AWS::EC2::SecurityGroup\n",
            "    Properties:\n",
            "      GroupDescription: the group the mount target answers in\n",
            "      VpcId: !Ref Vpc\n",
            "  Fs:\n",
            "    Type: AWS::EFS::FileSystem\n",
            "    Properties:\n",
            "      Encrypted: false\n",
            "      PerformanceMode: generalPurpose\n",
            "      ThroughputMode: provisioned\n",
            "      ProvisionedThroughputInMibps: 64\n",
            "      FileSystemTags:\n",
            "        - Key: Name\n",
            "          Value: app-state\n",
            "  Ap:\n",
            "    Type: AWS::EFS::AccessPoint\n",
            "    Properties:\n",
            "      FileSystemId: !Ref Fs\n",
            "      PosixUser:\n",
            "        Uid: \"1001\"\n",
            "        Gid: \"1001\"\n",
            "      RootDirectory:\n",
            "        Path: /app\n",
            "        CreationInfo:\n",
            "          OwnerUid: \"1001\"\n",
            "          OwnerGid: \"1001\"\n",
            "          Permissions: \"0755\"\n",
            "      AccessPointTags:\n",
            "        - Key: Name\n",
            "          Value: app-access\n",
            "  Mt:\n",
            "    Type: AWS::EFS::MountTarget\n",
            "    Properties:\n",
            "      FileSystemId: !Ref Fs\n",
            "      SubnetId: !Ref Subnet\n",
            "      SecurityGroups:\n",
            "        - !Ref Sg\n",
            "Outputs:\n",
            "  FsRef:\n",
            "    Value: !Ref Fs\n",
            "  FsId:\n",
            "    Value: !GetAtt Fs.FileSystemId\n",
            "  FsArn:\n",
            "    Value: !GetAtt Fs.Arn\n",
            "  ApRef:\n",
            "    Value: !Ref Ap\n",
            "  ApId:\n",
            "    Value: !GetAtt Ap.AccessPointId\n",
            "  ApArn:\n",
            "    Value: !GetAtt Ap.Arn\n",
            "  MtRef:\n",
            "    Value: !Ref Mt\n",
            "  MtId:\n",
            "    Value: !GetAtt Mt.Id\n",
            "  MtIp:\n",
            "    Value: !GetAtt Mt.IpAddress\n",
        ),
        encrypted: false,
        performance_mode: "generalPurpose",
        throughput_mode: "provisioned",
        name_tag: "app-state",
        declares_access_point: true,
        declares_mount_target: true,
    },
];

/// Every row: each EFS resource the template declares is created in Amazon EFS, the handle the
/// stack publishes for it is the identifier that service minted, every declared output carries
/// its value, and deleting the stack releases the resources there.
#[tokio::test]
async fn a_stack_provisions_the_efs_estate_its_template_declares() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let region = row.region;
        let stack = row.stack;

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", stack), ("TemplateBody", row.template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let described = wait_for_stack_status(&router, region, stack, "CREATE_COMPLETE").await;
        let outputs = outputs_of(&described);

        // Every declared output is reported WITH its value: an entry carrying a key and no
        // value is what a client reading `o['OutputValue']` breaks on.
        let declared_outputs: &[&str] = if row.declares_mount_target {
            &[
                "FsRef", "FsId", "FsArn", "ApRef", "ApId", "ApArn", "MtRef", "MtId", "MtIp",
            ]
        } else {
            &["FsRef", "FsId", "FsArn"]
        };
        for key in declared_outputs {
            let value = output(&outputs, key, case);
            assert!(
                !value.is_empty(),
                "case {case:?}: expected the output {key:?} to carry a value, got {value:?}"
            );
        }

        // THE FILE SYSTEM: the published handle is the `fs-` id Amazon EFS minted — not the
        // `<stack>-<logical id>-<suffix>` name CloudFormation computes for a resource whose own
        // service does not name it — and it is the same handle `Ref`, the type's identifier
        // attribute and the per-resource read all report.
        let file_system_id = output(&outputs, "FsRef", case);
        assert!(
            is_minted_id("fs-", &file_system_id),
            "case {case:?}: expected the file system's Ref to be the fs- id Amazon EFS minted, \
             got {file_system_id:?}"
        );
        assert!(
            !file_system_id.contains(stack),
            "case {case:?}: expected the published handle to be the minted id rather than a \
             name built from the stack, got {file_system_id:?}"
        );
        assert_eq!(
            output(&outputs, "FsId", case),
            file_system_id,
            "case {case:?}: expected Ref and FileSystemId to be the one handle"
        );
        assert_eq!(
            physical_resource_id(&router, region, stack, "Fs").await,
            file_system_id,
            "case {case:?}: expected the physical resource id to be the minted id"
        );

        // Amazon EFS itself resolves that handle, and reports the declaration back.
        let reported = described_file_system(&router, region, &file_system_id, case).await;
        for (member, expected) in [
            ("FileSystemId", json!(file_system_id.clone())),
            ("Encrypted", json!(row.encrypted)),
            ("PerformanceMode", json!(row.performance_mode)),
            ("ThroughputMode", json!(row.throughput_mode)),
            ("Name", json!(row.name_tag)),
            ("LifeCycleState", json!("available")),
        ] {
            assert_eq!(
                reported[member], expected,
                "case {case:?}: expected the file system's {member} to be {expected}, got {:?}",
                reported[member]
            );
        }
        assert_eq!(
            output(&outputs, "FsArn", case),
            reported["FileSystemArn"].as_str().unwrap_or_default(),
            "case {case:?}: expected the Arn output to be the ARN Amazon EFS reports, got {:?}",
            reported["FileSystemArn"]
        );

        // THE ACCESS POINT, where the row declares one: the same rule one resource along.
        if row.declares_access_point {
            let access_point_id = output(&outputs, "ApRef", case);
            assert!(
                is_minted_id("fsap-", &access_point_id),
                "case {case:?}: expected the access point's Ref to be the fsap- id Amazon EFS \
                 minted, got {access_point_id:?}"
            );
            assert_eq!(
                output(&outputs, "ApId", case),
                access_point_id,
                "case {case:?}: expected Ref and AccessPointId to be the one handle"
            );
            let reported =
                described_access_point(&router, region, &access_point_id, case).await;
            assert_eq!(
                reported["FileSystemId"],
                json!(file_system_id.clone()),
                "case {case:?}: expected the access point to be on the declared file system, \
                 got {:?}",
                reported["FileSystemId"]
            );
            assert_eq!(
                reported["RootDirectory"]["Path"],
                json!("/app"),
                "case {case:?}: expected the declared root directory, got {:?}",
                reported["RootDirectory"]
            );
            // The POSIX identifiers the type publishes as STRINGS reach Amazon EFS as the
            // numbers its own model types them as, at every depth the declaration nests them,
            // and the identity is reported with the secondary group list it holds.
            assert_eq!(
                reported["PosixUser"],
                json!({"Uid": 1001, "Gid": 1001, "SecondaryGids": []}),
                "case {case:?}: expected the declared POSIX user as the numbers that service \
                 types them as, got {:?}",
                reported["PosixUser"]
            );
            assert_eq!(
                reported["RootDirectory"]["CreationInfo"],
                json!({"OwnerUid": 1001, "OwnerGid": 1001, "Permissions": "0755"}),
                "case {case:?}: expected the declared creation info, got {:?}",
                reported["RootDirectory"]["CreationInfo"]
            );
            assert_eq!(
                output(&outputs, "ApArn", case),
                reported["AccessPointArn"].as_str().unwrap_or_default(),
                "case {case:?}: expected the Arn output to be the ARN Amazon EFS reports, got \
                 {:?}",
                reported["AccessPointArn"]
            );
        }

        // THE MOUNT TARGET: its id is the `fsmt-` one that service minted, and the address the
        // stack publishes is the one Amazon EFS chose inside the declared subnet.
        if row.declares_mount_target {
            let mount_target_id = output(&outputs, "MtRef", case);
            assert!(
                is_minted_id("fsmt-", &mount_target_id),
                "case {case:?}: expected the mount target's Ref to be the fsmt- id Amazon EFS \
                 minted, got {mount_target_id:?}"
            );
            assert_eq!(
                output(&outputs, "MtId", case),
                mount_target_id,
                "case {case:?}: expected Ref and Id to be the one handle"
            );
            let reported = described_mount_targets(&router, region, &file_system_id, case).await;
            assert_eq!(
                reported.len(),
                1,
                "case {case:?}: expected the one declared mount target, got {reported:?}"
            );
            assert_eq!(
                reported[0]["MountTargetId"],
                json!(mount_target_id.clone()),
                "case {case:?}: expected the minted id, got {:?}",
                reported[0]["MountTargetId"]
            );
            assert_eq!(
                output(&outputs, "MtIp", case),
                reported[0]["IpAddress"].as_str().unwrap_or_default(),
                "case {case:?}: expected the IpAddress output to be the address Amazon EFS \
                 assigned, got {:?}",
                reported[0]["IpAddress"]
            );
        }

        // Deleting the stack releases the estate in Amazon EFS: the handle it published no
        // longer resolves there.
        let (status, body) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        wait_until_stack_is_gone(&router, region, stack).await;
        let (status, answered) = efs(
            &router,
            region,
            "GET",
            &format!("{EFS_PREFIX}/file-systems?FileSystemId={file_system_id}"),
            None,
        )
        .await;
        assert_eq!(
            (status, answered["ErrorCode"].as_str()),
            (404, Some("FileSystemNotFound")),
            "case {case:?}: expected Amazon EFS to hold no file system once the stack is \
             deleted, got {status} with body {answered}"
        );
    }
}

/// A declaration Amazon EFS REFUSES fails the resource with that service's own sentence, and
/// nothing is left behind: `ThroughputMode: provisioned` without the amount that mode requires
/// is a request that service will not carry out.
#[tokio::test]
async fn a_declaration_amazon_efs_refuses_leaves_no_file_system_behind() {
    let router = build_router();
    let region = "us-west-2";
    let stack = "efs-provisioned-throughput-missing";
    let case = "a provisioned throughput mode with no amount";
    let template = r#"{
      "Resources": {
        "Fs": {
          "Type": "AWS::EFS::FileSystem",
          "Properties": {"Encrypted": true, "ThroughputMode": "provisioned"}
        }
      },
      "Outputs": {"FsRef": {"Value": {"Ref": "Fs"}}}
    }"#;

    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );

    let (reached, body) = wait_for_terminal_status(&router, region, stack).await;
    assert_eq!(
        reached, "ROLLBACK_COMPLETE",
        "case {case:?}: expected the refused create to roll the stack back, got {reached:?} \
         with answer {body}"
    );
    let reason = failure_reason(&router, region, stack, "Fs", "CREATE_FAILED").await;
    let (quoted, class) = quoted_and_class(case, &reason);
    // What the provider reports is the exception its own SDK raised: that service's sentence,
    // then the SDK's diagnostic of the refused call. The ids and the attempt count inside the
    // diagnostic are the call's own, so only the fixed parts are asserted.
    let sentence = "Provisioned throughput must be set for file systems that use provisioned \
                    throughput mode.";
    assert!(
        quoted.starts_with(sentence),
        "case {case:?}: expected the quoted refusal to open with Amazon EFS's own sentence for \
         that mode, got {quoted:?}"
    );
    assert!(
        quoted.contains("(Service: Efs, Status Code: 400, Request ID: ")
            && quoted.contains("(SDK Attempt Count: "),
        "case {case:?}: expected that service's SDK diagnostic after the sentence, got \
         {quoted:?}"
    );
    assert_eq!(
        class, "InvalidRequest",
        "case {case:?}: expected the class a refused REQUEST falls in, got {class:?}"
    );
    let held = all_file_systems(&router, region).await;
    assert!(
        held.is_empty(),
        "case {case:?}: expected a refused create to leave no file system behind, got {held:?}"
    );
}

/// A second mount target in an Availability Zone the file system already has one in fails the
/// stack: Amazon EFS refuses the create, and CloudFormation reports that refusal on the
/// resource under the class a resource that ALREADY EXISTS falls in — with only the first
/// mount target left in that service.
#[tokio::test]
async fn a_second_mount_target_in_one_zone_fails_the_stack() {
    let router = build_router();
    let region = "us-east-2";
    let stack = "efs-two-mount-targets-one-zone";
    let case = "two subnets of one Availability Zone";
    let template = concat!(
        "Resources:\n",
        "  Vpc:\n",
        "    Type: AWS::EC2::VPC\n",
        "    Properties:\n",
        "      CidrBlock: 10.60.0.0/16\n",
        "  SubnetOne:\n",
        "    Type: AWS::EC2::Subnet\n",
        "    Properties:\n",
        "      VpcId: !Ref Vpc\n",
        "      CidrBlock: 10.60.1.0/24\n",
        "      AvailabilityZone: us-east-2a\n",
        "  SubnetTwo:\n",
        "    Type: AWS::EC2::Subnet\n",
        "    Properties:\n",
        "      VpcId: !Ref Vpc\n",
        "      CidrBlock: 10.60.2.0/24\n",
        "      AvailabilityZone: us-east-2a\n",
        "  Fs:\n",
        "    Type: AWS::EFS::FileSystem\n",
        "    Properties:\n",
        "      Encrypted: true\n",
        "  MtOne:\n",
        "    Type: AWS::EFS::MountTarget\n",
        "    Properties:\n",
        "      FileSystemId: !Ref Fs\n",
        "      SubnetId: !Ref SubnetOne\n",
        "  MtTwo:\n",
        "    DependsOn: MtOne\n",
        "    Type: AWS::EFS::MountTarget\n",
        "    Properties:\n",
        "      FileSystemId: !Ref Fs\n",
        "      SubnetId: !Ref SubnetTwo\n",
        "Outputs:\n",
        "  FsRef:\n",
        "    Value: !Ref Fs\n",
    );

    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            ("DisableRollback", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );

    let (reached, body) = wait_for_terminal_status(&router, region, stack).await;
    assert_eq!(
        reached, "CREATE_FAILED",
        "case {case:?}: expected the stack to end CREATE_FAILED, got {reached:?} with answer \
         {body}"
    );

    // The refusal is reported on the resource that asked for it, in the one envelope
    // CloudFormation reports a provider's answer in: Amazon EFS's own sentence, and the class
    // that says the mount target already exists where the declaration asks for one.
    let reason = failure_reason(&router, region, stack, "MtTwo", "CREATE_FAILED").await;
    let (quoted, class) = quoted_and_class(case, &reason);
    assert_eq!(
        (quoted.as_str(), class.as_str()),
        (ZONE_CONFLICT, ALREADY_EXISTS),
        "case {case:?}: expected the zone refusal under the already-exists class, got {reason:?}"
    );
    // Reported ONCE: a resource whose create one service refused failed once, however many
    // passes the deployment took to reach it.
    let rows = failure_reasons(&router, region, stack, "MtTwo", "CREATE_FAILED").await;
    assert_eq!(
        rows.len(),
        1,
        "case {case:?}: expected one CREATE_FAILED row for MtTwo, got {rows:?}"
    );

    // And Amazon EFS holds the FIRST mount target and no second one: the refused create
    // stored nothing. The file system is read from that service's own inventory, which is
    // where the estate this stack deployed really lives.
    let held = all_file_systems(&router, region).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected the one file system the stack created, got {held:?}"
    );
    let file_system_id = held[0]["FileSystemId"].as_str().unwrap_or_default().to_string();
    // A create that failed is not a create that did nothing: the stack whose failed state the
    // caller asked to keep goes on reporting every resource it created, each under the
    // identifier its owning service minted — including the ones created before the pass that
    // failed.
    let reported = physical_resource_ids(&router, region, stack).await;
    for (logical_id, prefix) in [("Fs", "fs-"), ("MtOne", "fsmt-")] {
        let id = reported.get(logical_id).cloned().unwrap_or_default();
        assert!(
            is_minted_id(prefix, &id),
            "case {case:?}: expected the failed stack to report {logical_id} under the id \
             Amazon EFS minted, got {reported:?}"
        );
    }
    assert_eq!(
        reported.get("Fs").cloned().unwrap_or_default(),
        file_system_id,
        "case {case:?}: expected the reported file system to be the one Amazon EFS holds, got \
         {reported:?}"
    );
    let mount_targets = described_mount_targets(&router, region, &file_system_id, case).await;
    assert_eq!(
        mount_targets.len(),
        1,
        "case {case:?}: expected the one mount target that was accepted, got {mount_targets:?}"
    );
    assert_eq!(
        mount_targets[0]["MountTargetId"],
        json!(reported.get("MtOne").cloned().unwrap_or_default()),
        "case {case:?}: expected the mount target the stack created first, got {:?}",
        mount_targets[0]["MountTargetId"]
    );
    assert_eq!(
        mount_targets[0]["AvailabilityZoneName"],
        json!("us-east-2a"),
        "case {case:?}: expected the accepted mount target to be in the zone both subnets \
         declare, got {:?}",
        mount_targets[0]["AvailabilityZoneName"]
    );
}
