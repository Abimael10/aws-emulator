// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `GetInstanceUefiData`, driven over the wire the service actually
//! speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. the operation SERVES THE NON-VOLATILE UEFI VARIABLE STORE THE NAMED INSTANCE HOLDS
//!    ("base64 representation of the non-volatile UEFI variable store",
//!    `GetInstanceUefiDataResult$UefiData`), beside that instance's own id: for an instance
//!    launched from an AMI that was REGISTERED with one, that is the exact copy the register
//!    supplied — "when you use register-image to create an AMI, you can create an exact copy
//!    of your variable store by passing the UEFI data in the UefiData parameter" — the same
//!    bytes `DescribeImageAttribute` reports as that image's `uefiData`;
//! 2. an instance whose AMI carries no store of its own comes up with its FIRMWARE's: a store
//!    that opens with the magic every store AWS hands out opens with, carries the zlib stream
//!    of its variables ("this is a base64 encoded and zlib compressed binary value") and
//!    holds the non-volatile globally defined variables of the UEFI specification — the
//!    platform language, the boot timeout, the boot option for the loader of the instance's
//!    own architecture and the order the options are tried in, and the secure-boot operating
//!    mode;
//! 3. only a UEFI machine has UEFI variables: an instance that boots the LEGACY BIOS — by its
//!    AMI's boot mode or because its instance type offers no UEFI — reports no store at all,
//!    and which way an instance boots is what `DescribeInstances` reports as its
//!    `currentInstanceBootMode`;
//! 4. a store is what the firmware WROTE, so an instance whose guest has not come up yet
//!    holds none and one whose guest has holds one;
//! 5. the operation is a READ: two calls about one instance answer exactly the same bytes and
//!    neither moves the instance;
//! 6. `DryRun` asks whether the request WOULD have succeeded, so it is answered AFTER the
//!    instance has been resolved: a probe naming an instance the account holds answers
//!    `DryRunOperation` and reports no variable store, a probe naming an id that resolves to
//!    nothing is refused with that reason instead, and neither writes anything;
//! 7. an instance id outside the form EC2 issues is `InvalidInstanceID.Malformed`, and every
//!    id inside that form the account holds no instance under — including one DERIVED from
//!    the launched instance's own id, which is what the recorded contract of this operation
//!    probes with — is `InvalidInstanceID.NotFound`.
//!
//! Each table row is a distinct input along what the rule generalises over — the bytes a
//! register supplied, the AMI's architecture and boot mode, the instance type, and for the id
//! rule every way an id can fall outside the issued form beside the ids inside it that name
//! nothing — so no row is the specification.

use std::time::Duration;

use ec2::state::{
    BOOT_MODE_LEGACY_BIOS, BOOT_MODE_UEFI, BOOT_MODE_UEFI_PREFERRED,
    ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec, UefiVariable,
    decode_uefi_variable_store,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, member};
use super::volume_lifecycle::{REGION, p, zone_name};

/// The HTTP status EC2 answers a refusal about the request with.
const REFUSAL_STATUS: u16 = 400;

/// The HTTP status `DryRunOperation` carries: the request was refused because a precondition
/// the CALLER set holds, not because anything in it was wrong.
const DRY_RUN_STATUS: u16 = 412;

/// How long a poll waits for a move the clock owes — a launch settling and the guest coming
/// up with it. Generous next to the service's own transition span, so a loaded host still
/// gets there; nothing asserts how long it took.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How often such a poll looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The eight bytes every UEFI variable store AWS hands out OPENS with, which the recorded
/// contract of this operation asserts on the decoded value. Pinned here as the literal a
/// client checks rather than read from the crate, so the wire fact is the expectation.
const STORE_MAGIC: &[u8] = b"AMZNUEFI";

/// `EFI_VARIABLE_NON_VOLATILE`, the attribute bit that makes a variable one of the "only
/// non-volatile variables [that] are stored" in a stored variable store (UEFI specification,
/// "Variable Attributes").
const NON_VOLATILE: u32 = 0x0000_0001;

