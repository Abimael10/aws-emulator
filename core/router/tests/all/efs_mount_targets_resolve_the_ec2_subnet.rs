// SPDX-License-Identifier: CC0-1.0

//! Amazon EFS places a mount target in the SUBNET the request names, as the service that
//! OWNS subnets reports it.
//!
//! RULE: a subnet, its VPC, its Availability Zone and its address range are Amazon EC2's
//! facts, so `CreateMountTarget` resolves the `SubnetId` against the account's EC2
//! inventory and reports those facts back: the `IpAddress` the reply and every later
//! `DescribeMountTargets` carry is an address inside the `CidrBlock` EC2 reports for that
//! subnet ("the IP address range from which Amazon EFS selects the IP address of the mount
//! target"), with `VpcId`, `AvailabilityZoneName` and `AvailabilityZoneId` read off the same
//! record; a `SubnetId` EC2 holds no subnet for is refused with the modelled
//! `SubnetNotFound` (HTTP 400) and stores nothing.
//!
//! The refusals the same slice answers are asserted here too, because each one needs a
//! mount target to exist first: the per-Availability-Zone `MountTargetConflict` (HTTP 409)
//! carries AWS's fixed sentence with no value of the request or of state in it, a
//! `DeleteFileSystem` of a file system that has mount targets is the modelled
//! `FileSystemInUse` (HTTP 409) naming only the requested file system, and
//! `ModifyMountTargetSecurityGroups` replaces the set in effect (HTTP 204) while a mount
//! target the account no longer has is the modelled `MountTargetNotFound` (HTTP 404).
//!
//! This suite lives beside the router because the subnets are EC2's: every subnet below is
//! one EC2 minted — the default VPC's own, and subnets a client created in a VPC of its own
//! — on the SAME engine that serves EFS, so every address asserted is compared against the
//! range EC2 really reports rather than against a value written out here.
//!
//! The rows differ along what the rule generalises over: the VPC the subnet belongs to
//! (the account's default one and a client-created one), the SIZE of its block (`/20`,
//! `/24`, `/28`), the block's own range, and the Availability Zone it sits in.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// EFS's signing name, which is what routes a request to it.
const EFS_SIGNING_NAME: &str = "elasticfilesystem";

/// The CIDR block of the VPC this suite creates for the rows that do not use the account's
/// default one. Well away from the default VPC's `172.31.0.0/16`.
const OWN_VPC_CIDR: &str = "10.80.0.0/16";

/// A generous deadline for the clock-owed `creating` -> `available` transition of a file
/// system, and how often it is looked for. Nothing waits for a span: the loop looks again
/// until the condition holds or the deadline passes, so a loaded and a fast machine reach
/// the same verdict.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One HTTP answer, reduced to what the assertions read.
struct Reply {
    status: u16,
    error_type: Option<String>,
    body: Value,
    body_len: usize,
}

impl Reply {
    fn string(&self, key: &str) -> String {
        self.body[key].as_str().unwrap_or_default().to_string()
    }
}

/// One subnet of the account, exactly as EC2 reports it.
#[derive(Debug, Clone)]
struct SubnetFacts {
    case: &'static str,
    subnet_id: String,
    vpc_id: String,
    zone_name: String,
    zone_id: String,
    cidr_block: String,
}

/// The emulator: Amazon EC2 and Amazon EFS over ONE shared engine, as the production
/// router mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_efs_magnolioapiservice_v20150201::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header scoped to `signing_name` and [`REGION`] — what the router
/// routes on and what the handlers read the Region of a request from.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0123456789abcdef"
    )
}

