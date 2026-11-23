// SPDX-License-Identifier: CC0-1.0

//! What the `InstanceInformation` row of an EC2-registered managed node says about the
//! MACHINE behind it.
//!
//! RULE under test: a managed node an Amazon EC2 instance registered IS that instance, so the
//! row reports the same machine Amazon EC2 describes — its `SourceId` is the instance id and
//! its `SourceType` the EC2 instance type of source, its `IPAddress` and `ComputerName` are
//! the instance's own `PrivateIpAddress` and `PrivateDnsName` — and the operating system it
//! reports (`PlatformType`, `PlatformName`, `PlatformVersion`) is the one held by the image
//! the instance was launched from, read off that image's published name.
//!
//! The rows of the platform table differ along what the rule generalises over: the operating
//! system family of the image (four Amazon Linux families, a Windows Server release and four
//! other distributions), where its name comes from (a public Systems Manager parameter AWS
//! publishes an id under, and an image the account registers itself), and whether the name
//! names a family this emulator can read at all — an image of no family, an image whose name
//! only opens with a family's first word, and a family's image that spells no release. The expected name/version pairs are each
//! family's own release metadata — the `NAME` and `VERSION_ID` of its os-release file, and
//! for Windows Server the edition a base image runs with the NT version Microsoft publishes
//! for the release.
//!
//! Every call goes through the wire a client uses — `awsJson1_1` for Systems Manager,
//! `ec2Query` for Amazon EC2, `awsQuery` for IAM — through the harness
//! `tests/all/ec2_managed_node_registration.rs` builds. Nothing reaches into a state struct,
//! and nothing waits a fixed time: a registration is a stored due instant, so each case LOOKS
//! AGAIN until the node is reported or that harness's generous deadline passes.

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_managed_node_registration::{
    create_profile, ec2, element, emulator, image_id, launch, node_when_registered, Grant,
    Profile, EC2_PRINCIPAL,
};
use crate::parameter_store::REGION;

/// The instance profile every instance here launches with: a role Amazon EC2 may act as that
/// carries the AWS-managed policy a client attaches to let the agent register. Which profiles
/// let an instance register is the registration suite's own table; here every row registers,
/// so the rows differ only in the image.
const PROFILE: Profile = Profile {
    name: "node-platform-rows",
    trusts: EC2_PRINCIPAL,
    grant: Grant::AwsManaged,
};

/// The public parameter the Amazon Linux 2 image id is resolved through — the AMI the issue's
/// client launches.
const AL2_PARAMETER: &str = "/aws/service/ami-amazon-linux-latest/amzn2-ami-hvm-x86_64-gp2";

/// The root device a registered image boots from, which is what makes the register an
/// Amazon EBS-backed one.
const ROOT_DEVICE: &str = "/dev/xvda";

/// The architecture every registered image of this suite is built for, so the rows differ in
/// the operating system rather than in the hardware.
const ARCHITECTURE: &str = "x86_64";

/// The virtualization every registered image declares, which is what Amazon publishes today.
const VIRTUALIZATION: &str = "hvm";

