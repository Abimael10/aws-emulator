// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `CopyImage`, driven over the wire the service actually speaks —
//! `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. A COPY IS A NEW AMI COMPUTED FROM THE STORED SOURCE: its own id and creation date, the
//!    REQUEST's name and description, the facts of the image's contents read off the source
//!    record (architecture, platform and its billing strings, the root device and how it is
//!    backed, the virtualization, enhanced networking, boot mode, kernel and RAM disk), and a
//!    private launch permission owned by the calling account — read back through
//!    `DescribeImages`, which reports it as it reports a registered AMI.
//! 2. THE COPY RECORDS ITS PARENT: "the ID of the source AMI from which the AMI was created"
//!    (`Image$SourceImageId`) and "the Region of the source AMI" (`Image$SourceImageRegion`)
//!    are the image copied and the Region the request named, which is what `GetImageAncestry`
//!    walks a lineage over.
//! 3. EACH BACKING SNAPSHOT IS COPIED TOO: every Amazon EBS device of the copy restores from
//!    a NEW snapshot of this account — one `DescribeSnapshots` reports — rather than from the
//!    source's own, with the encryption state the request, the source and the destination
//!    Region's encryption-by-default setting add up to.
//! 4. THE COPY IS ASYNCHRONOUS: the image is stored `pending` with the `available` move owed
//!    by the clock, and nothing but the clock moves it.
//! 5. ONE `ClientToken` NAMES ONE AMI: a replay of the same request answers the image the
//!    first call minted and copies nothing more.
//! 6. `CopyImageTags` COPIES YOUR USER-DEFINED TAGS and nothing else, with the request's own
//!    `TagSpecifications` deciding what the new AMI and the new snapshots carry.
//! 7. A REQUEST THAT CANNOT BE SERVED IS REFUSED WITH EC2'S OWN CODE AND NOTHING IS STORED.
//!
//! Each table row is a distinct input along what the rule generalises over — which kind of
//! source image, which Region form, which encryption combination, which tag set, which
//! completion duration, which refusal — so no row is the specification.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ec2::ops::types::{BlockDeviceMapping, EbsBlockDevice, Tag};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, IMAGE_PLATFORM_WINDOWS, IMAGE_ROOT_DEVICE_TYPE_EBS,
    IMAGE_ROOT_DEVICE_TYPE_INSTANCE_STORE, IMAGE_STATE_AVAILABLE, IMAGE_STATE_PENDING,
    IMAGE_VIRTUALIZATION_TYPE_PARAVIRTUAL, ImageSpec, MAX_KMS_KEY_ID_CHARS,
    PLATFORM_DETAILS_WINDOWS, USAGE_OPERATION_WINDOWS, WINDOWS_ROOT_DEVICE_NAME,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message};

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// The Region every call here is signed for: the destination Region of every copy, since
/// "the copy operation must be initiated in the destination Region" (`CopyImage`).
const REGION: &str = "us-west-2";

/// Another Region of the same partition, so a row can copy an AMI whose source Region is not
/// the Region the call is made in.
const OTHER_REGION: &str = "eu-west-1";

/// An account that is not the one this emulator serves, so an image of its own is another
/// account's.
const PARTNER_ACCOUNT: &str = "210987654321";

/// A well-formed long-form AMI id this store mints no image under.
const UNKNOWN_IMAGE_ID: &str = "ami-0123456789abcdef0";

/// A KMS key identifier of the `Key ID` form the member documents
/// (`CopyImageRequest$KmsKeyId`: "Key ID. For example,
/// 1234abcd-12ab-34cd-56ef-1234567890ab").
const A_KMS_KEY_ID: &str = "1234abcd-12ab-34cd-56ef-1234567890ab";

/// The HTTP status EC2 answers a `DryRun` request with: 412, carrying `DryRunOperation`.
const DRY_RUN_STATUS: u16 = 412;

