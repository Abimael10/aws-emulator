// SPDX-License-Identifier: CC0-1.0

//! The OPTIONAL members a read of an AMI and a read of an instance carry, end to end over
//! the wire the service speaks — `ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness.
//!
//! Three rules, each with a table of its own:
//!
//! 1. EVERY Amazon EBS device an AMI describes REPORTS ITS ENCRYPTION STATE, and that state
//!    is the one the device restores with: the snapshot's own, and unencrypted for a device
//!    that restores no snapshot. The member is reported with the value `false` rather than
//!    left out, so a client deciding whether a copy of the image needs a KMS key can tell
//!    "unencrypted" from "not reported".
//! 2. AN IMAGE'S FREE TIER ELIGIBILITY FOLLOWS ITS OWN BILLING: an AMI billed through a
//!    product code of its own charges for its software beside the instance hours the Free
//!    Tier covers, so it cannot be launched under those limits; an image carrying no such
//!    code can, whoever published it.
//! 3. A PUBLIC ADDRESS IS REPORTED AGAINST THE PRIVATE ADDRESS IT IS BOUND TO: the
//!    association an instance reports against ONE private IPv4 address carries the same
//!    members as the interface's own association — the public address, its owner and its
//!    public DNS name — because one public address is mapped onto one private address of the
//!    interface, and the instance reports that same hostname as its `dnsName`.
//!
//! Every row is a distinct input along what its rule generalises over — how many devices the
//! AMI maps, whether a device restores an encrypted snapshot, an unencrypted one or none at
//! all, which writer stored the image, how many product codes it is billed through and of
//! which type, and whether the Elastic IP is bound to a primary or a secondary private
//! address, and whether another address was swapped out for it.

use std::time::{Duration, Instant};

use ec2::ops::types::ProductCode;
use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec, TRANSITION_DELAY};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, indexed};
use super::volume_lifecycle::{REGION, p, zone_name};
use super::wire::{Param, child, child_text, document, set_items};

/// The AMI every launch here uses: `RunInstances` only needs an image id that resolves.
const AMI: &str = "ami-0123456789abcdef0";

/// How long a poll below keeps looking: a large multiple of the service's own transition
/// span, so a loaded or slow host is never the reason an assertion fails. Nothing is waited
/// out — the CONDITION is looked for again until it holds.
fn poll_deadline() -> Duration {
    TRANSITION_DELAY * 40
}