/// Where one row's image comes from.
#[derive(Debug, Clone, Copy)]
enum Image {
    /// The public Systems Manager parameter AWS publishes the image's id under.
    Published(&'static str),
    /// An image the account registers itself, under the name the project that builds it
    /// publishes its own images under.
    Registered(&'static str),
}

/// One row of the platform table: an image, and the operating system a node launched from it
/// reports.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// What this row is, named in every assertion it makes.
    case: &'static str,
    /// The image the row's instance is launched from.
    image: Image,
    /// `InstanceInformation$PlatformType`, from the modelled `PlatformType` enum.
    platform_type: &'static str,
    /// `InstanceInformation$PlatformName`, or `None` for a row whose image name names no
    /// operating system — a member nothing can be read for is a member the service does not
    /// send.
    platform_name: Option<&'static str>,
    /// `InstanceInformation$PlatformVersion`, or `None` for the same reason.
    platform_version: Option<&'static str>,
}

/// The images a row is launched from, and the platform each one's node reports.
///
/// The published rows are AWS's own aliases for the current build of a family; the registered
/// rows carry the name forms Canonical, Debian, Red Hat and SUSE publish their images under,
/// so the version is read off the name rather than off a family table. The last row is an
/// image of the account's own with a name that names no family at all.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an Amazon Linux 2 image resolved through its public parameter",
            image: Image::Published(AL2_PARAMETER),
            platform_type: "Linux",
            platform_name: Some("Amazon Linux"),
            platform_version: Some("2"),
        },
        Row {
            case: "an Amazon Linux 2023 image resolved through its public parameter",
            image: Image::Published(
                "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64",
            ),
            platform_type: "Linux",
            platform_name: Some("Amazon Linux"),
            platform_version: Some("2023"),
        },
        Row {
            case: "the original Amazon Linux family, which renamed itself in a later release",
            image: Image::Published(
                "/aws/service/ami-amazon-linux-latest/amzn-ami-hvm-x86_64-gp2",
            ),
            platform_type: "Linux",
            platform_name: Some("Amazon Linux AMI"),
            platform_version: Some("2018.03"),
        },
        Row {
            case: "a Windows Server 2022 base image",
            image: Image::Published(
                "/aws/service/ami-windows-latest/Windows_Server-2022-English-Full-Base",
            ),
            platform_type: "Windows",
            platform_name: Some("Microsoft Windows Server 2022 Datacenter"),
            platform_version: Some("10.0.20348"),
        },
        Row {
            case: "an Ubuntu image of the account's own",
            image: Image::Registered("ubuntu-noble-24.04-amd64-server-20250601"),
            platform_type: "Linux",
            platform_name: Some("Ubuntu"),
            platform_version: Some("24.04"),
        },
        Row {
            case: "a Debian image, whose name spells one version component",
            image: Image::Registered("debian-12-amd64-20240717-1811"),
            platform_type: "Linux",
            platform_name: Some("Debian GNU/Linux"),
            platform_version: Some("12"),
        },
        Row {
            case: "a Red Hat Enterprise Linux image, whose name spells three",
            image: Image::Registered("RHEL-9.6.0_HVM-20250618-x86_64-0-Hourly2-GP3"),
            platform_type: "Linux",
            platform_name: Some("Red Hat Enterprise Linux"),
            platform_version: Some("9.6"),
        },
        Row {
            case: "a SUSE image, whose name spells a service pack",
            image: Image::Registered("suse-sles-15-sp7-v20250612-hvm-ssd-x86_64"),
            platform_type: "Linux",
            platform_name: Some("SLES"),
            platform_version: Some("15.7"),
        },
        Row {
            case: "an image of the account's own whose name names no operating system",
            image: Image::Registered("golden-base-image-v3"),
            platform_type: "Linux",
            platform_name: None,
            platform_version: None,
        },
        Row {
            case: "an image whose name opens with a family's first word but names another \
                   kind of image",
            image: Image::Registered("amzn-ec2-macos-14.4-20240408-x86_64"),
            platform_type: "Linux",
            platform_name: None,
            platform_version: None,
        },
        Row {
            case: "a family's image whose name spells a build date and no release",
            image: Image::Registered("debian-amd64-20240717"),
            platform_type: "Linux",
            platform_name: Some("Debian GNU/Linux"),
            platform_version: None,
        },
    ]
}

/// Register one image of the account's own under `name`, as the image id Amazon EC2 answered.
async fn register_image(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = ec2(
        router,
        REGION,
        "RegisterImage",
        &[
            ("Name", name),
            ("RootDeviceName", ROOT_DEVICE),
            ("Architecture", ARCHITECTURE),
            ("VirtualizationType", VIRTUALIZATION),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RegisterImage {name} must register an image, got {status} with {body}"
    );
    element(&body, "imageId")
        .unwrap_or_else(|| panic!("case {case:?}: RegisterImage reports an image id, got {body}"))
}

/// The id of the image one row launches from, resolved the way that row says.
async fn row_image(router: &MultiServiceRouter, row: &Row) -> String {
    match row.image {
        Image::Published(parameter) => image_id(router, parameter).await,
        Image::Registered(name) => register_image(router, row.case, name).await,
    }
}

/// The value a reported node carries for `member`, or `None` when it carries none.
fn member<'a>(reported: &'a Value, member: &str) -> Option<&'a str> {
    reported[member].as_str()
}