/// How long a poll for a clock-owed move may run before the test gives up. Generous on
/// purpose: the test waits for the CONDITION — the copy reporting `available` — and this is
/// only the ceiling that keeps a broken emulator from hanging the suite.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// One `action` call signed for the Region this suite copies into.
async fn op(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// A tag, as a seeded image carries them.
fn tag(key: &str, value: &str) -> Tag {
    Tag {
        key: Some(key.to_string()),
        value: Some(value.to_string()),
    }
}

/// Store one AMI and answer its id, through the store's own writer — what a register
/// operation calls.
fn seed(engine: &engine::InMemoryEngine, spec: ImageSpec) -> String {
    engine.state::<Ec2State>().lock().insert_image(spec)
}

/// A private AMI of the calling account, named `name`, Amazon EBS-backed with the root
/// mapping the store derives for it.
fn own_spec(name: &str) -> ImageSpec {
    ImageSpec {
        name: name.to_string(),
        description: format!("The copy suite's {name}"),
        ..ImageSpec::default()
    }
}

/// One Amazon EBS device of a seeded image's own mappings, restored from `snapshot_id`.
fn mapping(device_name: &str, snapshot_id: &str, size: i32, encrypted: bool) -> BlockDeviceMapping {
    BlockDeviceMapping {
        device_name: Some(device_name.to_string()),
        virtual_name: None,
        no_device: None,
        ebs: Some(EbsBlockDevice {
            snapshot_id: Some(snapshot_id.to_string()),
            volume_size: Some(size),
            volume_type: Some("gp3".to_string()),
            delete_on_termination: Some(true),
            encrypted: Some(encrypted),
            iops: None,
            kms_key_id: None,
            throughput: None,
            outpost_arn: None,
            availability_zone: None,
            volume_initialization_rate: None,
            availability_zone_id: None,
            ebs_card_index: None,
        }),
    }
}

/// The copy parameters of a copy of `source_image_id` made from `source_region` under `name`,
/// with whatever else the row adds.
fn copy_params(name: &str, source_image_id: &str, source_region: &str, extra: Params) -> Params {
    let mut params = vec![
        p("Name", name),
        p("SourceImageId", source_image_id),
        p("SourceRegion", source_region),
    ];
    params.extend(extra);
    params
}

/// Copy one AMI and answer the id the call minted, or panic naming the case.
async fn copy_ok(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    source_image_id: &str,
    source_region: &str,
    extra: Params,
) -> String {
    let (status, body) = op(
        router,
        "CopyImage",
        &copy_params(name, source_image_id, source_region, extra),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: copying {source_image_id}: expected HTTP 200, got {status} with body \
         {body}"
    );
    element(&body, "imageId")
        .unwrap_or_else(|| panic!("case {case:?}: CopyImage reports the AMI it minted: {body}"))
}

/// One image as `DescribeImages` reported it: its direct members as the text the response
/// carried, its tags, and its block device mappings.
#[derive(Debug, Clone, Default)]
struct ReportedImage {
    members: BTreeMap<String, String>,
    tags: Vec<(String, String)>,
    /// `(device name, snapshot id, encrypted, volume size)` per Amazon EBS device.
    devices: Vec<(String, String, String, String)>,
}

impl ReportedImage {
    /// The text of the member `name`, or the empty string when the response reported none.
    fn member(&self, name: &str) -> &str {
        self.members.get(name).map_or("", String::as_str)
    }

    /// This image's tags as `(key, value)` pairs in key order, so a row compares against a
    /// set rather than against one reporting order.
    fn sorted_tags(&self) -> Vec<(String, String)> {
        let mut tags = self.tags.clone();
        tags.sort();
        tags
    }

    /// The snapshot id of every Amazon EBS device, in the order the mappings were reported.
    fn snapshot_ids(&self) -> Vec<String> {
        self.devices
            .iter()
            .map(|(_, snapshot_id, _, _)| snapshot_id.clone())
            .collect()
    }
}

/// The ONE image `DescribeImages` reports for `image_id`, read back over the wire.
async fn described(router: &MultiServiceRouter, case: &str, image_id: &str) -> ReportedImage {
    let (status, body) = op(router, "DescribeImages", &[p("ImageId.1", image_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeImages for {image_id} must succeed, got {status} with body {body}"
    );
    let document = super::wire::document(case, &body);
    let items = super::wire::set_items(document.root_element(), "imagesSet");
    assert_eq!(
        items.len(),
        1,
        "case {case:?}: exactly one image is reported for {image_id}, got {body}"
    );
    let mut reported = ReportedImage::default();
    for member in items[0].children().filter(roxmltree::Node::is_element) {
        match member.tag_name().name() {
            "tagSet" => {
                for entry in member.children().filter(roxmltree::Node::is_element) {
                    reported.tags.push((
                        super::wire::child_text(entry, "key").unwrap_or_default(),
                        super::wire::child_text(entry, "value").unwrap_or_default(),
                    ));
                }
            }
            "blockDeviceMapping" => {
                for entry in member.children().filter(roxmltree::Node::is_element) {
                    let device_name = super::wire::child_text(entry, "deviceName").unwrap_or_default();
                    let Some(ebs) = super::wire::child(entry, "ebs") else {
                        continue;
                    };
                    reported.devices.push((
                        device_name,
                        super::wire::child_text(ebs, "snapshotId").unwrap_or_default(),
                        super::wire::child_text(ebs, "encrypted").unwrap_or_default(),
                        super::wire::child_text(ebs, "volumeSize").unwrap_or_default(),
                    ));
                }
            }
            other => {
                reported
                    .members
                    .insert(other.to_string(), member.text().unwrap_or_default().to_string());
            }
        }
    }
    reported
}

/// The state and the owed-move entry the STORE holds for `image_id`, read without going
/// through an operation — so reading them materializes nothing and the answer does not
/// depend on how long the call before it took.
fn stored_state(engine: &engine::InMemoryEngine, image_id: &str) -> (String, bool) {
    let store = engine.state::<Ec2State>();
    let guard = store.lock();
    (
        guard
            .images
            .get(image_id)
            .and_then(|image| image.state.clone())
            .unwrap_or_default(),
        guard.image_available_at.contains_key(image_id),
    )
}

/// How many images the store holds.
fn stored_image_count(engine: &engine::InMemoryEngine) -> usize {
    engine.state::<Ec2State>().lock().images.len()
}

/// Look again until `image_id` reports `available`, and answer it as it reports then.
///
/// Waits for the CONDITION rather than for a span: the move is the clock's, so the poll
/// keeps looking until the store reports it or [`POLL_DEADLINE`] passes.
async fn available(router: &MultiServiceRouter, case: &str, image_id: &str) -> ReportedImage {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let reported = described(router, case, image_id).await;
        if reported.member("imageState") == IMAGE_STATE_AVAILABLE {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {image_id} never reached {IMAGE_STATE_AVAILABLE}, last state was \
             {:?}",
            reported.member("imageState")
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether `DescribeSnapshots` reports a snapshot under `snapshot_id`.
async fn snapshot_exists(router: &MultiServiceRouter, snapshot_id: &str) -> bool {
    let (status, body) = op(
        router,
        "DescribeSnapshots",
        &[p("SnapshotId.1", snapshot_id)],
    )
    .await;
    status == 200 && elements(&body, "snapshotId").contains(&snapshot_id.to_string())
}

/// The `TagSpecification` form fields for one specification of `resource_type` carrying
/// `tags`, numbered `index`.
fn tag_specification(index: usize, resource_type: &str, tags: &[(&str, &str)]) -> Params {
    let mut params = vec![p(
        &format!("TagSpecification.{index}.ResourceType"),
        resource_type,
    )];
    for (position, (key, value)) in tags.iter().enumerate() {
        params.push(p(
            &format!("TagSpecification.{index}.Tag.{}.Key", position + 1),
            *key,
        ));
        params.push(p(
            &format!("TagSpecification.{index}.Tag.{}.Value", position + 1),
            *value,
        ));
    }
    params
}

// ---------------------------------------------------------------------------
// RULES 1-4: the copy is a new AMI computed from the stored source
// ---------------------------------------------------------------------------

/// One row of the copy table: a kind of source image, and the facts of its contents the copy
/// has to report as its own.
struct CopyRow {
    case: &'static str,
    /// The source the row copies.
    source: ImageSpec,
    /// The Region the request names as holding the source.
    source_region: &'static str,
    /// The name the copy is given.
    name: &'static str,
    /// The description the request gives the copy, if any.
    description: Option<&'static str>,
    /// How many Amazon EBS devices the copy must restore from snapshots of its own.
    devices: usize,
}

fn copy_rows() -> Vec<CopyRow> {
    vec![
        CopyRow {
            case: "an Amazon EBS-backed Linux AMI of the account, copied inside its Region",
            source: own_spec("copy-source-linux"),
            source_region: REGION,
            name: "copy-of-linux",
            description: Some("A copy made in the destination Region"),
            devices: 1,
        },
        CopyRow {
            case: "a Windows AMI, whose platform and billing strings are facts of its contents",
            source: ImageSpec {
                platform: Some(IMAGE_PLATFORM_WINDOWS.to_string()),
                platform_details: PLATFORM_DETAILS_WINDOWS.to_string(),
                usage_operation: USAGE_OPERATION_WINDOWS.to_string(),
                root_device_name: WINDOWS_ROOT_DEVICE_NAME.to_string(),
                root_volume_size: 30,
                ..own_spec("copy-source-windows")
            },
            source_region: REGION,
            name: "copy-of-windows",
            description: None,
            devices: 1,
        },
        CopyRow {
            case: "a public AMI of another account, copied from another Region",
            source: ImageSpec {
                public: true,
                owner_id: PARTNER_ACCOUNT.to_string(),
                architecture: "arm64".to_string(),
                ..own_spec("copy-source-published")
            },
            source_region: OTHER_REGION,
            name: "copy-of-published",
            description: Some("A copy of a shared AMI"),
            devices: 1,
        },
        CopyRow {
            case: "an instance store-backed paravirtual AMI, which restores no snapshot",
            source: ImageSpec {
                root_device_type: IMAGE_ROOT_DEVICE_TYPE_INSTANCE_STORE.to_string(),
                virtualization_type: IMAGE_VIRTUALIZATION_TYPE_PARAVIRTUAL.to_string(),
                kernel_id: Some("aki-0123456789abcdef0".to_string()),
                ramdisk_id: Some("ari-0123456789abcdef0".to_string()),
                ..own_spec("copy-source-instance-store")
            },
            source_region: REGION,
            name: "copy-of-instance-store",
            description: None,
            devices: 0,
        },
        CopyRow {
            case: "an AMI with three Amazon EBS devices, every one of them copied",
            source: ImageSpec {
                block_device_mappings: Some(vec![
                    mapping("/dev/xvda", "snap-00000000000000aa1", 8, false),
                    mapping("/dev/sdf", "snap-00000000000000aa2", 100, false),
                    mapping("/dev/sdg", "snap-00000000000000aa3", 500, false),
                ]),
                ..own_spec("copy-source-three-devices")
            },
            source_region: REGION,
            name: "copy-of-three-devices",
            description: Some("A copy of a multi-volume AMI"),
            devices: 3,
        },
    ]
}

#[tokio::test]
async fn copy_is_a_new_ami_computed_from_the_stored_source() {
    let account = engine::emulator_account_id().to_string();
    for row in copy_rows() {
        let (router, engine) = emulator();
        let case = row.case;
        let source_id = seed(&engine, row.source.clone());
        let source = described(&router, case, &source_id).await;

        let extra = row
            .description
            .map(|description| vec![p("Description", description)])
            .unwrap_or_default();
        let copy_id = copy_ok(
            &router,
            case,
            row.name,
            &source_id,
            row.source_region,
            extra,
        )
        .await;
        assert_ne!(
            copy_id, source_id,
            "case {case:?}: a copy is an AMI of its own, not a second name for the source"
        );
        assert!(
            super::wire::is_long_id("ami", &copy_id),
            "case {case:?}: expected a long-form AMI id, got {copy_id:?}"
        );

        // RULE 4, read off the store so the answer cannot depend on how long the call took:
        // the image is `pending` and the `available` move is one the clock owes it.
        let (state, owes_a_move) = stored_state(&engine, &copy_id);
        assert_eq!(
            state, IMAGE_STATE_PENDING,
            "case {case:?}: a copy is handed back {IMAGE_STATE_PENDING}, got {state:?}"
        );
        assert!(
            owes_a_move,
            "case {case:?}: the copy's `available` move is owed by the clock"
        );

        let copy = available(&router, case, &copy_id).await;

        // RULE 1: the request's own words, and the contents' facts read off the source.
        assert_eq!(
            copy.member("name"),
            row.name,
            "case {case:?}: the copy carries the name the request gave it"
        );
        assert_eq!(
            copy.member("description"),
            row.description.unwrap_or_default(),
            "case {case:?}: the copy carries the description the request gave it"
        );
        for member in [
            "architecture",
            "platform",
            "platformDetails",
            "usageOperation",
            "rootDeviceName",
            "rootDeviceType",
            "virtualizationType",
            "sriovNetSupport",
            "enaSupport",
            "hypervisor",
            "imageType",
            "kernelId",
            "ramdiskId",
            "freeTierEligible",
        ] {
            assert_eq!(
                copy.member(member),
                source.member(member),
                "case {case:?}: <{member}> is a fact of the image's contents, so the copy \
                 reports the source's value"
            );
        }
        assert_eq!(
            copy.member("imageOwnerId"),
            account,
            "case {case:?}: the copy belongs to the account that asked for it"
        );
        assert_eq!(
            copy.member("isPublic"),
            "false",
            "case {case:?}: a copy is private until something shares it"
        );
        assert_eq!(
            copy.member("imageOwnerAlias"),
            "",
            "case {case:?}: an owner alias names an Amazon-published image, which a copy is not"
        );
        assert_eq!(
            copy.member("imageLocation"),
            format!("{account}/{}", row.name),
            "case {case:?}: the copy's manifest location is derived from its own owner and name"
        );
        assert_eq!(
            copy.member("deprecationTime"),
            "",
            "case {case:?}: a newly copied AMI carries no deprecation time"
        );
        assert!(
            !copy.member("creationDate").is_empty(),
            "case {case:?}: the copy reports when it was created"
        );
        assert!(
            copy.sorted_tags().is_empty(),
            "case {case:?}: a copy that asked for no tags carries none, got {:?}",
            copy.sorted_tags()
        );

        // RULE 2: the lineage the copy records.
        assert_eq!(
            copy.member("sourceImageId"),
            source_id,
            "case {case:?}: the copy records the AMI it was made from"
        );
        assert_eq!(
            copy.member("sourceImageRegion"),
            row.source_region,
            "case {case:?}: the copy records the Region the request named as holding the parent"
        );
        assert_eq!(
            source.member("sourceImageId"),
            "",
            "case {case:?}: the SOURCE was copied from nothing, so it records no parent"
        );

        // RULE 3: every Amazon EBS device restores from a new snapshot of this account.
        assert_eq!(
            copy.devices.len(),
            row.devices,
            "case {case:?}: expected {} copied Amazon EBS device(s), got {:?}",
            row.devices,
            copy.devices
        );
        let source_snapshots = source.snapshot_ids();
        let copied_snapshots = copy.snapshot_ids();
        for (index, (device_name, snapshot_id, encrypted, volume_size)) in
            copy.devices.iter().enumerate()
        {
            let (source_device, source_snapshot, _, source_size) = &source.devices[index];
            assert_eq!(
                device_name, source_device,
                "case {case:?}: the copy describes the same devices as the source"
            );
            assert_eq!(
                volume_size, source_size,
                "case {case:?}: device {device_name} holds the source device's data, so it is \
                 the same size"
            );
            assert_ne!(
                snapshot_id, source_snapshot,
                "case {case:?}: device {device_name} restores from a snapshot of the copy's own"
            );
            assert!(
                !source_snapshots.contains(snapshot_id),
                "case {case:?}: no device of the copy restores from one of the source's \
                 snapshots, got {snapshot_id:?} among {source_snapshots:?}"
            );
            assert_eq!(
                encrypted, "false",
                "case {case:?}: device {device_name} copies an unencrypted source and was not \
                 asked to encrypt"
            );
            assert!(
                snapshot_exists(&router, snapshot_id).await,
                "case {case:?}: the snapshot {snapshot_id} the copy restores device \
                 {device_name} from must be a snapshot this account holds"
            );
        }
        assert_eq!(
            copied_snapshots.len(),
            copied_snapshots
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            "case {case:?}: each device gets a snapshot of its own, got {copied_snapshots:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 2: the lineage a chain of copies records
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_chain_of_copies_reports_its_ancestry_to_the_root() {
    let case = "a copy of a copy of a registered AMI";
    let (router, engine) = emulator();
    let root = seed(&engine, own_spec("ancestry-chain-root"));

    let first = copy_ok(&router, case, "ancestry-chain-first", &root, REGION, Vec::new()).await;
    let second = copy_ok(
        &router,
        case,
        "ancestry-chain-second",
        &first,
        OTHER_REGION,
        Vec::new(),
    )
    .await;

    let (status, body) = op(&router, "GetImageAncestry", &[p("ImageId", &second)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetImageAncestry must succeed, got {status} with body {body}"
    );
    assert_eq!(
        elements(&body, "imageId"),
        vec![second.clone(), first.clone(), root.clone()],
        "case {case:?}: the chain runs from the specified AMI to the root AMI: {body}"
    );
    assert_eq!(
        elements(&body, "sourceImageId"),
        vec![first.clone(), root.clone()],
        "case {case:?}: each entry names the AMI it was copied from: {body}"
    );
    assert_eq!(
        elements(&body, "sourceImageRegion"),
        vec![OTHER_REGION.to_string(), REGION.to_string()],
        "case {case:?}: each entry names the Region its parent was copied from: {body}"
    );
}

// ---------------------------------------------------------------------------
// RULE 3: the encryption state the copy's snapshots carry
// ---------------------------------------------------------------------------

/// One row of the encryption table: `(case, the source's own encryption state, the
/// `Encrypted` parameter, whether encryption by default is enabled, the state the copy's
/// snapshots must carry)`.
type EncryptionRow = (&'static str, bool, Option<&'static str>, bool, &'static str);

fn encryption_rows() -> Vec<EncryptionRow> {
    vec![
        (
            "an unencrypted source nothing asked to encrypt",
            false,
            None,
            false,
            "false",
        ),
        (
            "an unencrypted source the request asks to encrypt",
            false,
            Some("true"),
            false,
            "true",
        ),
        (
            "an encrypted source, which cannot be copied unencrypted",
            true,
            None,
            false,
            "true",
        ),
        (
            "an encrypted source the request asks to encrypt too",
            true,
            Some("true"),
            false,
            "true",
        ),
        (
            "an unencrypted source copied into a Region that encrypts by default",
            false,
            None,
            true,
            "true",
        ),
    ]
}

#[tokio::test]
async fn the_copys_snapshots_carry_the_encryption_state_the_request_source_and_region_add_up_to() {
    for (case, source_encrypted, requested, encrypt_by_default, expected) in encryption_rows() {
        let (router, engine) = emulator();
        let source_id = seed(
            &engine,
            ImageSpec {
                block_device_mappings: Some(vec![mapping(
                    "/dev/xvda",
                    "snap-00000000000000bb1",
                    8,
                    source_encrypted,
                )]),
                ..own_spec("encryption-source")
            },
        );
        if encrypt_by_default {
            let (status, body) = op(&router, "EnableEbsEncryptionByDefault", &[]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: enabling encryption by default: got {status} with body {body}"
            );
        }

        let extra = requested
            .map(|value| vec![p("Encrypted", value)])
            .unwrap_or_default();
        let copy_id = copy_ok(&router, case, "encryption-copy", &source_id, REGION, extra).await;
        let copy = available(&router, case, &copy_id).await;
        let encrypted: Vec<String> = copy
            .devices
            .iter()
            .map(|(_, _, encrypted, _)| encrypted.clone())
            .collect();
        assert_eq!(
            encrypted,
            vec![expected.to_string()],
            "case {case:?}: expected the copy's snapshot to report encrypted={expected:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 5: one `ClientToken` names one AMI
// ---------------------------------------------------------------------------

#[tokio::test]
async fn one_client_token_names_one_copy() {
    let case = "a copy replayed under the same client token";
    let (router, engine) = emulator();
    let source_id = seed(&engine, own_spec("token-source"));
    let token = "copy-image-token-1";

    let first = copy_ok(
        &router,
        case,
        "token-copy",
        &source_id,
        REGION,
        vec![p("ClientToken", token)],
    )
    .await;
    let after_first = stored_image_count(&engine);

    // The SAME request again: the token names the AMI the first call minted, so the replay
    // reports it and copies nothing — not even a name clash, which this request would
    // otherwise be refused for.
    let replayed = copy_ok(
        &router,
        case,
        "token-copy",
        &source_id,
        REGION,
        vec![p("ClientToken", token)],
    )
    .await;
    assert_eq!(
        replayed, first,
        "case {case:?}: a replay reports the AMI the token minted"
    );
    assert_eq!(
        stored_image_count(&engine),
        after_first,
        "case {case:?}: a replay copies nothing more"
    );

    // Another token is another request, so it mints another AMI.
    let other = copy_ok(
        &router,
        case,
        "token-copy-other",
        &source_id,
        REGION,
        vec![p("ClientToken", "copy-image-token-2")],
    )
    .await;
    assert_ne!(
        other, first,
        "case {case:?}: a different token is a different request"
    );
    assert_eq!(
        stored_image_count(&engine),
        after_first + 1,
        "case {case:?}: exactly one more AMI is stored"
    );
}

// ---------------------------------------------------------------------------
// RULE 6: which tags the copy carries
// ---------------------------------------------------------------------------

/// The tags every tag row's source image carries: one user-defined tag, one system tag of
/// the reserved `aws:` namespace, and one shared tag.
fn source_tags() -> Vec<Tag> {
    vec![
        tag("App", "web"),
        tag("aws:cloudformation:stack-name", "golden"),
        tag("ec2:SharedTag/Team", "platform"),
    ]
}

/// One row of the tag table: `(case, whether the source is the account's own,
/// `CopyImageTags`, the image tags the request asks for, the tags the copy must carry)`.
type TagRow = (
    &'static str,
    bool,
    Option<&'static str>,
    &'static [(&'static str, &'static str)],
    &'static [(&'static str, &'static str)],
);

fn tag_rows() -> Vec<TagRow> {
    vec![
        (
            "the default: your user-defined AMI tags are not copied",
            true,
            None,
            &[],
            &[],
        ),
        (
            "CopyImageTags copies the user-defined tags of your own AMI, never a system tag",
            true,
            Some("true"),
            &[],
            &[("App", "web"), ("ec2:SharedTag/Team", "platform")],
        ),
        (
            "CopyImageTags=false is the default spelled out",
            true,
            Some("false"),
            &[],
            &[],
        ),
        (
            "of another account's shared AMI, only the shared tags are yours to copy",
            false,
            Some("true"),
            &[],
            &[("ec2:SharedTag/Team", "platform")],
        ),
        (
            "the request's own tag specification is applied beside the copied tags",
            true,
            Some("true"),
            &[("Owner", "TeamB")],
            &[
                ("App", "web"),
                ("Owner", "TeamB"),
                ("ec2:SharedTag/Team", "platform"),
            ],
        ),
        (
            "a tag the request gives the copy replaces the copied value of that key",
            true,
            Some("true"),
            &[("App", "api")],
            &[("App", "api"), ("ec2:SharedTag/Team", "platform")],
        ),
        (
            "tags the request asks for, with nothing copied",
            true,
            None,
            &[("Owner", "TeamB")],
            &[("Owner", "TeamB")],
        ),
    ]
}

#[tokio::test]
async fn the_copy_carries_the_tags_the_request_asks_for() {
    for (case, own_source, copy_image_tags, requested, expected) in tag_rows() {
        let (router, engine) = emulator();
        let spec = ImageSpec {
            tags: source_tags(),
            public: !own_source,
            owner_id: if own_source {
                engine::emulator_account_id().to_string()
            } else {
                PARTNER_ACCOUNT.to_string()
            },
            ..own_spec("tag-source")
        };
        let source_id = seed(&engine, spec);

        let mut extra = Vec::new();
        if let Some(flag) = copy_image_tags {
            extra.push(p("CopyImageTags", flag));
        }
        if !requested.is_empty() {
            extra.extend(tag_specification(1, "image", requested));
        }
        let copy_id = copy_ok(&router, case, "tag-copy", &source_id, REGION, extra).await;
        let copy = available(&router, case, &copy_id).await;
        let expected: Vec<(String, String)> = {
            let mut pairs: Vec<(String, String)> = expected
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect();
            pairs.sort();
            pairs
        };
        assert_eq!(
            copy.sorted_tags(),
            expected,
            "case {case:?}: expected the copy to carry {expected:?}"
        );
    }
}

#[tokio::test]
async fn a_snapshot_tag_specification_tags_the_copys_snapshots() {
    let case = "a tag specification for the snapshots, beside one for the AMI";
    let (router, engine) = emulator();
    let source_id = seed(
        &engine,
        ImageSpec {
            block_device_mappings: Some(vec![
                mapping("/dev/xvda", "snap-00000000000000cc1", 8, false),
                mapping("/dev/sdf", "snap-00000000000000cc2", 20, false),
            ]),
            ..own_spec("snapshot-tag-source")
        },
    );

    let mut extra = tag_specification(1, "image", &[("Owner", "TeamA")]);
    extra.extend(tag_specification(2, "snapshot", &[("Backup", "nightly")]));
    let copy_id = copy_ok(&router, case, "snapshot-tag-copy", &source_id, REGION, extra).await;
    let copy = available(&router, case, &copy_id).await;
    assert_eq!(
        copy.sorted_tags(),
        vec![("Owner".to_string(), "TeamA".to_string())],
        "case {case:?}: the AMI carries the tags the `image` specification named"
    );

    for snapshot_id in copy.snapshot_ids() {
        let (status, body) = op(
            &router,
            "DescribeSnapshots",
            &[p("SnapshotId.1", &snapshot_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeSnapshots for {snapshot_id}: got {status} with body {body}"
        );
        assert_eq!(
            elements(&body, "key"),
            vec!["Backup".to_string()],
            "case {case:?}: the same tag is applied to all the new snapshots: {body}"
        );
        assert_eq!(
            elements(&body, "value"),
            vec!["nightly".to_string()],
            "case {case:?}: the snapshot tag carries the value the request gave: {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 7: what the operation refuses
// ---------------------------------------------------------------------------

/// One row of the refusal table: `(case, the parameters, the HTTP status, the error code)`.
type RefusalRow = (&'static str, Params, u16, &'static str);

/// The code the request walk answers a REQUIRED member the request did not carry at all,
/// before any handler runs: EC2's own `MissingParameter`, the same code the handler answers
/// for one of the three `clientOptional` required members that reaches it EMPTY.
const ABSENT_REQUIRED_MEMBER_CODE: &str = "MissingParameter";

/// The refusals that do not depend on a stored source, each a distinct input along what the
/// rule generalises over.
fn refusal_rows(source_id: &str, duplicate_name: &str) -> Vec<RefusalRow> {
    vec![
        (
            "no name at all",
            vec![p("SourceImageId", source_id), p("SourceRegion", REGION)],
            400,
            ABSENT_REQUIRED_MEMBER_CODE,
        ),
        (
            "a name outside the published character set",
            copy_params("not a name: ?", source_id, REGION, Vec::new()),
            400,
            "InvalidAMIName.Malformed",
        ),
        (
            "a name another AMI of the account already carries",
            copy_params(duplicate_name, source_id, REGION, Vec::new()),
            400,
            "InvalidAMIName.Duplicate",
        ),
        (
            "no source image at all",
            vec![p("Name", "copy-no-source"), p("SourceRegion", REGION)],
            400,
            ABSENT_REQUIRED_MEMBER_CODE,
        ),
        (
            "a source image id the request carries empty",
            copy_params("copy-empty-source", "", REGION, Vec::new()),
            400,
            "MissingParameter",
        ),
        (
            "a source that is not an AMI id at all",
            copy_params("copy-bad-source", "image-1", REGION, Vec::new()),
            400,
            "InvalidAMIID.Malformed",
        ),
        (
            "a well-formed source id the account can see no image for",
            copy_params("copy-unknown-source", UNKNOWN_IMAGE_ID, REGION, Vec::new()),
            400,
            "InvalidAMIID.NotFound",
        ),
        (
            "no source Region at all",
            vec![p("Name", "copy-no-region"), p("SourceImageId", source_id)],
            400,
            ABSENT_REQUIRED_MEMBER_CODE,
        ),
        (
            "a source Region the request carries empty",
            copy_params("copy-empty-region", source_id, "", Vec::new()),
            400,
            "MissingParameter",
        ),
        (
            "a source Region that names no Region",
            copy_params("copy-bad-region", source_id, "us-west-9", Vec::new()),
            400,
            "InvalidParameterValue",
        ),
        (
            "a destination Outpost, which this Region has none of",
            copy_params(
                "copy-to-outpost",
                source_id,
                REGION,
                vec![p(
                    "DestinationOutpostArn",
                    "arn:aws:outposts:us-west-2:123456789012:outpost/op-0123456789abcdef0",
                )],
            ),
            400,
            "InvalidParameterValue",
        ),
        (
            "a destination Local Zone, which this Region has none of",
            copy_params(
                "copy-to-local-zone",
                source_id,
                REGION,
                vec![p("DestinationAvailabilityZone", "us-west-2-lax-1a")],
            ),
            400,
            "InvalidParameterValue",
        ),
        (
            "a destination Local Zone named by id",
            copy_params(
                "copy-to-local-zone-id",
                source_id,
                REGION,
                vec![p("DestinationAvailabilityZoneId", "usw2-lax1-az1")],
            ),
            400,
            "InvalidParameterValue",
        ),
        (
            "two destinations, where only one may be specified",
            copy_params(
                "copy-to-two-destinations",
                source_id,
                REGION,
                vec![
                    p("DestinationAvailabilityZone", "us-west-2-lax-1a"),
                    p("DestinationAvailabilityZoneId", "usw2-lax1-az1"),
                ],
            ),
            400,
            "InvalidParameterCombination",
        ),
        (
            "a KMS key for a copy that was not asked to be encrypted",
            copy_params(
                "copy-with-key",
                source_id,
                REGION,
                vec![p("KmsKeyId", A_KMS_KEY_ID)],
            ),
            400,
            "InvalidParameterValue",
        ),
        (
            "a KMS key identifier longer than AWS KMS accepts",
            copy_params(
                "copy-with-long-key",
                source_id,
                REGION,
                vec![
                    p("Encrypted", "true"),
                    p("KmsKeyId", "k".repeat(MAX_KMS_KEY_ID_CHARS + 1)),
                ],
            ),
            400,
            "InvalidParameterValue",
        ),
        (
            "a completion duration that is not a whole 15 minute increment",
            copy_params(
                "copy-with-odd-duration",
                source_id,
                REGION,
                vec![p("SnapshotCopyCompletionDurationMinutes", "20")],
            ),
            400,
            "InvalidParameterValue",
        ),
        (
            "a completion duration of nothing at all",
            copy_params(
                "copy-with-zero-duration",
                source_id,
                REGION,
                vec![p("SnapshotCopyCompletionDurationMinutes", "0")],
            ),
            400,
            "InvalidParameterValue",
        ),
        (
            "a completion duration past the published window",
            copy_params(
                "copy-with-long-duration",
                source_id,
                REGION,
                vec![p("SnapshotCopyCompletionDurationMinutes", "2895")],
            ),
            400,
            "InvalidParameterValue",
        ),
        (
            "a tag specification naming a resource type this call does not create",
            copy_params(
                "copy-with-bad-tag-type",
                source_id,
                REGION,
                tag_specification(1, "volume", &[("Owner", "TeamA")]),
            ),
            400,
            "InvalidParameterValue",
        ),
        (
            "a tag specification naming no resource type at all",
            copy_params(
                "copy-with-no-tag-type",
                source_id,
                REGION,
                vec![
                    p("TagSpecification.1.Tag.1.Key", "Owner"),
                    p("TagSpecification.1.Tag.1.Value", "TeamA"),
                ],
            ),
            400,
            "MissingParameter",
        ),
        (
            "a dry run of a copy that would have worked",
            copy_params(
                "copy-dry-run",
                source_id,
                REGION,
                vec![p("DryRun", "true")],
            ),
            DRY_RUN_STATUS,
            "DryRunOperation",
        ),
        (
            "a dry run of a copy whose source does not exist, which still asks about permission",
            copy_params(
                "copy-dry-run-unknown-source",
                UNKNOWN_IMAGE_ID,
                REGION,
                vec![p("DryRun", "true")],
            ),
            DRY_RUN_STATUS,
            "DryRunOperation",
        ),
    ]
}

#[tokio::test]
async fn a_copy_that_cannot_be_served_is_refused_and_stores_nothing() {
    let (router, engine) = emulator();
    let source_id = seed(&engine, own_spec("refusal-source"));
    let duplicate_name = "refusal-duplicate-name";
    seed(&engine, own_spec(duplicate_name));
    let before = stored_image_count(&engine);

    for (case, params, status, code) in refusal_rows(&source_id, duplicate_name) {
        let (answered, body) = op(&router, "CopyImage", &params).await;
        assert_eq!(
            answered, status,
            "case {case:?}: expected HTTP {status}, got {answered} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            code,
            "case {case:?}: expected {code}, got {}: {}",
            error_code(&body),
            error_message(&body)
        );
        assert_eq!(
            stored_image_count(&engine),
            before,
            "case {case:?}: a refused copy stores no image"
        );
    }
}

/// The two refusals whose SENTENCE names the resource the request got wrong, so a caller can
/// act on it: the name another AMI holds, and the encryption state an encrypted source
/// cannot be copied with.
#[tokio::test]
async fn a_refusal_names_what_the_request_got_wrong() {
    let (router, engine) = emulator();
    let held = seed(&engine, own_spec("worded-refusal-source"));
    let encrypted_source = seed(
        &engine,
        ImageSpec {
            block_device_mappings: Some(vec![mapping(
                "/dev/xvda",
                "snap-00000000000000dd1",
                8,
                true,
            )]),
            ..own_spec("worded-refusal-encrypted-source")
        },
    );

    let (status, body) = op(
        &router,
        "CopyImage",
        &copy_params("worded-refusal-source", &held, REGION, Vec::new()),
    )
    .await;
    assert_eq!(status, 400, "a duplicate name is refused: {body}");
    assert_eq!(
        error_message(&body),
        format!("AMI name worded-refusal-source is already in use by AMI {held}"),
        "the refusal names the name at fault and the AMI that holds it: {body}"
    );

    let (status, body) = op(
        &router,
        "CopyImage",
        &copy_params(
            "copy-unencrypted",
            &encrypted_source,
            REGION,
            vec![p("Encrypted", "false")],
        ),
    )
    .await;
    assert_eq!(status, 400, "an unencrypted copy of an encrypted source: {body}");
    assert_eq!(error_code(&body), "InvalidParameterValue", "body {body}");
    assert_eq!(
        error_message(&body),
        "Invalid value 'false' for encrypted. \
         You cannot create an unencrypted copy of an encrypted snapshot.",
        "the refusal states the rule the request broke: {body}"
    );
}

/// A time-based copy — one that asks for a completion duration — is accepted at each end of
/// the published window, and the snapshots it makes report that duration and the transfer
/// type that goes with it.
#[tokio::test]
async fn a_time_based_copy_reports_its_completion_duration_on_each_snapshot() {
    for (case, duration) in [
        ("the smallest increment the member accepts", "15"),
        ("a duration in the middle of the window", "120"),
        ("the longest duration the window allows", "2880"),
    ] {
        let (router, engine) = emulator();
        let source_id = seed(&engine, own_spec("time-based-source"));
        let copy_id = copy_ok(
            &router,
            case,
            "time-based-copy",
            &source_id,
            REGION,
            vec![p("SnapshotCopyCompletionDurationMinutes", duration)],
        )
        .await;
        let copy = available(&router, case, &copy_id).await;
        let snapshots = copy.snapshot_ids();
        assert_eq!(
            snapshots.len(),
            1,
            "case {case:?}: the source has one Amazon EBS device, got {snapshots:?}"
        );
        let (status, body) = op(
            &router,
            "DescribeSnapshots",
            &[p("SnapshotId.1", &snapshots[0])],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeSnapshots: got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "transferType").unwrap_or_default(),
            "time-based",
            "case {case:?}: a copy that asked for a completion duration is time-based: {body}"
        );
        assert_eq!(
            element(&body, "completionDurationMinutes").unwrap_or_default(),
            duration,
            "case {case:?}: the duration applies to each snapshot of the AMI: {body}"
        );
    }
}

/// The copy of an AMI that is itself still `pending` is a copy like any other: the state of
/// the source is a fact of the source, and the store holds the parent link either way.
#[tokio::test]
async fn the_source_of_a_copy_may_itself_be_a_copy_in_flight() {
    let case = "a copy taken while the source copy is still pending";
    let (router, engine) = emulator();
    let root = seed(&engine, own_spec("in-flight-root"));
    let first = copy_ok(&router, case, "in-flight-first", &root, REGION, Vec::new()).await;
    let (state, _) = stored_state(&engine, &first);
    assert_eq!(
        state, IMAGE_STATE_PENDING,
        "case {case:?}: the first copy is still being assembled"
    );

    let second = copy_ok(&router, case, "in-flight-second", &first, REGION, Vec::new()).await;
    let copy = available(&router, case, &second).await;
    assert_eq!(
        copy.member("sourceImageId"),
        first,
        "case {case:?}: the copy records the AMI it was made from"
    );
    assert_eq!(
        copy.member("rootDeviceType"),
        IMAGE_ROOT_DEVICE_TYPE_EBS,
        "case {case:?}: how the root device is backed is a fact of the image's contents"
    );
}

/// What a copy's `ClientToken` and its owed `available` move mean once the AMI the token
/// minted is DEREGISTERED into the Recycle Bin and restored out of it again.
///
/// The bin is reached through the store's own writer, the way `image_recycle_bin`'s suite
/// reaches it, because `DeregisterImage` is still a scaffold with a stub issue of its own.
#[tokio::test]
async fn a_copy_that_was_deregistered_is_reported_absent_and_keeps_its_owed_move() {
    let case = "a copy recycled before its `available` move fell due";
    let (router, engine) = emulator();
    let source_id = seed(&engine, own_spec("recycled-copy-source"));
    let token = "copy-image-token-recycled";
    let copy_id = copy_ok(
        &router,
        case,
        "recycled-copy",
        &source_id,
        REGION,
        vec![p("ClientToken", token)],
    )
    .await;

    // Into the bin, with the retention period a Recycle Bin rule carries.
    assert!(
        engine
            .state::<Ec2State>()
            .lock()
            .recycle_image(&copy_id, Duration::from_secs(24 * 60 * 60)),
        "case {case:?}: the copy must move into the Recycle Bin"
    );

    // A replay of the same request now names an AMI the account can no longer see, which is
    // the answer every reader of an AMI id gives for one — and it copies nothing more.
    let before = stored_image_count(&engine);
    let (status, body) = op(
        &router,
        "CopyImage",
        &copy_params("recycled-copy", &source_id, REGION, vec![p("ClientToken", token)]),
    )
    .await;
    assert_eq!(status, 400, "case {case:?}: got {status} with body {body}");
    assert_eq!(
        error_code(&body),
        "InvalidAMIID.NotFound",
        "case {case:?}: the token names an AMI the account cannot see: {}",
        error_message(&body)
    );
    assert_eq!(
        stored_image_count(&engine),
        before,
        "case {case:?}: a replay copies nothing more"
    );

    // Restored out of the bin, the copy still owes the move the clock promised it, so it
    // reports `available` rather than staying `pending` for ever.
    assert!(
        engine
            .state::<Ec2State>()
            .lock()
            .restore_recycled_image(&copy_id),
        "case {case:?}: the copy must come back out of the Recycle Bin"
    );
    let restored = available(&router, case, &copy_id).await;
    assert_eq!(
        restored.member("sourceImageId"),
        source_id,
        "case {case:?}: the restored copy is the one the token minted"
    );
}