/// `EFI_GLOBAL_VARIABLE`, the vendor GUID the UEFI specification's globally defined variables
/// live under — `8be4df61-93ca-11d2-aa0d-00e098032b8c` in `EFI_GUID` byte order (UEFI
/// specification, "Globally Defined Variables").
const GLOBAL_VARIABLE_GUID: [u8; 16] = [
    0x61, 0xdf, 0xe4, 0x8b, 0xca, 0x93, 0xd2, 0x11, 0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03, 0x2b, 0x8c,
];

/// The variables a firmware's own store holds, in the order it lists them: the globally
/// defined variables of the UEFI specification's "Globally Defined Variables" that it marks
/// non-volatile — the platform's languages, the boot timeout, the one boot option and the boot
/// order, and the three secure-boot mode variables.
const FIRMWARE_VARIABLES: [&str; 8] = [
    "PlatformLangCodes",
    "PlatformLang",
    "Timeout",
    "Boot0000",
    "BootOrder",
    "SetupMode",
    "AuditMode",
    "DeployedMode",
];

/// One ec2Query call through the shared harness, signed for the Region this suite works in.
async fn api(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One emulator over its own engine, with `specs` seeded as AMIs of the calling account the
/// way a register operation would store them — answered beside their image ids, in order.
fn emulator(specs: Vec<ImageSpec>) -> (MultiServiceRouter, Vec<String>) {
    let (router, engine) = super::ec2_query::emulator();
    let image_ids = {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        specs
            .into_iter()
            .map(|spec| guard.insert_image(spec))
            .collect()
    };
    (router, image_ids)
}

/// An AMI of `architecture` declaring the boot mode `boot_mode`, named `name`.
fn image(name: &str, architecture: &str, boot_mode: Option<&str>) -> ImageSpec {
    ImageSpec {
        name: name.to_string(),
        architecture: architecture.to_string(),
        boot_mode: boot_mode.map(str::to_string),
        ..ImageSpec::default()
    }
}

/// Launch one instance of `instance_type` from `image_id`, answering its id.
async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    image_id: &str,
    instance_type: &str,
) -> String {
    let (status, body) = api(
        router,
        "RunInstances",
        &[
            p("ImageId", image_id),
            p("InstanceType", instance_type),
            p("MinCount", "1"),
            p("MaxCount", "1"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances must succeed, got {status} with body {body}"
    );
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances must report an instanceId: {body}"))
}

/// The variable store `GetInstanceUefiData` reports for `instance_id`, or `None` for an
/// instance that holds none — having pinned that the call succeeded and that the response
/// names the instance the service resolved.
async fn reported_store(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> Option<String> {
    let (status, body) = api(
        router,
        "GetInstanceUefiData",
        &[p("InstanceId", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetInstanceUefiData must succeed, got {status} with body {body}"
    );
    assert_eq!(
        member(&body, "instanceId"),
        Some(instance_id.to_string()),
        "case {case:?}: the response must name the instance the service resolved, got {body}"
    );
    member(&body, "uefiData")
}

/// Wait until the launch of `instance_id` has settled, which is when its guest has come up:
/// `DescribeInstances` reports it `running`. The CONDITION is what is waited for, never an
/// interval.
async fn until_running(router: &MultiServiceRouter, case: &str, instance_id: &str) {
    let started = tokio::time::Instant::now();
    loop {
        let state = super::ec2_query::described_instance_state(router, REGION, instance_id).await;
        if state == "running" {
            return;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected {instance_id} to reach running, still {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The variable store of `instance_id` once its firmware has come up, polled for rather than
/// waited out.
async fn settled_store(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    until_running(router, case, instance_id).await;
    let started = tokio::time::Instant::now();
    loop {
        if let Some(store) = reported_store(router, case, instance_id).await {
            return store;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected {instance_id} to report a UEFI variable store once its \
             firmware had come up, got none"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The bytes a reported store carries, read the way a client reads one and WITHOUT the
/// service's own decoder: base64, then the magic every store opens with, then the zlib stream
/// of the variables.
fn store_bytes(case: &str, store: &str) -> Vec<u8> {
    let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, store)
        .unwrap_or_else(|error| panic!("case {case:?}: the store must be base64 ({error}): {store}"));
    assert_eq!(
        decoded.get(..STORE_MAGIC.len()),
        Some(STORE_MAGIC),
        "case {case:?}: expected the store to open with {STORE_MAGIC:?}, got {:?}",
        decoded.get(..STORE_MAGIC.len())
    );
    let mut binary = Vec::new();
    let compressed = &decoded[STORE_MAGIC.len()..];
    std::io::Read::read_to_end(
        &mut flate2::read::ZlibDecoder::new(compressed),
        &mut binary,
    )
    .unwrap_or_else(|error| {
        panic!("case {case:?}: the store's records must be a zlib stream ({error})")
    });
    assert!(
        !binary.is_empty(),
        "case {case:?}: expected the zlib stream to carry the variable records, got none"
    );
    binary
}

/// The variables a reported store holds, read through the crate's own reader of one, having
/// first pinned the envelope a client sees ([`store_bytes`]) and that every variable's name is
/// really in the compressed records.
fn store_variables(case: &str, store: &str) -> Vec<UefiVariable> {
    let binary = store_bytes(case, store);
    let variables = decode_uefi_variable_store(store)
        .unwrap_or_else(|| panic!("case {case:?}: the store must read as a variable store"));
    for variable in &variables {
        let name = ucs2(&variable.name);
        assert!(
            binary.windows(name.len()).any(|window| window == name),
            "case {case:?}: expected the records to carry the name {:?}",
            variable.name
        );
    }
    variables
}

/// `text` as the little-endian UCS-2 a UEFI structure writes a name or a description in,
/// without the terminator.
fn ucs2(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// The text a NUL-terminated little-endian UCS-2 string at the head of `bytes` holds.
fn from_ucs2(bytes: &[u8]) -> String {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|unit| *unit != 0)
        .filter_map(|unit| char::from_u32(u32::from(unit)))
        .collect()
}

/// The variable `name` of `variables`.
fn variable<'a>(case: &str, variables: &'a [UefiVariable], name: &str) -> &'a UefiVariable {
    variables
        .iter()
        .find(|variable| variable.name == name)
        .unwrap_or_else(|| panic!("case {case:?}: expected the store to hold {name}"))
}

/// The description and the file path list of an `EFI_LOAD_OPTION` (UEFI specification, "Load
/// Options"): `u32` attributes, the `u16` length of the file path list, then the
/// NUL-terminated UCS-2 description, then that list.
fn load_option(case: &str, data: &[u8]) -> (String, Vec<u8>) {
    let attributes = u32::from_le_bytes(
        data.get(..4)
            .and_then(|bytes| bytes.try_into().ok())
            .unwrap_or_else(|| panic!("case {case:?}: a load option must carry its attributes")),
    );
    // `LOAD_OPTION_ACTIVE`: the option is one the firmware attempts.
    assert_eq!(
        attributes & 0x0000_0001,
        0x0000_0001,
        "case {case:?}: expected the boot option to be active, got attributes {attributes:#x}"
    );
    let declared = usize::from(u16::from_le_bytes(
        data.get(4..6)
            .and_then(|bytes| bytes.try_into().ok())
            .unwrap_or_else(|| panic!("case {case:?}: a load option must carry its path length")),
    ));
    let description = from_ucs2(data.get(6..).unwrap_or_default());
    let path_at = 6 + (description.chars().count() + 1) * 2;
    let file_path_list = data.get(path_at..).unwrap_or_default().to_vec();
    assert_eq!(
        file_path_list.len(),
        declared,
        "case {case:?}: expected the declared file path list length {declared} to be the bytes \
         after the description, got {}",
        file_path_list.len()
    );
    (description, file_path_list)
}

/// What `DescribeInstances` reports as the boot mode `instance_id` is booting under
/// (`Instance$CurrentInstanceBootMode`).
async fn current_boot_mode(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let (status, body) = api(
        router,
        "DescribeInstances",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances must succeed, got {status} with body {body}"
    );
    element(&body, "currentInstanceBootMode").unwrap_or_else(|| {
        panic!("case {case:?}: DescribeInstances must report currentInstanceBootMode: {body}")
    })
}

/// Mint one snapshot of this account through the operations that create one, and answer its
/// id — the snapshot a register's block device mapping restores the AMI's root device from.
async fn snapshot(router: &MultiServiceRouter, case: &str) -> String {
    let (status, body) = api(
        router,
        "CreateVolume",
        &[p("AvailabilityZone", zone_name(0)), p("Size", "8")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateVolume must succeed, got {status} with body {body}"
    );
    let volume_id = element(&body, "volumeId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVolume must report a volumeId: {body}"));
    let (status, body) = api(router, "CreateSnapshot", &[p("VolumeId", &volume_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSnapshot must succeed, got {status} with body {body}"
    );
    element(&body, "snapshotId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSnapshot must report a snapshotId: {body}"))
}

/// RULE 1: the store an instance holds is the EXACT copy the register of its AMI supplied, and
/// the same bytes that image's `uefiData` attribute reports.
#[tokio::test]
async fn the_store_is_the_copy_the_ami_was_registered_with() {
    // The rows differ in the BYTES the register supplied: a store is opaque to this service,
    // so what varies is its length and alphabet — a short store, a long one, one whose base64
    // carries no padding, and one carrying the whole base64 alphabet.
    let rows: [(&str, &str); 4] = [
        ("a short store", "dGhlIHZhcmlhYmxlIHN0b3Jl"),
        (
            "a longer store",
            "QU1aTlVFRkkAAAAAUEsDBBQAAAAIAAAAIQBkZWFkYmVlZgAAAAAAAAAAAAAAAA==",
        ),
        ("a store whose base64 needs no padding", "YWJjZGVmZ2hpams"),
        (
            "every character of the alphabet",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/",
        ),
    ];

    for (case, uefi_data) in rows {
        let (router, _) = emulator(Vec::new());
        let snapshot_id = snapshot(&router, case).await;
        let root_device = "/dev/xvda";
        let (status, body) = api(
            &router,
            "RegisterImage",
            &[
                p("Name", format!("uefi-store-{}", uefi_data.len())),
                p("RootDeviceName", root_device),
                p("BlockDeviceMapping.1.DeviceName", root_device),
                p("BlockDeviceMapping.1.Ebs.SnapshotId", &snapshot_id),
                p("Architecture", "x86_64"),
                p("BootMode", BOOT_MODE_UEFI),
                p("UefiData", uefi_data),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: RegisterImage must succeed, got {status} with body {body}"
        );
        let image_id = element(&body, "imageId")
            .unwrap_or_else(|| panic!("case {case:?}: RegisterImage must report an imageId: {body}"));

        let instance_id = launch(&router, case, &image_id, "m5.large").await;
        let store = settled_store(&router, case, &instance_id).await;
        assert_eq!(
            store, uefi_data,
            "case {case:?}: expected the exact copy the register supplied, got {store:?}"
        );

        // The image's own attribute reports the same bytes: one store, reported by the
        // instance that holds it and by the AMI it came from.
        let (status, body) = api(
            &router,
            "DescribeImageAttribute",
            &[p("ImageId", &image_id), p("Attribute", "uefiData")],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeImageAttribute must succeed, got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "value").as_deref(),
            Some(uefi_data),
            "case {case:?}: expected the image to report the same store, got {body}"
        );
    }
}

/// RULE 2: an instance whose AMI carries no store of its own comes up with its FIRMWARE's —
/// the non-volatile globally defined UEFI variables, naming the loader of the instance's own
/// architecture.
#[tokio::test]
async fn an_instance_without_a_registered_store_holds_its_firmwares_own() {
    // The rows differ in the AMI's architecture and boot mode and in the instance type, which
    // is what decides whether the machine boots UEFI and which loader its firmware starts.
    let rows: [(&str, &str, &str, &str, &str); 3] = [
        (
            "an x86_64 AMI that requires UEFI",
            "x86_64",
            BOOT_MODE_UEFI,
            "m5.large",
            "\\EFI\\BOOT\\BOOTX64.EFI",
        ),
        (
            "an x86_64 AMI that prefers UEFI on a type that offers it",
            "x86_64",
            BOOT_MODE_UEFI_PREFERRED,
            "c5.xlarge",
            "\\EFI\\BOOT\\BOOTX64.EFI",
        ),
        (
            "an arm64 AMI, whose instance types boot UEFI alone",
            "arm64",
            BOOT_MODE_UEFI,
            "m6g.large",
            "\\EFI\\BOOT\\BOOTAA64.EFI",
        ),
    ];

    for (case, architecture, boot_mode, instance_type, loader) in rows {
        let name = format!("firmware-store-{architecture}-{boot_mode}");
        let (router, image_ids) = emulator(vec![image(&name, architecture, Some(boot_mode))]);
        let instance_id = launch(&router, case, &image_ids[0], instance_type).await;
        let store = settled_store(&router, case, &instance_id).await;
        let variables = store_variables(case, &store);

        assert_eq!(
            variables
                .iter()
                .map(|variable| variable.name.clone())
                .collect::<Vec<String>>(),
            FIRMWARE_VARIABLES
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the firmware's non-volatile variables, got {variables:?}"
        );
        for variable in &variables {
            assert_eq!(
                variable.attributes & NON_VOLATILE,
                NON_VOLATILE,
                "case {case:?}: only non-volatile variables are stored, so {} must carry the \
                 bit, got attributes {:#x}",
                variable.name,
                variable.attributes
            );
            assert_eq!(
                variable.vendor_guid, GLOBAL_VARIABLE_GUID,
                "case {case:?}: expected {} under EFI_GLOBAL_VARIABLE, got {:?}",
                variable.name, variable.vendor_guid
            );
        }

        // "PlatformLang" is a NUL-terminated language code, and the platform reports the one
        // it supports.
        assert_eq!(
            variable(case, &variables, "PlatformLang").data,
            b"en-US\0".to_vec(),
            "case {case:?}: expected the platform's language, got {:?}",
            variable(case, &variables, "PlatformLang").data
        );
        assert_eq!(
            variable(case, &variables, "PlatformLangCodes").data,
            variable(case, &variables, "PlatformLang").data,
            "case {case:?}: expected the supported codes to carry the reported language"
        );
        // "BootOrder" is the list of 16-bit boot option numbers, and every number in it names
        // an option the store holds.
        let boot_order = &variable(case, &variables, "BootOrder").data;
        assert!(
            boot_order.len().is_multiple_of(2),
            "case {case:?}: expected whole 16-bit option numbers, got {boot_order:?}"
        );
        assert!(
            !boot_order.is_empty(),
            "case {case:?}: expected the firmware to have an option to boot"
        );
        for number in boot_order.as_chunks::<2>().0 {
            let number = u16::from_le_bytes(*number);
            let option = format!("Boot{number:04X}");
            assert!(
                variables.iter().any(|variable| variable.name == option),
                "case {case:?}: expected the boot order's {option} to be one of the store's \
                 variables, got {:?}",
                variables
                    .iter()
                    .map(|variable| variable.name.clone())
                    .collect::<Vec<String>>()
            );
        }
        // The boot option names the loader of this machine's own architecture, and is
        // described by the AMI the instance was launched from.
        let (description, file_path_list) =
            load_option(case, &variable(case, &variables, "Boot0000").data);
        assert_eq!(
            description, name,
            "case {case:?}: expected the boot option to name the AMI it loads, got \
             {description:?}"
        );
        let expected = ucs2(loader);
        assert!(
            file_path_list
                .windows(expected.len())
                .any(|window| window == expected),
            "case {case:?}: expected the file path list to name {loader:?}, got \
             {file_path_list:?}"
        );
        // "Timeout" is the seconds the firmware waits, as a 16-bit value.
        assert_eq!(
            variable(case, &variables, "Timeout").data.len(),
            2,
            "case {case:?}: expected a 16-bit timeout, got {:?}",
            variable(case, &variables, "Timeout").data
        );
        // A platform that holds no enrolled platform key is in SETUP MODE, and has entered
        // neither audit nor deployed mode.
        assert_eq!(
            variable(case, &variables, "SetupMode").data,
            vec![1_u8],
            "case {case:?}: expected setup mode for a platform with no enrolled key, got {:?}",
            variable(case, &variables, "SetupMode").data
        );
        for name in ["AuditMode", "DeployedMode"] {
            assert_eq!(
                variable(case, &variables, name).data,
                vec![0_u8],
                "case {case:?}: expected {name} off, got {:?}",
                variable(case, &variables, name).data
            );
        }
    }
}

/// RULE 3: only a UEFI machine has UEFI variables — an instance that boots the legacy BIOS
/// reports no store, and that is the boot mode `DescribeInstances` reports for it.
#[tokio::test]
async fn an_instance_that_boots_the_legacy_bios_holds_no_store() {
    // The rows differ in WHY the machine boots the legacy BIOS: its AMI requires that mode,
    // its AMI prefers UEFI but its instance type offers none, or its AMI declares no boot mode
    // at all.
    let rows: [(&str, Option<&str>, &str); 3] = [
        (
            "an AMI that requires the legacy BIOS",
            Some(BOOT_MODE_LEGACY_BIOS),
            "m5.large",
        ),
        (
            "an AMI that prefers UEFI on an instance type that offers none",
            Some(BOOT_MODE_UEFI_PREFERRED),
            "t2.micro",
        ),
        ("an AMI that declares no boot mode", None, "m5.large"),
    ];

    for (case, boot_mode, instance_type) in rows {
        let name = format!("legacy-bios-{}", boot_mode.unwrap_or("none"));
        let (router, image_ids) = emulator(vec![image(&name, "x86_64", boot_mode)]);
        let instance_id = launch(&router, case, &image_ids[0], instance_type).await;
        // The machine has come up, so a store it had would be there to read.
        until_running(&router, case, &instance_id).await;

        assert_eq!(
            current_boot_mode(&router, case, &instance_id).await,
            BOOT_MODE_LEGACY_BIOS,
            "case {case:?}: expected the instance to boot the legacy BIOS"
        );
        assert_eq!(
            reported_store(&router, case, &instance_id).await,
            None,
            "case {case:?}: a machine with no UEFI firmware holds no UEFI variables"
        );
    }
}

/// RULE 4: a store is what the firmware WROTE, so an instance whose guest has not come up yet
/// holds none, and one whose guest has holds one.
#[tokio::test]
async fn a_store_appears_once_the_firmware_has_come_up() {
    let case = "a launch still settling";
    let (router, image_ids) = emulator(vec![image(
        "firmware-arrives",
        "x86_64",
        Some(BOOT_MODE_UEFI),
    )]);
    let instance_id = launch(&router, case, &image_ids[0], "m5.large").await;

    assert_eq!(
        reported_store(&router, case, &instance_id).await,
        None,
        "case {case:?}: straight after the launch the firmware has not come up, so there is no \
         store to report"
    );

    let store = settled_store(&router, "the launch once it settled", &instance_id).await;
    assert!(
        !store.is_empty(),
        "case {case:?}: expected the store the firmware wrote once the machine came up"
    );
    // It really is the store of a machine that booted: the variables are readable.
    let variables = store_variables("the launch once it settled", &store);
    assert!(
        !variables.is_empty(),
        "case {case:?}: expected the store to hold variables, got none"
    );
}

/// RULE 5: the operation is a READ — two calls about one instance answer exactly the same
/// bytes, and neither moves the instance.
#[tokio::test]
async fn reading_a_store_changes_nothing() {
    let case = "two reads of one instance";
    let (router, image_ids) = emulator(vec![image(
        "read-twice",
        "x86_64",
        Some(BOOT_MODE_UEFI_PREFERRED),
    )]);
    let instance_id = launch(&router, case, &image_ids[0], "m5.large").await;

    let first = settled_store(&router, case, &instance_id).await;
    let before = super::ec2_query::described_instance_state(&router, REGION, &instance_id).await;
    let second = reported_store(&router, case, &instance_id)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: the second read must report the same store"));
    let after = super::ec2_query::described_instance_state(&router, REGION, &instance_id).await;

    assert_eq!(
        first, second,
        "case {case:?}: expected the same bytes from both reads"
    );
    assert_eq!(
        before, after,
        "case {case:?}: reading a store must not move the instance"
    );
    assert_eq!(
        before, "running",
        "case {case:?}: expected the launched instance to be running, got {before:?}"
    );
}

/// RULE 6: `DryRun` asks whether the request WOULD have succeeded, so it is answered after the
/// instance has been resolved: a probe naming an instance the account holds is
/// `DryRunOperation` and reports no store, and a probe naming an id this operation cannot
/// resolve is refused with THAT reason instead. Either way it writes nothing.
#[tokio::test]
async fn a_dry_run_reports_whether_the_request_would_have_succeeded() {
    let (router, image_ids) = emulator(vec![image("dry-run", "x86_64", Some(BOOT_MODE_UEFI))]);
    let case = "an instance the account holds";
    let known = launch(&router, "the probe's setup", &image_ids[0], "m5.large").await;
    let store = settled_store(&router, "the probe's setup", &known).await;

    let (status, body) = api(
        &router,
        "GetInstanceUefiData",
        &[p("InstanceId", &known), p("DryRun", "true")],
    )
    .await;
    assert_eq!(
        status, DRY_RUN_STATUS,
        "case {case:?}: expected HTTP {DRY_RUN_STATUS} from a DryRun, got {status} with body \
         {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "case {case:?}: expected DryRunOperation, got {body}"
    );
    assert_eq!(
        error_message(&body),
        "Request would have succeeded, but DryRun flag is set.",
        "case {case:?}: expected the wording EC2 answers a permitted probe, got {body}"
    );
    assert_eq!(
        member(&body, "uefiData"),
        None,
        "case {case:?}: a probe reports no variable store, got {body}"
    );
    // The probe wrote nothing: the store reads exactly as it did before it.
    assert_eq!(
        reported_store(&router, case, &known).await,
        Some(store),
        "case {case:?}: a probe must leave the instance's store as it was"
    );

    // A probe cannot report that a request would have succeeded when the id it names resolves
    // to nothing, so these rows are answered the same refusal a call without the flag is: the
    // rows differ in which half of that refusal the id falls in.
    let rows: [(&str, &str, &str, &str); 2] = [
        (
            "a well-formed id naming no instance",
            "i-0123456789abcdef0",
            "InvalidInstanceID.NotFound",
            "The instance ID 'i-0123456789abcdef0' does not exist",
        ),
        (
            "a value that is no instance id",
            "not-an-id",
            "InvalidInstanceID.Malformed",
            "The instance ID 'not-an-id' is malformed",
        ),
    ];

    for (case, instance_id, expected_code, expected_message) in rows {
        let (status, body) = api(
            &router,
            "GetInstanceUefiData",
            &[p("InstanceId", instance_id), p("DryRun", "true")],
        )
        .await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS} for a probe of an id that resolves \
             to nothing, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected {expected_code}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_message,
            "case {case:?}: expected {expected_message:?}, got {body}"
        );
    }
}

/// RULE 7: an id outside the form EC2 issues is `InvalidInstanceID.Malformed`, and every id
/// inside that form the account holds no instance under is `InvalidInstanceID.NotFound` —
/// including an id derived from the launched instance's own.
#[tokio::test]
async fn an_unresolvable_instance_id_is_refused() {
    let (router, image_ids) = emulator(vec![image(
        "unresolvable-ids",
        "x86_64",
        Some(BOOT_MODE_UEFI),
    )]);
    // Other resources are created first, so the service's shared id mint has issued the
    // identifiers the derived ids below carry: what another KIND of resource was given says
    // nothing about whether a value reads as an instance id, so those ids name an instance
    // that does not exist rather than being unreadable.
    let (status, body) = api(&router, "CreateVpc", &[p("CidrBlock", "10.0.0.0/16")]).await;
    assert_eq!(
        status, 200,
        "CreateVpc must succeed, got {status} with body {body}"
    );
    let vpc_id = element(&body, "vpcId").expect("CreateVpc must report a vpcId");
    for index in 0..4 {
        let (status, body) = api(
            &router,
            "CreateSecurityGroup",
            &[
                p("GroupName", format!("uefi-ids-{index}")),
                p("GroupDescription", "identifiers of another kind"),
                p("VpcId", &vpc_id),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "CreateSecurityGroup must succeed, got {status} with body {body}"
        );
    }
    let known = launch(&router, "the refusals' setup", &image_ids[0], "m5.large").await;

    // Every way a value can fall outside the form EC2 issues, then the ids inside it that name
    // no instance of the account: the two published forms, and ids derived from the launched
    // instance's own id by moving one digit on.
    let mut rows: Vec<(String, String, &str, String)> = vec![
        (
            "not an id at all".to_string(),
            "not-an-id".to_string(),
            "InvalidInstanceID.Malformed",
            "The instance ID 'not-an-id' is malformed".to_string(),
        ),
        (
            "another resource's prefix".to_string(),
            "vol-0123456789abcdef0".to_string(),
            "InvalidInstanceID.Malformed",
            "The instance ID 'vol-0123456789abcdef0' is malformed".to_string(),
        ),
        (
            "a known id with a suffix".to_string(),
            format!("{known}x"),
            "InvalidInstanceID.Malformed",
            format!("The instance ID '{known}x' is malformed"),
        ),
        (
            "the null id".to_string(),
            "i-00000000000000000".to_string(),
            "InvalidInstanceID.Malformed",
            "The instance ID 'i-00000000000000000' is malformed".to_string(),
        ),
        (
            "a long form whose leading digit is above zero".to_string(),
            "i-1234567890abcdef0".to_string(),
            "InvalidInstanceID.Malformed",
            "The instance ID 'i-1234567890abcdef0' is malformed".to_string(),
        ),
        (
            "a well-formed long id nothing was launched under".to_string(),
            "i-0123456789abcdef0".to_string(),
            "InvalidInstanceID.NotFound",
            "The instance ID 'i-0123456789abcdef0' does not exist".to_string(),
        ),
        (
            "the retired short form, naming no instance".to_string(),
            "i-12345678".to_string(),
            "InvalidInstanceID.NotFound",
            "The instance ID 'i-12345678' does not exist".to_string(),
        ),
    ];
    for digit in ['2', '3', '4', '5'] {
        let mut derived: Vec<char> = known.chars().collect();
        if let Some(last) = derived.last_mut() {
            *last = digit;
        }
        let derived: String = derived.into_iter().collect();
        if derived == known {
            continue;
        }
        rows.push((
            format!("an id derived from the launched instance's own, ending {digit}"),
            derived.clone(),
            "InvalidInstanceID.NotFound",
            format!("The instance ID '{derived}' does not exist"),
        ));
    }

    for (case, instance_id, expected_code, expected_message) in rows {
        let (status, body) = api(
            &router,
            "GetInstanceUefiData",
            &[p("InstanceId", &instance_id)],
        )
        .await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected {expected_code}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_message,
            "case {case:?}: expected {expected_message:?}, got {body}"
        );
    }
}
