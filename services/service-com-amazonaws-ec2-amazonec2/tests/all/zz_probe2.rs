// SPDX-License-Identifier: CC0-1.0

//! A launch that SPELLS OUT the defaults of the facts it later reads back, driven over the
//! wire the service speaks: `ec2Query`, an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! ONE rule is under test: a launch that names a member AT ITS DEFAULT, or leaves an optional
//! member empty, is the same launch as the one that omitted it — "if you specify neither one,
//! Amazon EC2 automatically selects an Availability Zone for you"
//! (`Placement$AvailabilityZone`) — so it is SERVED with a reservation whose instance starts in
//! `pending` ("`0` (pending)", the `instance-state-code` filter), and every fact it spelled out
//! is the fact the instance reports back: through `DescribeInstanceAttribute` for the
//! attributes a launch sets, and through `DescribeInstanceCreditSpecifications` for the credit
//! option.
//!
//! The rows span the class rather than one recorded launch: the credit option is driven on both
//! documented values ("valid values: `standard` | `unlimited`",
//! `CreditSpecificationRequest$CpuCredits`), the shutdown behaviour on both
//! (`InstanceInitiatedShutdownBehavior`), and the root volume on two sizes and two types — each
//! row carrying the empty placement zone, the explicit block device mapping and the `Name` tag
//! together, which is the shape a client that normalises its launch parameters sends.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements};
use super::wire::{child, child_text, document, param, set_items, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The AMI every launch here names: an id that resolves, whose own facts answer the members
/// the launch leaves to the image.
const AMI: &str = "ami-0123456789abcdef0";

/// The instance type every launch here asks for — a burstable type, so it HAS a credit option.
const INSTANCE_TYPE: &str = "t3.micro";

/// The network every launch here goes into.
const VPC_CIDR: &str = "10.92.0.0/16";
const SUBNET_CIDR: &str = "10.92.1.0/24";

/// The security group a launch that names none is placed in: "if you don't specify a security
/// group ID, we use the default security group for the VPC" (`RunInstances`).
const DEFAULT_GROUP: &str = "default";

/// The root device every launch here spells out, and the `Image$RootDeviceType` the instance
/// then reports.
const ROOT_DEVICE_NAME: &str = "/dev/xvda";
const ROOT_DEVICE_TYPE: &str = "ebs";

/// One call that must answer HTTP 200, as its body.
async fn ok(router: &MultiServiceRouter, case: &str, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// What one row launches: the defaults it spells out.
struct Row {
    case: &'static str,
    cpu_credits: &'static str,
    shutdown_behaviour: &'static str,
    volume_size: &'static str,
    volume_type: &'static str,
}

/// One emulator with one VPC and one subnet of the caller's own.
async fn network(case: &str) -> (MultiServiceRouter, String, String) {
    let (router, _engine) = super::ec2_query::emulator();
    let body = ok(&router, case, "CreateVpc", &[param("CidrBlock", VPC_CIDR)]).await;
    let vpc_id = element(&body, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVpc reported no VPC: {body}"));
    let body = ok(
        &router,
        case,
        "CreateSubnet",
        &[param("VpcId", &vpc_id), param("CidrBlock", SUBNET_CIDR)],
    )
    .await;
    let subnet_id = element(&body, "subnetId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSubnet reported no subnet: {body}"));
    (router, vpc_id, subnet_id)
}

/// The value one `DescribeInstanceAttribute` reports for `attribute`.
async fn attribute(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    attribute: &str,
) -> String {
    let body = ok(
        router,
        case,
        "DescribeInstanceAttribute",
        &[
            param("InstanceId", instance_id),
            param("Attribute", attribute),
        ],
    )
    .await;
    let parsed = document(case, &body);
    let reported = child(parsed.root_element(), attribute).unwrap_or_else(|| {
        panic!("case {case:?}: the answer must carry the {attribute} element: {body}")
    });
    child_text(reported, "value").unwrap_or_else(|| {
        panic!("case {case:?}: the {attribute} attribute must report a value: {body}")
    })
}

/// The credit option `DescribeInstanceCreditSpecifications` reports for `instance_id`.
async fn credit_option(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let body = ok(
        router,
        case,
        "DescribeInstanceCreditSpecifications",
        &[param("InstanceId.1", instance_id)],
    )
    .await;
    let parsed = document(case, &body);
    let items = set_items(parsed.root_element(), "instanceCreditSpecificationSet");
    let item = *items.first().unwrap_or_else(|| {
        panic!("case {case:?}: the answer must report the instance's option: {body}")
    });
    child_text(item, "cpuCredits")
        .unwrap_or_else(|| panic!("case {case:?}: the report must name an option: {body}"))
}

/// RULE: the explicit-default launch is SERVED, reports a reservation whose instance is
/// `pending`, and every fact it spelled out is read back off the instance.
#[tokio::test]
async fn a_launch_that_spells_out_its_defaults_is_served_and_read_back() {
    let case = "the network every row launches into";
    let (router, vpc_id, subnet_id) = network(case).await;

    let rows: [Row; 4] = [
        Row {
            case: "the standard credit option, stopped on shutdown, a gp3 root volume",
            cpu_credits: "standard",
            shutdown_behaviour: "stop",
            volume_size: "8",
            volume_type: "gp3",
        },
        Row {
            case: "the unlimited credit option, stopped on shutdown, a gp3 root volume",
            cpu_credits: "unlimited",
            shutdown_behaviour: "stop",
            volume_size: "8",
            volume_type: "gp3",
        },
        Row {
            case: "the standard credit option, terminated on shutdown, a bigger gp2 volume",
            cpu_credits: "standard",
            shutdown_behaviour: "terminate",
            volume_size: "30",
            volume_type: "gp2",
        },
        Row {
            case: "the unlimited credit option, terminated on shutdown, a bigger gp2 volume",
            cpu_credits: "unlimited",
            shutdown_behaviour: "terminate",
            volume_size: "30",
            volume_type: "gp2",
        },
    ];

    for row in rows {
        let case = row.case;
        let body = ok(
            &router,
            case,
            "RunInstances",
            &[
                param("ImageId", AMI),
                param("InstanceType", INSTANCE_TYPE),
                param("MinCount", "1"),
                param("MaxCount", "1"),
                param("SubnetId", &subnet_id),
                // The launch spells out the zone member at its default — "EC2 chooses" —
                // which is the empty value.
                param("Placement.AvailabilityZone", ""),
                param("Placement.Tenancy", "default"),
                param("Monitoring.Enabled", "false"),
                param("DisableApiTermination", "false"),
                param("InstanceInitiatedShutdownBehavior", row.shutdown_behaviour),
                param("CreditSpecification.CpuCredits", row.cpu_credits),
                param("BlockDeviceMapping.1.DeviceName", ROOT_DEVICE_NAME),
                param("BlockDeviceMapping.1.Ebs.VolumeSize", row.volume_size),
                param("BlockDeviceMapping.1.Ebs.VolumeType", row.volume_type),
                param("BlockDeviceMapping.1.Ebs.DeleteOnTermination", "true"),
                param("BlockDeviceMapping.1.Ebs.Encrypted", "false"),
                param("TagSpecification.1.ResourceType", "instance"),
                param("TagSpecification.1.Tag.1.Key", "Name"),
                param("TagSpecification.1.Tag.1.Value", case),
            ],
        )
        .await;

        // The reservation projection: the reservation itself, and the one instance it holds.
        let parsed = document(case, &body);
        let root = parsed.root_element();
        assert!(
            child_text(root, "reservationId").is_some_and(|id| id.starts_with("r-")),
            "case {case:?}: the answer must report the reservation it minted: {body}"
        );
        assert!(
            child_text(root, "ownerId").is_some_and(|owner| !owner.is_empty()),
            "case {case:?}: the answer must report the account that owns it: {body}"
        );
        let instances = set_items(root, "instancesSet");
        assert_eq!(
            instances.len(),
            1,
            "case {case:?}: expected exactly one instance, got {body}"
        );
        let instance = instances[0];
        let state = child(instance, "instanceState").unwrap_or_else(|| {
            panic!("case {case:?}: the instance must report its state: {body}")
        });
        assert_eq!(
            (
                child_text(state, "code").unwrap_or_default(),
                child_text(state, "name").unwrap_or_default(),
            ),
            ("0".to_string(), "pending".to_string()),
            "case {case:?}: a launched instance starts pending, got {body}"
        );
        assert_eq!(
            child_text(instance, "instanceType").as_deref(),
            Some(INSTANCE_TYPE),
            "case {case:?}: expected the type the launch asked for, got {body}"
        );
        assert_eq!(
            child_text(instance, "subnetId").as_deref(),
            Some(subnet_id.as_str()),
            "case {case:?}: expected the subnet the launch named, got {body}"
        );
        assert_eq!(
            child_text(instance, "vpcId").as_deref(),
            Some(vpc_id.as_str()),
            "case {case:?}: expected the VPC of that subnet, got {body}"
        );
        assert_eq!(
            child_text(instance, "rootDeviceType").as_deref(),
            Some(ROOT_DEVICE_TYPE),
            "case {case:?}: expected an EBS-backed instance, got {body}"
        );
        let groups = set_items(instance, "groupSet");
        let named: Vec<Option<String>> = groups
            .iter()
            .map(|group| child_text(*group, "groupName"))
            .collect();
        assert_eq!(
            named,
            vec![Some(DEFAULT_GROUP.to_string())],
            "case {case:?}: a launch naming no group is placed in the VPC's own, got {body}"
        );
        // The zone the launch left empty: one of the Region's, chosen for it.
        let placement = child(instance, "placement").unwrap_or_else(|| {
            panic!("case {case:?}: the instance must report its placement: {body}")
        });
        let zone = child_text(placement, "availabilityZone").unwrap_or_default();
        let zones = ok(&router, case, "DescribeAvailabilityZones", &[]).await;
        assert!(
            elements(&zones, "zoneName").contains(&zone),
            "case {case:?}: expected a zone of the Region, got {zone:?} against {zones}"
        );

        let instance_id = child_text(instance, "instanceId")
            .unwrap_or_else(|| panic!("case {case:?}: the instance must report its id: {body}"));

        // Read back: every fact the launch spelled out, off the instance itself.
        assert_eq!(
            attribute(&router, case, &instance_id, "instanceType").await,
            INSTANCE_TYPE,
            "case {case:?}: the instanceType attribute must report the launch's own type"
        );
        assert_eq!(
            attribute(
                &router,
                case,
                &instance_id,
                "instanceInitiatedShutdownBehavior"
            )
            .await,
            row.shutdown_behaviour,
            "case {case:?}: the shutdown behaviour must be the launch's own"
        );
        assert_eq!(
            attribute(&router, case, &instance_id, "disableApiTermination").await,
            "false",
            "case {case:?}: the launch asked for no termination protection"
        );
        assert_eq!(
            credit_option(&router, case, &instance_id).await,
            row.cpu_credits,
            "case {case:?}: the credit option must be the launch's own"
        );
    }
}