/// One look inside that deadline, so the deadline costs many looks rather than one.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One `action` call signed for the Region this suite works in.
async fn op(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One call that must answer HTTP 200, as its body.
async fn served(router: &MultiServiceRouter, case: &str, action: &str, params: &[Param]) -> String {
    let (status, body) = op(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

// ---------------------------------------------------------------------------
// Rule 1: every Amazon EBS device of an AMI reports the encryption state it restores with
// ---------------------------------------------------------------------------

/// Mint one snapshot of this account of `size` GiB, encrypted or not, through the operations
/// that create one, and answer its id.
async fn snapshot_of(
    router: &MultiServiceRouter,
    case: &str,
    size: i32,
    encrypted: bool,
) -> String {
    let mut params = vec![
        p("AvailabilityZone", zone_name(0)),
        p("Size", size.to_string()),
    ];
    if encrypted {
        params.push(p("Encrypted", "true"));
    }
    let body = served(router, case, "CreateVolume", &params).await;
    let volume_id =
        element(&body, "volumeId").unwrap_or_else(|| panic!("case {case:?}: no volumeId in {body}"));
    let body = served(router, case, "CreateSnapshot", &[p("VolumeId", &volume_id)]).await;
    element(&body, "snapshotId").unwrap_or_else(|| panic!("case {case:?}: no snapshotId in {body}"))
}

/// The form fields of one block device mapping at `index` (counting from 1).
fn mapping_fields(index: usize, device_name: &str, members: &[(&str, String)]) -> Vec<Param> {
    let mut fields = vec![p(
        &format!("BlockDeviceMapping.{index}.DeviceName"),
        device_name,
    )];
    for (member, value) in members {
        fields.push(p(
            &format!("BlockDeviceMapping.{index}.{member}"),
            value.clone(),
        ));
    }
    fields
}

/// Every image of a `DescribeImages` body: the element that carries an `imageId`.
fn image_items<'a, 'i>(
    parsed: &'a roxmltree::Document<'i>,
) -> Vec<roxmltree::Node<'a, 'i>> {
    parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "imageId").is_some())
        .collect()
}

/// The one image `image_id` names, as the `(device name, encrypted)` pairs of the Amazon EBS
/// devices it describes — `None` for a device whose Amazon EBS half reports no `encrypted`
/// member at all, which is exactly what a client cannot act on.
async fn described_device_encryption(
    router: &MultiServiceRouter,
    case: &str,
    image_id: &str,
) -> Vec<(String, Option<String>)> {
    let body = served(router, case, "DescribeImages", &[p("ImageId.1", image_id)]).await;
    let parsed = document(case, &body);
    let items = image_items(&parsed);
    assert_eq!(
        items.len(),
        1,
        "case {case:?}: expected exactly one image for {image_id}, got {body}"
    );
    set_items(items[0], "blockDeviceMapping")
        .into_iter()
        .filter_map(|mapping| {
            let device_name = child_text(mapping, "deviceName").unwrap_or_default();
            let ebs = child(mapping, "ebs")?;
            Some((device_name, child_text(ebs, "encrypted")))
        })
        .collect()
}

/// One row of the encryption table: what the register describes, and what every device of the
/// AMI must then report.
struct DeviceRow {
    /// What makes this row a different input.
    case: &'static str,
    /// The mapping parameters of the register, beside its `Name` and `RootDeviceName`.
    mappings: Vec<Param>,
    /// The root device the register names.
    root_device: &'static str,
    /// The `(device name, encrypted)` pairs `DescribeImages` must then report, in order.
    reported: Vec<(&'static str, &'static str)>,
}

/// RULE 1: every Amazon EBS device of a registered AMI reports the encryption state it
/// restores with, taken from the snapshot it restores.
///
/// The rows differ along how many devices the AMI maps, whether a device restores an
/// encrypted snapshot, an unencrypted one or no snapshot at all, and whether the encrypted
/// device is the root or a data device — so no row is the rule.
#[tokio::test]
async fn every_amazon_ebs_device_of_an_ami_reports_the_encryption_state_it_restores_with() {
    let (router, _engine) = super::ec2_query::emulator();

    let plain = snapshot_of(&router, "the table's unencrypted snapshot", 8, false).await;
    let secret = snapshot_of(&router, "the table's encrypted snapshot", 20, true).await;

    let rows: Vec<DeviceRow> = vec![
        DeviceRow {
            case: "a root device restored from an unencrypted snapshot",
            mappings: mapping_fields(1, "/dev/xvda", &[("Ebs.SnapshotId", plain.clone())]),
            root_device: "/dev/xvda",
            reported: vec![("/dev/xvda", "false")],
        },
        DeviceRow {
            case: "a root device restored from an encrypted snapshot",
            mappings: mapping_fields(1, "/dev/xvda", &[("Ebs.SnapshotId", secret.clone())]),
            root_device: "/dev/xvda",
            reported: vec![("/dev/xvda", "true")],
        },
        DeviceRow {
            case: "an unencrypted root device beside an encrypted data device",
            mappings: mapping_fields(1, "/dev/xvda", &[("Ebs.SnapshotId", plain.clone())])
                .into_iter()
                .chain(mapping_fields(
                    2,
                    "/dev/sdb",
                    &[("Ebs.SnapshotId", secret.clone())],
                ))
                .collect(),
            root_device: "/dev/xvda",
            reported: vec![("/dev/xvda", "false"), ("/dev/sdb", "true")],
        },
        DeviceRow {
            case: "an encrypted root device beside an unencrypted data device",
            mappings: mapping_fields(1, "/dev/sda1", &[("Ebs.SnapshotId", secret.clone())])
                .into_iter()
                .chain(mapping_fields(
                    2,
                    "/dev/sdc",
                    &[("Ebs.SnapshotId", plain.clone())],
                ))
                .collect(),
            root_device: "/dev/sda1",
            reported: vec![("/dev/sda1", "true"), ("/dev/sdc", "false")],
        },
        DeviceRow {
            case: "a device that restores no snapshot, sized by the request",
            mappings: mapping_fields(
                1,
                "/dev/xvda",
                &[("Ebs.VolumeSize", "12".to_string())],
            ),
            root_device: "/dev/xvda",
            reported: vec![("/dev/xvda", "false")],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let mut params = vec![
            p("Name", format!("device-encryption-row-{index}")),
            p("RootDeviceName", row.root_device),
        ];
        params.extend(row.mappings.iter().cloned());
        let body = served(&router, case, "RegisterImage", &params).await;
        let image_id = element(&body, "imageId")
            .unwrap_or_else(|| panic!("case {case:?}: RegisterImage minted no AMI: {body}"));

        let reported = described_device_encryption(&router, case, &image_id).await;
        let expected: Vec<(String, Option<String>)> = row
            .reported
            .iter()
            .map(|(device, encrypted)| (device.to_string(), Some(encrypted.to_string())))
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {expected:?} from the AMI's devices, got {reported:?}"
        );
    }
}

/// RULE 1, for the image whose root mapping the STORE derives rather than a register
/// supplying it: a published catalog AMI reports its root device unencrypted too, with the
/// member present.
#[tokio::test]
async fn a_published_image_reports_its_derived_root_device_as_unencrypted() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "the published catalog";

    let body = served(
        &router,
        case,
        "DescribeImages",
        &[p("Owner.1", "amazon"), p("IncludeDeprecated", "true")],
    )
    .await;
    let parsed = document(case, &body);
    let items = image_items(&parsed);
    assert!(
        !items.is_empty(),
        "case {case:?}: a fresh account sees the published catalog, got {body}"
    );
    for image in items {
        let name = child_text(image, "name").unwrap_or_default();
        for mapping in set_items(image, "blockDeviceMapping") {
            let device = child_text(mapping, "deviceName").unwrap_or_default();
            let Some(ebs) = child(mapping, "ebs") else {
                continue;
            };
            assert_eq!(
                child_text(ebs, "encrypted").as_deref(),
                Some("false"),
                "case {case:?}: image {name:?} device {device:?} restores an unencrypted \
                 snapshot this store minted, so it reports encrypted=false"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 2: Free Tier eligibility follows the image's own billing
// ---------------------------------------------------------------------------

/// One product code of a paid image.
fn product_code(id: &str, code_type: &str) -> ProductCode {
    ProductCode {
        product_code_id: Some(id.to_string()),
        product_code_type: Some(code_type.to_string()),
    }
}

/// Store one AMI of this account the way a register would, and answer its id — the path a
/// paid image reaches this store by, no operation of this emulator being authorized to claim
/// billing product codes.
fn seeded_image(engine: &engine::InMemoryEngine, name: &str, codes: Vec<ProductCode>) -> String {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    guard.insert_image(ImageSpec {
        name: name.to_string(),
        product_codes: codes,
        ..ImageSpec::default()
    })
}

/// What `DescribeImages` reports as `freeTierEligible` for the one AMI `image_id`.
async fn described_free_tier(router: &MultiServiceRouter, case: &str, image_id: &str) -> Option<String> {
    let body = served(router, case, "DescribeImages", &[p("ImageId.1", image_id)]).await;
    let parsed = document(case, &body);
    let items = image_items(&parsed);
    assert_eq!(
        items.len(),
        1,
        "case {case:?}: expected exactly one image for {image_id}, got {body}"
    );
    child_text(items[0], "freeTierEligible")
}

/// RULE 2: an AMI billed through a product code of its own is not Free-Tier launchable, and
/// every image that carries no such code is.
///
/// The rows differ along how many codes the image is billed through and of which type, and
/// along which writer stored the image — the store's own seeding, and a register over the
/// wire.
#[tokio::test]
async fn an_image_reports_the_free_tier_eligibility_its_billing_gives_it() {
    let (router, engine) = super::ec2_query::emulator();

    let rows: Vec<(&str, Vec<ProductCode>, &str)> = vec![
        ("an image nobody sells", Vec::new(), "true"),
        (
            "an image billed through one Marketplace code",
            vec![product_code("abc123prodcode", "marketplace")],
            "false",
        ),
        (
            "an image billed through a DevPay code",
            vec![product_code("aaa111bbb222ccc3", "devpay")],
            "false",
        ),
        (
            "an image billed through two codes",
            vec![
                product_code("abc123prodcode", "marketplace"),
                product_code("def456prodcode", "marketplace"),
            ],
            "false",
        ),
    ];

    for (index, (case, codes, expected)) in rows.into_iter().enumerate() {
        let image_id = seeded_image(&engine, &format!("free-tier-row-{index}"), codes);
        let reported = described_free_tier(&router, case, &image_id).await;
        assert_eq!(
            reported.as_deref(),
            Some(expected),
            "case {case:?}: expected freeTierEligible {expected:?}, got {reported:?}"
        );
    }

    // The same rule for an AMI a REGISTER mints: nothing bills it, so it is Free-Tier
    // launchable.
    let case = "a registered AMI of this account";
    let snapshot = snapshot_of(&router, case, 8, false).await;
    let body = served(
        &router,
        case,
        "RegisterImage",
        &[
            p("Name", "free-tier-registered"),
            p("RootDeviceName", "/dev/xvda"),
            p("BlockDeviceMapping.1.DeviceName", "/dev/xvda"),
            p("BlockDeviceMapping.1.Ebs.SnapshotId", &snapshot),
        ],
    )
    .await;
    let image_id = element(&body, "imageId")
        .unwrap_or_else(|| panic!("case {case:?}: RegisterImage minted no AMI: {body}"));
    let reported = described_free_tier(&router, case, &image_id).await;
    assert_eq!(
        reported.as_deref(),
        Some("true"),
        "case {case:?}: expected freeTierEligible \"true\", got {reported:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule 3: the public DNS name belongs to the interface, not to a private address
// ---------------------------------------------------------------------------

/// One association as an instance report carries it: the members the assertions below read,
/// with `public_dns_name` as `Some` only when the member is REPORTED at all.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedAssociation {
    public_ip: Option<String>,
    ip_owner_id: Option<String>,
    public_dns_name: Option<String>,
}

/// One network interface of an instance report: its own association, and the association of
/// each private IPv4 address it holds.
#[derive(Debug, Clone)]
struct ReportedInterface {
    interface: Option<ReportedAssociation>,
    addresses: Vec<(String, Option<ReportedAssociation>)>,
}

fn reported_association(node: roxmltree::Node<'_, '_>) -> ReportedAssociation {
    ReportedAssociation {
        public_ip: child_text(node, "publicIp"),
        ip_owner_id: child_text(node, "ipOwnerId"),
        public_dns_name: child_text(node, "publicDnsName"),
    }
}

/// The interfaces the instance `instance_id` reports, and the instance's own `dnsName`.
async fn described_interfaces(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> (String, Vec<ReportedInterface>) {
    let body = served(
        router,
        case,
        "DescribeInstances",
        &indexed("InstanceId", &[instance_id]),
    )
    .await;
    let parsed = document(case, &body);
    let instance = parsed
        .descendants()
        .find(|node| {
            node.is_element() && child_text(*node, "instanceId").as_deref() == Some(instance_id)
        })
        .unwrap_or_else(|| panic!("case {case:?}: no instance {instance_id} in {body}"));
    let interfaces = set_items(instance, "networkInterfaceSet")
        .into_iter()
        .map(|interface| ReportedInterface {
            interface: child(interface, "association").map(reported_association),
            addresses: set_items(interface, "privateIpAddressesSet")
                .into_iter()
                .map(|address| {
                    (
                        child_text(address, "privateIpAddress").unwrap_or_default(),
                        child(address, "association").map(reported_association),
                    )
                })
                .collect(),
        })
        .collect();
    (
        child_text(instance, "dnsName").unwrap_or_default(),
        interfaces,
    )
}

/// Look again until the instance reports an association against one of its private addresses,
/// or a deadline far past the service's own transition span passes — the association is
/// written by the call before this one, so what is polled for is the interface the attachment
/// made part of the instance rather than the clock.
async fn poll_until_an_address_is_associated(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> (String, Vec<ReportedInterface>) {
    let deadline = Instant::now() + poll_deadline();
    loop {
        let reported = described_interfaces(router, case, instance_id).await;
        let associated = reported
            .1
            .iter()
            .any(|interface| interface.addresses.iter().any(|(_, held)| held.is_some()));
        if associated {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no private address of {instance_id} ever reported an \
             association, last report {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Launch one instance, and answer its id.
async fn launch(router: &MultiServiceRouter, case: &str) -> String {
    let body = served(
        router,
        case,
        "RunInstances",
        &[
            p("ImageId", AMI),
            p("MinCount", "1"),
            p("MaxCount", "1"),
        ],
    )
    .await;
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances reported no instance: {body}"))
}

/// Allocate one Elastic IP address, as `(allocation id, public ip)`.
async fn allocate(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let body = served(router, case, "AllocateAddress", &[]).await;
    (
        element(&body, "allocationId")
            .unwrap_or_else(|| panic!("case {case:?}: no allocationId in {body}")),
        element(&body, "publicIp")
            .unwrap_or_else(|| panic!("case {case:?}: no publicIp in {body}")),
    )
}

/// Assert RULE 3 over one instance report: exactly ONE private address reports the public
/// address `public_ip`, that association carries its owner and the public DNS name the
/// INTERFACE holding the address reports for it, and the instance reports a public hostname of
/// its own. Answers the private address the public one is bound to.
fn assert_the_address_reports_the_whole_association(
    case: &str,
    account: &str,
    public_ip: &str,
    dns_name: &str,
    interfaces: &[ReportedInterface],
) -> String {
    // The name the interface holding the address reports for it, which is what every report of
    // that one association has to agree on.
    let holder = interfaces
        .iter()
        .filter_map(|interface| interface.interface.as_ref())
        .find(|association| association.public_ip.as_deref() == Some(public_ip))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the interface holding {public_ip} reports it in its own \
                 association, got {interfaces:?}"
            )
        });
    let held_name = holder.public_dns_name.clone().unwrap_or_default();
    // The private addresses this public one is bound to: exactly one, whichever other public
    // addresses the instance's interfaces hold.
    let bound: Vec<&(String, Option<ReportedAssociation>)> = interfaces
        .iter()
        .flat_map(|interface| interface.addresses.iter())
        .filter(|(_, held)| {
            held.as_ref()
                .and_then(|held| held.public_ip.as_deref())
                == Some(public_ip)
        })
        .collect();
    assert_eq!(
        bound.len(),
        1,
        "case {case:?}: exactly the private address {public_ip} is bound to reports it, got \
         {interfaces:?}"
    );
    let (address, held) = &bound[0];
    let held = held
        .as_ref()
        .unwrap_or_else(|| panic!("case {case:?}: {address} reports no association"));
    assert_eq!(
        held.public_dns_name.as_deref(),
        Some(held_name.as_str()),
        "case {case:?}: the association of private address {address} reports the public DNS \
         name its interface reports ({held_name:?}), got {held:?}"
    );
    assert_eq!(
        held.ip_owner_id.as_deref(),
        Some(account),
        "case {case:?}: the association of private address {address} reports the account that \
         owns the Elastic IP, got {held:?}"
    );

    // These instances run in the account's DEFAULT VPC, which hands its instances public
    // hostnames (`enableDnsHostnames`), so the name itself is reported everywhere it belongs.
    assert!(
        !held_name.is_empty(),
        "case {case:?}: the interface's own association reports the public DNS name of \
         {public_ip}, got {holder:?}"
    );
    assert!(
        !dns_name.is_empty(),
        "case {case:?}: the instance reports a public hostname of its own, got {dns_name:?}"
    );
    address.clone()
}

/// RULE 3: the association an instance reports against ONE private IPv4 address carries the
/// whole association the interface holding it reports — the public address, its owner and its
/// public DNS name — which the instance also reports as its `dnsName`.
///
/// The rows differ along how the address came to be associated: bound to the interface's
/// primary private address, swapped for another Elastic IP on that same address, and bound to
/// a SECONDARY private address of an interface the instance was given later.
#[tokio::test]
async fn an_instance_reports_the_whole_association_against_the_bound_private_address() {
    let (router, _engine) = super::ec2_query::emulator();
    let account = engine::emulator_account_id();

    // Row 1: the Elastic IP bound to the instance's primary private address.
    let case = "an Elastic IP bound to the instance's primary private address";
    let instance_id = launch(&router, case).await;
    let (allocation_id, public_ip) = allocate(&router, case).await;
    served(
        &router,
        case,
        "AssociateAddress",
        &[
            p("AllocationId", &allocation_id),
            p("InstanceId", &instance_id),
        ],
    )
    .await;
    let (dns_name, interfaces) =
        poll_until_an_address_is_associated(&router, case, &instance_id).await;
    assert_the_address_reports_the_whole_association(
        case,
        account,
        &public_ip,
        &dns_name,
        &interfaces,
    );

    // Row 2: a second Elastic IP swapped onto the same address — "if you associate an Elastic
    // IP address with an instance that has an existing Elastic IP address, the existing
    // address is disassociated from the instance" (`AssociateAddress`).
    let case = "a second Elastic IP swapped onto that same address";
    let (swapped_allocation, swapped_ip) = allocate(&router, case).await;
    served(
        &router,
        case,
        "AssociateAddress",
        &[
            p("AllocationId", &swapped_allocation),
            p("InstanceId", &instance_id),
        ],
    )
    .await;
    let (dns_name, interfaces) =
        poll_until_an_address_is_associated(&router, case, &instance_id).await;
    assert_the_address_reports_the_whole_association(
        case,
        account,
        &swapped_ip,
        &dns_name,
        &interfaces,
    );

    // Row 3: an Elastic IP bound to a SECONDARY private address of an interface the instance
    // was given after it was launched — the address-level association is on that one address,
    // beside the interface's own primary.
    let case = "an Elastic IP bound to a secondary private address";
    let instance_id = launch(&router, case).await;
    let subnet_id = {
        let body = served(
            &router,
            case,
            "DescribeInstances",
            &indexed("InstanceId", &[&instance_id]),
        )
        .await;
        let parsed = document(case, &body);
        let interface = parsed
            .descendants()
            .find(|node| node.is_element() && child(*node, "networkInterfaceId").is_some())
            .unwrap_or_else(|| panic!("case {case:?}: no interface in {body}"));
        child_text(interface, "subnetId")
            .unwrap_or_else(|| panic!("case {case:?}: the interface names no subnet in {body}"))
    };
    let created = served(
        &router,
        case,
        "CreateNetworkInterface",
        &[
            p("SubnetId", &subnet_id),
            p("SecondaryPrivateIpAddressCount", "1"),
        ],
    )
    .await;
    let network_interface_id = element(&created, "networkInterfaceId")
        .unwrap_or_else(|| panic!("case {case:?}: no interface minted: {created}"));
    let parsed = document(case, &created);
    let interface = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "networkInterfaceId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no interface element in {created}"));
    let secondary = set_items(interface, "privateIpAddressesSet")
        .into_iter()
        .find(|item| child_text(*item, "primary").as_deref() == Some("false"))
        .and_then(|item| child_text(item, "privateIpAddress"))
        .unwrap_or_else(|| panic!("case {case:?}: the interface holds no secondary address: {created}"));
    served(
        &router,
        case,
        "AttachNetworkInterface",
        &[
            p("NetworkInterfaceId", &network_interface_id),
            p("InstanceId", &instance_id),
            p("DeviceIndex", "1"),
        ],
    )
    .await;
    let (bound_allocation, bound_ip) = allocate(&router, case).await;
    served(
        &router,
        case,
        "AssociateAddress",
        &[
            p("AllocationId", &bound_allocation),
            p("NetworkInterfaceId", &network_interface_id),
            p("PrivateIpAddress", &secondary),
        ],
    )
    .await;
    let (dns_name, interfaces) =
        poll_until_an_address_is_associated(&router, case, &instance_id).await;
    // The Elastic IP is reported against the SECONDARY address it was bound to, and against
    // no other — the primary address of the instance's own interface reports the public
    // address AWS assigned THAT interface instead.
    let bound_to = assert_the_address_reports_the_whole_association(
        case,
        account,
        &bound_ip,
        &dns_name,
        &interfaces,
    );
    assert_eq!(
        bound_to, secondary,
        "case {case:?}: the Elastic IP was bound to {secondary}, so that is the address \
         reporting it, got {interfaces:?}"
    );
}