/// The `PrivateIpAddress` and `PrivateDnsName` Amazon EC2 reports for `instance_id` — its own
/// view of the machine the node is.
async fn described_machine(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> (String, String) {
    let (status, body) = ec2(
        router,
        REGION,
        "DescribeInstances",
        &[("InstanceId.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances({instance_id}) must answer, got {status} with {body}"
    );
    let address = element(&body, "privateIpAddress").unwrap_or_else(|| {
        panic!("case {case:?}: DescribeInstances reports a private address, got {body}")
    });
    let host_name = element(&body, "privateDnsName").unwrap_or_else(|| {
        panic!("case {case:?}: DescribeInstances reports a private DNS name, got {body}")
    });
    (address, host_name)
}

#[tokio::test]
async fn a_node_reports_the_operating_system_of_the_image_it_was_launched_from() {
    let router = emulator();
    create_profile(&router, &PROFILE).await;
    // Every instance is launched before any node is awaited, so the rows' registration spans
    // run together rather than one after another.
    let mut launched = Vec::new();
    for row in rows() {
        let image = row_image(&router, &row).await;
        let instance = launch(&router, row.case, REGION, &image, Some(PROFILE.name)).await;
        launched.push((row, instance));
    }
    for (row, instance) in launched {
        let reported = node_when_registered(&router, row.case, REGION, &instance).await;
        assert_eq!(
            member(&reported, "PlatformType"),
            Some(row.platform_type),
            "case {:?}: expected PlatformType {:?}, got {:?} in {reported}",
            row.case,
            row.platform_type,
            member(&reported, "PlatformType")
        );
        assert_eq!(
            member(&reported, "PlatformName"),
            row.platform_name,
            "case {:?}: expected PlatformName {:?}, got {:?} in {reported}",
            row.case,
            row.platform_name,
            member(&reported, "PlatformName")
        );
        assert_eq!(
            member(&reported, "PlatformVersion"),
            row.platform_version,
            "case {:?}: expected PlatformVersion {:?}, got {:?} in {reported}",
            row.case,
            row.platform_version,
            member(&reported, "PlatformVersion")
        );
    }
}

#[tokio::test]
async fn a_node_names_the_same_machine_amazon_ec2_describes() {
    let router = emulator();
    create_profile(&router, &PROFILE).await;
    let image = image_id(&router, AL2_PARAMETER).await;
    // TWO instances of the same image, so no member of either row can be a value this
    // endpoint holds for every node: each one names its own machine.
    let mut launched = Vec::new();
    for case in ["the first instance", "the second instance"] {
        launched.push((
            case,
            launch(&router, case, REGION, &image, Some(PROFILE.name)).await,
        ));
    }
    let mut addresses = Vec::new();
    for (case, instance) in launched {
        let reported = node_when_registered(&router, case, REGION, &instance).await;
        let (address, host_name) = described_machine(&router, case, &instance).await;
        assert_eq!(
            member(&reported, "SourceId"),
            Some(instance.as_str()),
            "case {case:?}: expected SourceId {instance:?}, got {:?} in {reported}",
            member(&reported, "SourceId")
        );
        assert_eq!(
            member(&reported, "SourceType"),
            Some("AWS::EC2::Instance"),
            "case {case:?}: expected the EC2 instance SourceType, got {:?} in {reported}",
            member(&reported, "SourceType")
        );
        assert_eq!(
            member(&reported, "ResourceType"),
            Some("EC2Instance"),
            "case {case:?}: expected ResourceType EC2Instance, got {:?} in {reported}",
            member(&reported, "ResourceType")
        );
        assert_eq!(
            member(&reported, "InstanceId"),
            Some(instance.as_str()),
            "case {case:?}: expected InstanceId {instance:?}, got {:?} in {reported}",
            member(&reported, "InstanceId")
        );
        assert_eq!(
            member(&reported, "IPAddress"),
            Some(address.as_str()),
            "case {case:?}: expected the instance's own address {address:?}, got {:?} in \
             {reported}",
            member(&reported, "IPAddress")
        );
        assert_eq!(
            member(&reported, "ComputerName"),
            Some(host_name.as_str()),
            "case {case:?}: expected the instance's own host name {host_name:?}, got {:?} in \
             {reported}",
            member(&reported, "ComputerName")
        );
        assert_eq!(
            member(&reported, "PingStatus"),
            Some("Online"),
            "case {case:?}: expected the agent reporting Online, got {:?} in {reported}",
            member(&reported, "PingStatus")
        );
        addresses.push((case, address));
    }
    assert_ne!(
        addresses[0].1, addresses[1].1,
        "the two instances must be two machines: both report the address {:?}",
        addresses[0].1
    );
}