/// One ec2Query form body: the action, the version and the parameters.
fn form(action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// Drive one ec2Query action that must succeed, and answer its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://ec2.{REGION}.amazonaws.com/"))
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(form(action, params)))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One EFS call over its restJson1 binding.
async fn efs(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Reply {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://{EFS_SIGNING_NAME}.{REGION}.amazonaws.com{path}"
        ))
        .header("host", format!("{EFS_SIGNING_NAME}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(EFS_SIGNING_NAME));
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
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let answered = response.into_body();
    Reply {
        status,
        error_type,
        body: serde_json::from_slice(&answered).unwrap_or(Value::Null),
        body_len: answered.len(),
    }
}

/// Whether `address` lies inside the IPv4 CIDR block `cidr_block`.
///
/// Written out here rather than taken from a crate for one reason, and it is not
/// `Cargo.lock`: `core/router/Cargo.toml` is a pinned path in this repository, so no
/// dev-dependency can be added to this test crate at all (see `tests/all/query_wire.rs`,
/// which records the same constraint for its form encoder). The service crate under test
/// takes the maintained crate instead — `ipnet` in
/// `services/service-com-amazonaws-efs-magnolioapiservice-v20150201/Cargo.toml`.
fn within(cidr_block: &str, address: &str) -> bool {
    let Some((network, prefix_length)) = cidr_block.split_once('/') else {
        return false;
    };
    let Ok(prefix_length) = prefix_length.parse::<u32>() else {
        return false;
    };
    let (Some(network), Some(address)) = (packed(network), packed(address)) else {
        return false;
    };
    if prefix_length > 32 {
        return false;
    }
    // A /0 block holds every address; shifting by 32 is undefined, so it is its own case.
    let mask = if prefix_length == 0 {
        0
    } else {
        u32::MAX << (32 - prefix_length)
    };
    network & mask == address & mask
}

/// A dotted-quad IPv4 address as the 32-bit number it names, or `None` when it is not one.
fn packed(address: &str) -> Option<u32> {
    let octets: Vec<u32> = address
        .split('.')
        .map(|octet| octet.parse::<u32>().ok())
        .collect::<Option<Vec<u32>>>()?;
    if octets.len() != 4 || octets.iter().any(|octet| *octet > 255) {
        return None;
    }
    Some(octets.iter().fold(0, |packed, octet| (packed << 8) | octet))
}

/// The subnet `subnet_id` names, as EC2's own `DescribeSubnets` reports it.
///
/// Asked by id, so the answer carries exactly that one subnet and each member read off it is
/// its own.
async fn subnet_facts(
    router: &MultiServiceRouter,
    case: &'static str,
    subnet_id: &str,
) -> SubnetFacts {
    let described = ec2(router, "DescribeSubnets", &[("SubnetId.1", subnet_id)]).await;
    let facts = SubnetFacts {
        case,
        subnet_id: element(&described, "subnetId"),
        vpc_id: element(&described, "vpcId"),
        zone_name: element(&described, "availabilityZone"),
        zone_id: element(&described, "availabilityZoneId"),
        cidr_block: element(&described, "cidrBlock"),
    };
    assert_eq!(
        facts.subnet_id, subnet_id,
        "setup: EC2 must report the subnet {subnet_id} in {described}"
    );
    facts
}

/// The subnets every row below is placed in: the account's own default subnet, and three a
/// client created in a VPC of its own, differing in block size, range and zone.
async fn rows(router: &MultiServiceRouter) -> Vec<SubnetFacts> {
    let defaults = ec2(router, "DescribeSubnets", &[]).await;
    let default_subnet = elements(&defaults, "subnetId")
        .into_iter()
        .next()
        .expect("setup: the account must have a default subnet");

    let vpc = own_vpc(router).await;
    let mut facts = vec![
        subnet_facts(router, "a default subnet of the account's default VPC", &default_subnet)
            .await,
    ];
    // (case, the block, the zone) — the sizes a VPC subnet is published to admit at the
    // large and the small end ("the allowed block size is between a /28 netmask and /16
    // netmask", Amazon VPC User Guide, "Subnets") and one in between.
    for (case, cidr, zone) in [
        ("a /20 subnet of a client-created VPC", "10.80.16.0/20", "us-east-1a"),
        ("a /24 subnet in another zone", "10.80.1.0/24", "us-east-1b"),
        ("a /28, the smallest block a subnet may have", "10.80.2.16/28", "us-east-1c"),
    ] {
        facts.push(created_subnet(router, case, &vpc, cidr, zone).await);
    }
    facts
}

/// A VPC of the client's own, so the rows are not all in the account's default one.
async fn own_vpc(router: &MultiServiceRouter) -> String {
    element(
        &ec2(router, "CreateVpc", &[("CidrBlock", OWN_VPC_CIDR)]).await,
        "vpcId",
    )
}

/// A subnet EC2 minted in `vpc` over `cidr` in `zone`, reported as EC2 itself describes it.
async fn created_subnet(
    router: &MultiServiceRouter,
    case: &'static str,
    vpc: &str,
    cidr: &str,
    zone: &str,
) -> SubnetFacts {
    let created = ec2(
        router,
        "CreateSubnet",
        &[("VpcId", vpc), ("CidrBlock", cidr), ("AvailabilityZone", zone)],
    )
    .await;
    subnet_facts(router, case, &element(&created, "subnetId")).await
}

/// Create a file system and answer its id once the clock-owed `creating` -> `available`
/// transition has landed, which is what `CreateMountTarget` requires of it.
async fn available_file_system(router: &MultiServiceRouter, token: &str) -> String {
    let created = efs(
        router,
        "POST",
        "/2015-02-01/file-systems",
        Some(json!({ "CreationToken": token })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: CreateFileSystem({token}) expected 201, got {} {:?}",
        created.status, created.body
    );
    let file_system_id = created.string("FileSystemId");
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = efs(
            router,
            "GET",
            &format!("/2015-02-01/file-systems?FileSystemId={file_system_id}"),
            None,
        )
        .await;
        if described.body["FileSystems"][0]["LifeCycleState"].as_str() == Some("available") {
            return file_system_id;
        }
        assert!(
            Instant::now() < deadline,
            "setup: file system {file_system_id} never reached available, last read {:?}",
            described.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn a_mount_targets_address_and_placement_come_from_the_ec2_subnet() {
    let router = build_router();
    let subnets = rows(&router).await;

    for (index, subnet) in subnets.iter().enumerate() {
        let case = subnet.case;
        let file_system_id = available_file_system(&router, &format!("placement-{index}")).await;
        let created = efs(
            &router,
            "POST",
            "/2015-02-01/mount-targets",
            Some(json!({
                "FileSystemId": file_system_id,
                "SubnetId": subnet.subnet_id,
            })),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateMountTarget expected 200, got {} {:?}",
            created.status, created.body
        );
        assert!(
            within(&subnet.cidr_block, &created.string("IpAddress")),
            "case {case:?}: the address must lie inside the subnet's own block {}, got {:?}",
            subnet.cidr_block,
            created.body["IpAddress"]
        );
        for (member, expected) in [
            ("SubnetId", &subnet.subnet_id),
            ("VpcId", &subnet.vpc_id),
            ("AvailabilityZoneName", &subnet.zone_name),
            ("AvailabilityZoneId", &subnet.zone_id),
        ] {
            assert_eq!(
                &created.string(member),
                expected,
                "case {case:?}: {member} must be the subnet's own value as EC2 reports it, \
                 got {:?}",
                created.body[member]
            );
        }

        // The persisted record carries the same placement, so a later read of it answers
        // the network the account really has rather than a value of the create's own.
        let described = efs(
            &router,
            "GET",
            &format!("/2015-02-01/mount-targets?FileSystemId={file_system_id}"),
            None,
        )
        .await;
        assert_eq!(
            described.body["MountTargets"],
            json!([created.body.clone()]),
            "case {case:?}: DescribeMountTargets must answer the stored description, got {:?}",
            described.body["MountTargets"]
        );
    }
}

#[tokio::test]
async fn a_subnet_the_account_does_not_have_cannot_be_placed_in() {
    let router = build_router();
    let file_system_id = available_file_system(&router, "no-such-subnet").await;

    // Each value is a well-formed `SubnetId` (`^subnet-[0-9a-f]{8,40}$`) that EC2 holds no
    // subnet for: EC2's 17-hex id form, its older 8-hex form, and one more of each width.
    for subnet_id in [
        "subnet-0123456789abcdef0",
        "subnet-deadbeef",
        "subnet-00000000000000001",
        "subnet-ffffffff",
    ] {
        let refused = efs(
            &router,
            "POST",
            "/2015-02-01/mount-targets",
            Some(json!({
                "FileSystemId": file_system_id,
                "SubnetId": subnet_id,
            })),
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_type.as_deref()),
            (400, Some("SubnetNotFound")),
            "case {subnet_id:?}: expected the modelled SubnetNotFound, got {} {:?}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.string("Message"),
            format!("The subnet ID '{subnet_id}' does not exist"),
            "case {subnet_id:?}: the refusal names the subnet the request asked for, got {:?}",
            refused.body["Message"]
        );
    }

    // Nothing was stored for any of them.
    let described = efs(
        &router,
        "GET",
        &format!("/2015-02-01/mount-targets?FileSystemId={file_system_id}"),
        None,
    )
    .await;
    assert_eq!(
        described.body["MountTargets"],
        json!([]),
        "a refused create must store nothing, got {:?}",
        described.body["MountTargets"]
    );
}

#[tokio::test]
async fn the_mount_target_refusals_and_the_security_group_replacement_carry_their_own_contract()
{
    let router = build_router();
    let vpc = own_vpc(&router).await;
    // TWO subnets of ONE Availability Zone in ONE VPC: the zone restriction is keyed on the
    // zone, so a retry in either of them is the same refusal.
    let zone = "us-east-1a";
    let first =
        created_subnet(&router, "the subnet the zone's mount target is in", &vpc, "10.80.64.0/20", zone)
            .await;
    let second =
        created_subnet(&router, "another subnet of that same zone", &vpc, "10.80.80.0/20", zone)
            .await;
    let file_system_id = available_file_system(&router, "refusal-contract").await;
    let created = efs(
        &router,
        "POST",
        "/2015-02-01/mount-targets",
        Some(json!({
            "FileSystemId": file_system_id,
            "SubnetId": first.subnet_id,
            "SecurityGroups": ["sg-0123456789abcdef0"],
        })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "CreateMountTarget expected 200, got {} {:?}",
        created.status, created.body
    );
    let mount_target_id = created.string("MountTargetId");

    // "there can be only one mount target per Availability Zone": the refusal is the same
    // fixed sentence whichever subnet of the occupied zone the retry names.
    for subnet in [&first, &second] {
        let case = subnet.case;
        let conflict = efs(
            &router,
            "POST",
            "/2015-02-01/mount-targets",
            Some(json!({
                "FileSystemId": file_system_id,
                "SubnetId": subnet.subnet_id,
            })),
        )
        .await;
        assert_eq!(
            (conflict.status, conflict.error_type.as_deref()),
            (409, Some("MountTargetConflict")),
            "case {case:?}: expected the modelled MountTargetConflict, got {} {:?}",
            conflict.status,
            conflict.body
        );
        assert_eq!(
            conflict.string("Message"),
            "mount target already exists in this AZ",
            "case {case:?}: the per-zone refusal carries one fixed sentence, got {:?}",
            conflict.body["Message"]
        );
    }

    // A file system with mount targets is not deleted: the refusal names the file system
    // the request asked for, and nothing else — not how many mount targets it has.
    let in_use = efs(
        &router,
        "DELETE",
        &format!("/2015-02-01/file-systems/{file_system_id}"),
        None,
    )
    .await;
    assert_eq!(
        (in_use.status, in_use.error_type.as_deref()),
        (409, Some("FileSystemInUse")),
        "expected the modelled FileSystemInUse, got {} {:?}",
        in_use.status,
        in_use.body
    );
    assert_eq!(
        in_use.string("Message"),
        format!(
            "The file system '{file_system_id}' cannot be deleted because it has mount \
             targets. You must delete the mount targets, and then try again."
        ),
        "the in-use refusal names only the requested file system, got {:?}",
        in_use.body["Message"]
    );

    // The security groups in effect are REPLACED by the request's, and the matching read
    // answers exactly that set.
    let groups = json!(["sg-00000000000000001", "sg-abcdef01"]);
    let modified = efs(
        &router,
        "PUT",
        &format!("/2015-02-01/mount-targets/{mount_target_id}/security-groups"),
        Some(json!({ "SecurityGroups": groups.clone() })),
    )
    .await;
    assert_eq!(
        (modified.status, modified.body_len),
        (204, 0),
        "ModifyMountTargetSecurityGroups expected 204 with an empty body, got {} {:?}",
        modified.status,
        modified.body
    );
    let read = efs(
        &router,
        "GET",
        &format!("/2015-02-01/mount-targets/{mount_target_id}/security-groups"),
        None,
    )
    .await;
    assert_eq!(
        (read.status, &read.body["SecurityGroups"]),
        (200, &groups),
        "the read must answer the replacement, got {} {:?}",
        read.status,
        read.body
    );

    // Once the mount target is gone, the mutation of it is refused as not found — the
    // client-side class, with the sentence built from the id the request named.
    let deleted = efs(
        &router,
        "DELETE",
        &format!("/2015-02-01/mount-targets/{mount_target_id}"),
        None,
    )
    .await;
    assert_eq!(
        deleted.status, 204,
        "DeleteMountTarget expected 204, got {} {:?}",
        deleted.status, deleted.body
    );
    let dead = efs(
        &router,
        "PUT",
        &format!("/2015-02-01/mount-targets/{mount_target_id}/security-groups"),
        Some(json!({ "SecurityGroups": ["sg-00000000000000001"] })),
    )
    .await;
    assert_eq!(
        (dead.status, dead.error_type.as_deref()),
        (404, Some("MountTargetNotFound")),
        "a mutation of a deleted mount target expected the modelled MountTargetNotFound, \
         got {} {:?}",
        dead.status,
        dead.body
    );
    assert_eq!(
        dead.string("Message"),
        format!("Mount target '{mount_target_id}' does not exist."),
        "the refusal names the mount target the request asked for, got {:?}",
        dead.body["Message"]
    );

    // And with its last mount target gone, the file system deletes.
    let dropped = efs(
        &router,
        "DELETE",
        &format!("/2015-02-01/file-systems/{file_system_id}"),
        None,
    )
    .await;
    assert_eq!(
        dropped.status, 204,
        "DeleteFileSystem expected 204 once the mount targets are gone, got {} {:?}",
        dropped.status, dropped.body
    );
}
