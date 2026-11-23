// SPDX-License-Identifier: CC0-1.0

//! The virtual MFA devices an account provisions: what a create records, what it reports
//! once and only once, what a later read of the account's devices reports, and the TAGS
//! those devices carry.
//!
//! The rule under test: `CreateVirtualMFADevice` records a device the ACCOUNT holds,
//! identified by its path and name together — the `mfa` ARN of the two, which is the
//! serial number every later call addresses the device by — and answers with that serial
//! beside the two secrets the device is provisioned with: a base32 seed and a QR code PNG
//! of the `otpauth://` URI carrying that seed. A second create under one path and name is
//! a conflict rather than a second device, and a later read of the account's devices
//! reports what was recorded without the secrets.
//!
//! The tag suites at the end of this file pin the rule the device's own tag operations
//! follow: `TagMFADevice` merges the supplied pairs into the set the device PERSISTS —
//! overwriting a held key's value and appending a key it does not hold — `UntagMFADevice`
//! drops exactly the keys it names, `ListMFADeviceTags` reports the persisted set sorted
//! by tag key and paged over that order, and a serial number the account holds no device
//! under is refused as an entity that does not exist with nothing written.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the path
//! the device is created under, the characters its name is spelled with, how many tags it
//! carries, and how many devices the account holds. The recorded case — one device at the
//! default path — is one row.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns and the other suites
//! in this crate share.

use std::collections::BTreeMap;

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::TAG_LIMIT_EXCEEDED_MESSAGE;

use crate::identity_policy_enforcement::{principal, statement};
use crate::role_lifecycle::{param, reported_tags};
use crate::user_lifecycle::{
    account_id, call, call_signed_by, element, elements, error_code, router,
};

/// The base32 alphabet of RFC 3548 / RFC 4648, which `VirtualMFADevice$Base32StringSeed`
/// is spelled in ("the base32 seed defined as specified in RFC3548").
const BASE32_ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// How many base32 characters a 160-bit seed is spelled in: base32 carries 5 bits per
/// character.
const BASE32_SEED_CHARACTERS: usize = 32;

/// The eight-byte signature every PNG file begins with (PNG specification, "PNG file
/// signature"), which is what says the `QRCodePNG` member carries a PNG image.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// IAM's whole refusal for a create naming a path and name the account already holds a
/// virtual MFA device under, written out here rather than read from the implementation's
/// constant: what these suites pin is the SENTENCE IAM answers, so a reworded constant has
/// to fail here rather than agree with itself.
///
/// Entirely fixed text — the kind of entity the account already holds, with neither the
/// device's name nor its path in it.
const DEVICE_ALREADY_EXISTS: &str = "MFA device already exists.";

/// One device a row provisions: the path it is created under, the name it is spelled with,
/// and the tags it carries.
pub(crate) struct Provisioned {
    /// The case this row stands for, named in every assertion it makes.
    pub(crate) case: &'static str,
    /// The `Path` the request carries, or `None` for a request that names none — which
    /// "defaults to a slash (/)".
    pub(crate) path: Option<&'static str>,
    /// The `VirtualMFADeviceName` the request carries.
    pub(crate) name: &'static str,
    /// The tags the request attaches, as key/value pairs.
    pub(crate) tags: &'static [(&'static str, &'static str)],
}

/// The table every suite below spans: the path, the name's character classes (the
/// `^[\w+=,.@-]+$` pattern `virtualMFADeviceName` declares) and the number of tags all
/// vary, and no single row is the specification.
pub(crate) const PROVISIONED: &[Provisioned] = &[
    Provisioned {
        case: "the default path, a plain name, no tags",
        path: None,
        name: "root-account-mfa-device",
        tags: &[],
    },
    Provisioned {
        case: "the account root named explicitly",
        path: Some("/"),
        name: "BillingOperator",
        tags: &[("team", "billing")],
    },
    Provisioned {
        case: "a nested path",
        path: Some("/engineering/platform/"),
        name: "platform_on-call",
        tags: &[("team", "platform"), ("rotation", "on-call")],
    },
    Provisioned {
        case: "every character class the name pattern admits",
        path: Some("/contractors/"),
        name: "a.b+c=d,e@f-g_0",
        tags: &[],
    },
    Provisioned {
        case: "a long name under a deep path",
        path: Some("/a/b/c/d/"),
        name: "VirtualMFADeviceForTheOperatorWhoCarriesTheLongestNameInThisAccount",
        tags: &[
            ("one", "1"),
            ("two", "2"),
            ("three", "3"),
            ("four", "4"),
            ("five", "5"),
        ],
    },
];

impl Provisioned {
    /// The path this row's device is created under, resolved: a request that names none
    /// creates the device at the account root.
    fn resolved_path(&self) -> &str {
        self.path.unwrap_or("/")
    }

    /// The serial number — the `mfa` ARN of the path and the name — the device created by
    /// this row must carry.
    pub(crate) fn serial_number(&self) -> String {
        format!(
            "arn:aws:iam::{}:mfa{}{}",
            account_id(),
            self.resolved_path(),
            self.name
        )
    }

    /// This row's create as awsQuery form parameters.
    pub(crate) fn params(&self) -> Vec<(String, String)> {
        let mut params = vec![(
            "VirtualMFADeviceName".to_string(),
            self.name.to_string(),
        )];
        if let Some(path) = self.path {
            params.push(("Path".to_string(), path.to_string()));
        }
        for (index, (key, value)) in self.tags.iter().enumerate() {
            let member = index + 1;
            params.push((format!("Tags.member.{member}.Key"), (*key).to_string()));
            params.push((format!("Tags.member.{member}.Value"), (*value).to_string()));
        }
        params
    }
}

/// `params` as the borrowed pairs the harness takes.
pub(crate) fn borrowed(params: &[(String, String)]) -> Vec<(&str, &str)> {
    params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// The bytes of a base64 member of an awsQuery response — the encoding a `blob` member is
/// carried in on the wire.
fn blob(case: &str, encoded: &str) -> Vec<u8> {
    data_encoding::BASE64
        .decode(encoded.as_bytes())
        .unwrap_or_else(|error| panic!("case {case:?}: expected base64, got {encoded:?}: {error}"))
}

/// What a create answers: the serial number, the seed as the caller reads it, and the
/// QR code image's bytes.
pub(crate) async fn provision(
    router: &smithy_server_runtime::MultiServiceRouter,
    row: &Provisioned,
) -> (String, String, Vec<u8>) {
    let params = row.params();
    let (status, body) = call(router, "CreateVirtualMFADevice", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {:?}: expected HTTP 200 from the create, got {status}: {body}",
        row.case
    );
    let serial_number = element(&body, "SerialNumber");
    let seed = String::from_utf8(blob(row.case, &element(&body, "Base32StringSeed")))
        .unwrap_or_else(|error| panic!("case {:?}: expected a text seed: {error}", row.case));
    let qr_code = blob(row.case, &element(&body, "QRCodePNG"));
    (serial_number, seed, qr_code)
}

/// A create records the device under the serial number its path and name make, and answers
/// with that serial, a base32 seed and a QR code PNG of the URI carrying that seed.
#[tokio::test]
async fn a_created_device_reports_its_serial_number_seed_and_qr_code() {
    let router = router();
    let mut seeds: Vec<String> = Vec::new();
    for row in PROVISIONED {
        let (serial_number, seed, qr_code) = provision(&router, row).await;

        assert_eq!(
            serial_number,
            row.serial_number(),
            "case {:?}: expected the serial number to be the mfa ARN of the path and name, \
             got {serial_number:?}",
            row.case
        );

        assert_eq!(
            seed.chars().count(),
            BASE32_SEED_CHARACTERS,
            "case {:?}: expected a {BASE32_SEED_CHARACTERS}-character base32 seed, got {seed:?}",
            row.case
        );
        assert!(
            seed.chars().all(|spelled| BASE32_ALPHABET.contains(spelled)),
            "case {:?}: expected every seed character to be base32, got {seed:?}",
            row.case
        );
        assert!(
            !seeds.contains(&seed),
            "case {:?}: expected a seed no other device was provisioned with, got {seed:?} \
             again among {seeds:?}",
            row.case
        );
        seeds.push(seed);

        assert_eq!(
            qr_code.get(..PNG_SIGNATURE.len()),
            Some(&PNG_SIGNATURE[..]),
            "case {:?}: expected the QR code to be a PNG image, got {:?}",
            row.case,
            qr_code.get(..PNG_SIGNATURE.len())
        );
        // A real image, not bytes that merely start like one: the decoder reads the
        // header the encoder wrote, and a QR symbol is square.
        let decoded = png::Decoder::new(std::io::Cursor::new(&qr_code))
            .read_info()
            .unwrap_or_else(|error| {
                panic!("case {:?}: expected a decodable PNG: {error}", row.case)
            });
        let info = decoded.info();
        assert_eq!(
            info.width, info.height,
            "case {:?}: expected a square QR code image, got {}x{}",
            row.case, info.width, info.height
        );
        assert!(
            info.width > 0,
            "case {:?}: expected a QR code image with pixels, got {}x{}",
            row.case,
            info.width,
            info.height
        );
    }
}

/// The devices a create recorded are what a later read of the account's devices reports:
/// every serial, in the order the account holds them, carrying neither secret.
#[tokio::test]
async fn created_devices_are_read_back_without_their_secrets() {
    let router = router();
    let mut expected: Vec<String> = Vec::new();
    for row in PROVISIONED {
        provision(&router, row).await;
        expected.push(row.serial_number());
    }
    expected.sort();

    let (status, body) = call(&router, "ListVirtualMFADevices", &[]).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from the read, got {status}: {body}"
    );
    assert_eq!(
        elements(&body, "SerialNumber"),
        expected,
        "expected every created device to be reported in serial-number order: {body}"
    );
    assert!(
        elements(&body, "Base32StringSeed").is_empty(),
        "expected a read of the account's devices to report no seed: {body}"
    );
    assert!(
        elements(&body, "QRCodePNG").is_empty(),
        "expected a read of the account's devices to report no QR code: {body}"
    );
    assert!(
        elements(&body, "Key").is_empty(),
        "expected a listing to report no tags — \"this operation does not return tags, even \
         though they are an attribute of the returned object\": {body}"
    );
    assert_eq!(
        element(&body, "IsTruncated"),
        "false",
        "expected a single page to be the whole answer: {body}"
    );
}

/// A device nothing has enabled is UNASSIGNED, so the status filter selects it and only
/// it: `Unassigned` and the default `Any` report every created device, `Assigned` reports
/// none.
#[tokio::test]
async fn the_assignment_status_filter_selects_unheld_devices() {
    let router = router();
    let mut expected: Vec<String> = Vec::new();
    for row in PROVISIONED {
        provision(&router, row).await;
        expected.push(row.serial_number());
    }
    expected.sort();

    for (case, status_filter, selected) in [
        ("no filter at all", None, expected.clone()),
        ("Any", Some("Any"), expected.clone()),
        ("Unassigned", Some("Unassigned"), expected.clone()),
        ("Assigned", Some("Assigned"), Vec::new()),
    ] {
        let params: Vec<(&str, &str)> = status_filter
            .map(|value| vec![("AssignmentStatus", value)])
            .unwrap_or_default();
        let (status, body) = call(&router, "ListVirtualMFADevices", &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the read, got {status}: {body}"
        );
        assert_eq!(
            elements(&body, "SerialNumber"),
            selected,
            "case {case:?}: expected exactly the devices that filter selects: {body}"
        );
    }
}

/// A read pages over the devices: each page carries at most `MaxItems` of them and hands
/// back the marker the next page resumes strictly after, so following the markers reports
/// every device exactly once.
#[tokio::test]
async fn a_read_pages_over_the_accounts_devices() {
    let router = router();
    let mut expected: Vec<String> = Vec::new();
    for row in PROVISIONED {
        provision(&router, row).await;
        expected.push(row.serial_number());
    }
    expected.sort();

    for page_size in [1usize, 2, 5] {
        let mut reported: Vec<String> = Vec::new();
        let mut marker: Option<String> = None;
        loop {
            let size = page_size.to_string();
            let mut params = vec![("MaxItems", size.as_str())];
            if let Some(resume) = marker.as_deref() {
                params.push(("Marker", resume));
            }
            let (status, body) = call(&router, "ListVirtualMFADevices", &params).await;
            assert_eq!(
                status, 200,
                "case {page_size:?} per page: expected HTTP 200, got {status}: {body}"
            );
            let page = elements(&body, "SerialNumber");
            assert!(
                page.len() <= page_size,
                "case {page_size:?} per page: expected at most {page_size} devices, got {}: \
                 {body}",
                page.len()
            );
            reported.extend(page);
            if element(&body, "IsTruncated") == "true" {
                marker = Some(element(&body, "Marker"));
            } else {
                assert!(
                    elements(&body, "Marker").is_empty(),
                    "case {page_size:?} per page: expected no marker on the last page: {body}"
                );
                break;
            }
        }
        assert_eq!(
            reported, expected,
            "case {page_size:?} per page: expected the pages to report every device once"
        );
    }
}

/// The path and the name TOGETHER identify a device, so a second create under one pair is
/// the conflict `EntityAlreadyExists` names, the account keeps the device it already held,
/// and the same name under ANOTHER path is a different device.
#[tokio::test]
async fn a_second_create_at_one_path_and_name_is_a_conflict() {
    for row in PROVISIONED {
        let router = router();
        let (first, _, _) = provision(&router, row).await;

        let params = row.params();
        let (status, body) = call(&router, "CreateVirtualMFADevice", &borrowed(&params)).await;
        assert_eq!(
            status, 409,
            "case {:?}: expected HTTP 409 from the second create, got {status}: {body}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            "EntityAlreadyExists",
            "case {:?}: expected the conflict error code: {body}",
            row.case
        );
        assert_eq!(
            element(&body, "Message"),
            DEVICE_ALREADY_EXISTS,
            "case {:?}: expected {DEVICE_ALREADY_EXISTS:?}, got {:?}: {body}",
            row.case,
            element(&body, "Message")
        );

        // The same NAME under another path is another device, so the account then holds
        // both — which is what "use with path to uniquely identify a virtual MFA device"
        // means.
        let elsewhere = Provisioned {
            case: row.case,
            path: Some("/somewhere-else/"),
            name: row.name,
            tags: &[],
        };
        let (second, _, _) = provision(&router, &elsewhere).await;
        assert_ne!(
            first, second,
            "case {:?}: expected one name under two paths to be two devices",
            row.case
        );

        let (status, body) = call(&router, "ListVirtualMFADevices", &[]).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from the read, got {status}: {body}",
            row.case
        );
        let mut held = vec![first, second];
        held.sort();
        assert_eq!(
            elements(&body, "SerialNumber"),
            held,
            "case {:?}: expected the account to hold one device per path and name: {body}",
            row.case
        );
    }
}

/// The tags a create attaches are the device's, and a reserved key is refused with
/// nothing created: "if any one of the tags is invalid or if you exceed the allowed
/// maximum number of tags, then the entire request fails and the resource is not created".
#[tokio::test]
async fn attached_tags_are_the_devices_and_a_reserved_key_creates_nothing() {
    for row in PROVISIONED {
        let router = router();
        let params = row.params();
        let (status, body) = call(&router, "CreateVirtualMFADevice", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from the create, got {status}: {body}",
            row.case
        );
        let keys: Vec<String> = row
            .tags
            .iter()
            .map(|(key, _)| (*key).to_string())
            .collect();
        assert_eq!(
            elements(&body, "Key"),
            keys,
            "case {:?}: expected the create to report the tags it attached: {body}",
            row.case
        );

        // `aws:` is reserved for AWS, so a tag naming it is an invalid input — and the
        // device is not created.
        let reserved = Provisioned {
            case: row.case,
            path: row.path,
            name: "reserved-tag-device",
            tags: &[("aws:managed", "no")],
        };
        let params = reserved.params();
        let (status, body) = call(&router, "CreateVirtualMFADevice", &borrowed(&params)).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400 for a reserved tag key, got {status}: {body}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            "InvalidInput",
            "case {:?}: expected the invalid-input error code: {body}",
            row.case
        );

        let (status, body) = call(&router, "ListVirtualMFADevices", &[]).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from the read, got {status}: {body}",
            row.case
        );
        assert_eq!(
            elements(&body, "SerialNumber"),
            vec![row.serial_number()],
            "case {:?}: expected the refused create to have stored nothing: {body}",
            row.case
        );
    }
}

// ---------------------------------------------------------------------------
// The TAGS a device carries: `TagMFADevice`, `UntagMFADevice`,
// `ListMFADeviceTags`.
// ---------------------------------------------------------------------------

/// The published maximum number of tags one IAM resource may carry: `tagListType` is
/// len 0..50 (`smithy_query(query="operation", operation="TagMFADevice")`).
const MAX_TAGS_PER_RESOURCE: usize = 50;

/// How IAM words a serial number the account holds no MFA device under — the sentence
/// every operation addressing a device by serial refuses with.
fn not_found_message(serial_number: &str) -> String {
    format!("MFA Device with serial number {serial_number} does not exist.")
}

/// The `Tags.member.N.Key` / `Tags.member.N.Value` parameters of an awsQuery tag list.
fn tag_members(tags: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut params: Vec<(String, String)> = Vec::new();
    for (index, (key, value)) in tags.iter().enumerate() {
        let position = index + 1;
        params.push(param(&format!("Tags.member.{position}.Key"), key));
        params.push(param(&format!("Tags.member.{position}.Value"), value));
    }
    params
}

/// One `TagMFADevice` call for the device `serial_number`.
async fn tag(
    router: &MultiServiceRouter,
    serial_number: &str,
    tags: &[(&str, &str)],
) -> (u16, String) {
    let mut params = vec![param("SerialNumber", serial_number)];
    params.extend(tag_members(tags));
    call(router, "TagMFADevice", &borrowed(&params)).await
}

/// One `UntagMFADevice` call for the device `serial_number`.
async fn untag(router: &MultiServiceRouter, serial_number: &str, keys: &[&str]) -> (u16, String) {
    let mut params = vec![param("SerialNumber", serial_number)];
    for (index, key) in keys.iter().enumerate() {
        params.push(param(&format!("TagKeys.member.{}", index + 1), key));
    }
    call(router, "UntagMFADevice", &borrowed(&params)).await
}

/// One `ListMFADeviceTags` call, with an optional page size and marker.
async fn list_tags(
    router: &MultiServiceRouter,
    serial_number: &str,
    max_items: Option<i32>,
    marker: Option<&str>,
) -> (u16, String) {
    let mut params = vec![param("SerialNumber", serial_number)];
    if let Some(size) = max_items {
        params.push(param("MaxItems", &size.to_string()));
    }
    if let Some(token) = marker {
        params.push(param("Marker", token));
    }
    call(router, "ListMFADeviceTags", &borrowed(&params)).await
}

/// The tag set the device PERSISTS, read back through the listing, together with the key
/// order the listing reported it in.
async fn persisted_tags(
    router: &MultiServiceRouter,
    case: &str,
    serial_number: &str,
) -> (BTreeMap<String, String>, Vec<String>) {
    let (status, body) = list_tags(router, serial_number, None, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the tag listing, got {status}: {body}"
    );
    assert_eq!(
        element(&body, "IsTruncated"),
        "false",
        "case {case:?}: a whole tag set fits one default page of 100: {body}"
    );
    (reported_tags(&body), elements(&body, "Key"))
}

/// `expected` as the map the listing must report.
fn expected_map(expected: &[(&str, &str)]) -> BTreeMap<String, String> {
    expected
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// One call of a tag lifecycle.
enum TagCall {
    /// `TagMFADevice` carrying these pairs.
    Tag(&'static [(&'static str, &'static str)]),
    /// `UntagMFADevice` naming these keys.
    Untag(&'static [&'static str]),
}

/// One step: a call, and the WHOLE tag set the device must hold once it has been served.
struct Step {
    /// The case this step stands for, named in every assertion it makes.
    case: &'static str,
    /// The call the step makes.
    call: TagCall,
    /// Every tag the device holds afterwards, stated in full rather than as a delta.
    expected: &'static [(&'static str, &'static str)],
}

/// The tag set the alphabet-spanning step leaves behind, which the three steps after it
/// are stated against. The keys and values span what `tagKeyType` and `tagValueType`
/// publish — `^[\p{L}\p{Z}\p{N}_.:/=+\-@]+$` and the same with `*` (`smithy_query(query =
/// "shape", shape="Tag")`) — including a letter outside ASCII, a space, a digit-only key
/// and the empty value `tagValueType`'s 0..256 length admits.
const SPANNING: &[(&str, &str)] = &[
    ("7", "seven"),
    ("Cost Center", "Human Resources"),
    ("Department", "Engineering"),
    ("empty-value", ""),
    ("key=value", "="),
    ("owner", "sre"),
    ("project/name", "emulator"),
    ("zone", "c"),
    ("Ünicode", "Grüße"),
];

/// The same set with `owner` dropped: what step 6 leaves.
const SPANNING_WITHOUT_OWNER: &[(&str, &str)] = &[
    ("7", "seven"),
    ("Cost Center", "Human Resources"),
    ("Department", "Engineering"),
    ("empty-value", ""),
    ("key=value", "="),
    ("project/name", "emulator"),
    ("zone", "c"),
    ("Ünicode", "Grüße"),
];

/// The keys of [`SPANNING_WITHOUT_OWNER`], which the step that empties the set names.
const SPANNING_KEYS: &[&str] = &[
    "7",
    "Cost Center",
    "Department",
    "empty-value",
    "key=value",
    "project/name",
    "zone",
    "Ünicode",
];

/// The lifecycle every device shape below is walked through. The rows differ along what
/// the rule generalises over: how many pairs one call carries, whether a key is new or
/// held, the order the pairs arrive in, the alphabet the keys and values are spelled with,
/// whether an untagged key is held at all, and whether the set is empty when the call
/// arrives.
const LIFECYCLE: &[Step] = &[
    Step {
        case: "the first tag of a device created without any",
        call: TagCall::Tag(&[("Department", "Engineering")]),
        expected: &[("Department", "Engineering")],
    },
    Step {
        case: "two more keys, arriving out of key order",
        call: TagCall::Tag(&[("zone", "c"), ("owner", "ops")]),
        expected: &[
            ("Department", "Engineering"),
            ("owner", "ops"),
            ("zone", "c"),
        ],
    },
    Step {
        case: "a held key is overwritten with the new value",
        call: TagCall::Tag(&[("owner", "sre")]),
        expected: &[
            ("Department", "Engineering"),
            ("owner", "sre"),
            ("zone", "c"),
        ],
    },
    Step {
        case: "the same write again, which changes nothing",
        call: TagCall::Tag(&[("owner", "sre")]),
        expected: &[
            ("Department", "Engineering"),
            ("owner", "sre"),
            ("zone", "c"),
        ],
    },
    Step {
        case: "every character class a key and a value admit",
        call: TagCall::Tag(&[
            ("Cost Center", "Human Resources"),
            ("Ünicode", "Grüße"),
            ("empty-value", ""),
            ("project/name", "emulator"),
            ("key=value", "="),
            ("7", "seven"),
        ]),
        expected: SPANNING,
    },
    Step {
        case: "a key the device holds is dropped",
        call: TagCall::Untag(&["owner"]),
        expected: SPANNING_WITHOUT_OWNER,
    },
    Step {
        case: "a key the device does not hold drops nothing",
        call: TagCall::Untag(&["owner", "never-held"]),
        expected: SPANNING_WITHOUT_OWNER,
    },
    Step {
        case: "every remaining key at once",
        call: TagCall::Untag(SPANNING_KEYS),
        expected: &[],
    },
    Step {
        case: "a tag written after the set was emptied",
        call: TagCall::Tag(&[("after", "empty")]),
        expected: &[("after", "empty")],
    },
];

/// RULE: a tag write MERGES the supplied pairs into the set the device persists — a held
/// key is overwritten and a new one appended — an untag drops exactly the keys it names,
/// and the listing reports the resulting set sorted by tag key.
#[tokio::test]
async fn tag_and_untag_walk_the_tag_set_the_device_persists() {
    // The device shapes span the serial numbers the rule is addressed by: the default
    // path, a nested one, and a name spelled with every class its pattern admits. Each is
    // created WITHOUT tags, so the step table states the whole set from the first call on.
    for shape in PROVISIONED {
        let router = router();
        let device = Provisioned {
            case: shape.case,
            path: shape.path,
            name: shape.name,
            tags: &[],
        };
        let (serial_number, _, _) = provision(&router, &device).await;

        for step in LIFECYCLE {
            let case = format!("{} / {}", shape.case, step.case);
            let (status, body) = match step.call {
                TagCall::Tag(tags) => tag(&router, &serial_number, tags).await,
                TagCall::Untag(keys) => untag(&router, &serial_number, keys).await,
            };
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from the tag write, got {status}: {body}"
            );

            let (held, order) = persisted_tags(&router, &case, &serial_number).await;
            assert_eq!(
                held,
                expected_map(step.expected),
                "case {case:?}: expected the device to hold exactly that set, got {held:?}"
            );
            let mut sorted: Vec<String> =
                step.expected.iter().map(|(key, _)| (*key).to_string()).collect();
            sorted.sort();
            assert_eq!(
                order, sorted,
                "case {case:?}: expected the listing to report the keys sorted by key, got \
                 {order:?}"
            );
        }
    }
}

/// RULE: the set a write merges into is the one the CREATE attached, so tagging a device
/// provisioned with tags keeps every key the request does not name and overwrites the one
/// it does.
#[tokio::test]
async fn a_write_merges_onto_the_tags_the_create_attached() {
    for row in PROVISIONED {
        let router = router();
        let (serial_number, _, _) = provision(&router, row).await;
        let case = format!("{} / the create's own tags", row.case);

        let (held, _) = persisted_tags(&router, &case, &serial_number).await;
        let mut expected: BTreeMap<String, String> = row
            .tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        assert_eq!(
            held, expected,
            "case {case:?}: expected the listing to report the tags the create attached, \
             got {held:?}"
        );

        // One new key, plus — for a device the create tagged — the FIRST held key written
        // again with another value, so the row exercises both halves of the merge.
        let mut write: Vec<(&str, &str)> = vec![("owner", "ops")];
        if let Some((key, _)) = row.tags.first() {
            write.push((key, "rewritten"));
            expected.insert((*key).to_string(), "rewritten".to_string());
        }
        expected.insert("owner".to_string(), "ops".to_string());

        let (status, body) = tag(&router, &serial_number, &write).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the tag write, got {status}: {body}"
        );
        let (held, _) = persisted_tags(&router, &case, &serial_number).await;
        assert_eq!(
            held, expected,
            "case {case:?}: expected the merged set, got {held:?}"
        );
    }
}

/// A device holding `count` tags whose keys arrive in an order that is NOT the sorted one
/// (the stride walks the key space), so a listing that reported arrival order would differ
/// from one that sorted.
fn scrambled_tags(count: usize) -> Vec<(String, String)> {
    (0..count)
        .map(|index| {
            let key = (index * 17) % count.max(1);
            (format!("key-{key:02}"), format!("value-{key}"))
        })
        .collect()
}

/// RULE: the listing pages over the SAME key order, so whatever the page size every tag is
/// reported exactly once in ascending key order, and only a truncated page carries the
/// `Marker` that resumes after it.
#[tokio::test]
async fn the_listing_pages_over_the_tags_in_key_order() {
    let router = router();
    let device = Provisioned {
        case: "a device carrying the published maximum number of tags",
        path: Some("/paged/"),
        name: "paged-device",
        tags: &[],
    };
    let (serial_number, _, _) = provision(&router, &device).await;
    let owned = scrambled_tags(MAX_TAGS_PER_RESOURCE);
    let written: Vec<(&str, &str)> = owned
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let (status, body) = tag(&router, &serial_number, &written).await;
    assert_eq!(
        status, 200,
        "the fixture tag write must be served, got {status}: {body}"
    );
    let mut expected: Vec<String> = owned.iter().map(|(key, _)| key.clone()).collect();
    expected.sort();

    // Rows span the page size: the published minimum, sizes that do and do not divide the
    // set, the size of the set, the published maximum, and the absent-parameter default.
    // `maxItemsType` is range 1..1000 (`smithy_query(query="operation",
    // operation="ListMFADeviceTags")`).
    for page_size in [Some(1), Some(2), Some(7), Some(50), Some(1000), None] {
        let case = format!("MaxItems={page_size:?}");
        let mut seen: Vec<String> = Vec::new();
        let mut marker: Option<String> = None;
        let mut pages = 0;
        loop {
            let (status, body) =
                list_tags(&router, &serial_number, page_size, marker.as_deref()).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status}: {body}"
            );
            let page = elements(&body, "Key");
            if let Some(size) = page_size {
                assert!(
                    page.len() <= size as usize,
                    "case {case:?}: expected at most {size} tags on a page, got {}",
                    page.len()
                );
            }
            seen.extend(page);
            if element(&body, "IsTruncated") == "true" {
                let token = elements(&body, "Marker");
                assert_eq!(
                    token.len(),
                    1,
                    "case {case:?}: a truncated page must carry a Marker, got {token:?}"
                );
                marker = Some(token.into_iter().next().expect("a marker"));
            } else {
                assert!(
                    elements(&body, "Marker").is_empty(),
                    "case {case:?}: a final page must carry no Marker, got {body}"
                );
                break;
            }
            pages += 1;
            assert!(
                pages <= expected.len() + 1,
                "case {case:?}: pagination did not terminate after {pages} pages"
            );
        }
        assert_eq!(
            seen, expected,
            "case {case:?}: expected every tag exactly once in key order, got {seen:?}"
        );
    }

    // A `Marker` names a position in ONE device's tag order, so a token this service did
    // not issue for that device's tags is refused rather than read as "start from the
    // beginning".
    let other = Provisioned {
        case: "another device whose tags are paged",
        path: Some("/paged/"),
        name: "other-paged-device",
        tags: &[],
    };
    let (other_serial, _, _) = provision(&router, &other).await;
    let (status, body) = tag(&router, &other_serial, &[("a", "1"), ("b", "2")]).await;
    assert_eq!(
        status, 200,
        "the second fixture tag write must be served, got {status}: {body}"
    );
    let (_, first_page) = list_tags(&router, &other_serial, Some(1), None).await;
    let foreign = element(&first_page, "Marker");
    for token in ["not-a-marker", "zz", foreign.as_str()] {
        let (status, body) = list_tags(&router, &serial_number, Some(1), Some(token)).await;
        assert_eq!(
            status, 400,
            "case {token:?}: a marker this service did not issue for these tags must be \
             refused, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidInput",
            "case {token:?}: expected InvalidInput, got {body}"
        );
    }

    // One past each bound of `maxItemsType` is refused before the operation runs, and each
    // bound itself is served.
    for out_of_range in [0, 1001, -1] {
        let (status, body) = list_tags(&router, &serial_number, Some(out_of_range), None).await;
        assert_eq!(
            status, 400,
            "case {out_of_range:?}: expected HTTP 400, got {status}: {body}"
        );
    }
    for in_range in [1, 1000] {
        let (status, body) = list_tags(&router, &serial_number, Some(in_range), None).await;
        assert_eq!(
            status, 200,
            "case {in_range:?}: expected HTTP 200, got {status}: {body}"
        );
    }
}

/// RULE: the tag listing is a READ — two listings report the same set, a paged walk leaves
/// it alone, and the device the account's own read reports is unchanged by having had its
/// tags listed.
#[tokio::test]
async fn listing_the_tags_changes_nothing_a_later_caller_sees() {
    for row in PROVISIONED {
        let router = router();
        let (serial_number, _, _) = provision(&router, row).await;
        let case = format!("{} / the listing is a read", row.case);
        let (status, body) = tag(&router, &serial_number, &[("owner", "ops")]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture tag write must be served, got {status}: {body}"
        );

        let (first, first_order) = persisted_tags(&router, &case, &serial_number).await;
        // A paged walk, which issues markers, must leave the set alone too.
        let (status, paged) = list_tags(&router, &serial_number, Some(1), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the paged read, got {status}: {paged}"
        );
        let (second, second_order) = persisted_tags(&router, &case, &serial_number).await;
        assert_eq!(
            first, second,
            "case {case:?}: two listings must report the same tags, got {first:?} then \
             {second:?}"
        );
        assert_eq!(
            first_order, second_order,
            "case {case:?}: two listings must report the same key order"
        );

        let (status, read) = call(&router, "ListVirtualMFADevices", &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the account's devices must still read back, got {status}: {read}"
        );
        assert_eq!(
            elements(&read, "SerialNumber"),
            vec![serial_number.clone()],
            "case {case:?}: listing the tags must leave the account's devices alone: {read}"
        );
    }
}

/// RULE: a serial number the account holds NO virtual MFA device under is refused as an
/// entity that does not exist (HTTP 404 `NoSuchEntity`) by every one of the three
/// operations, and the refused write stores nothing.
#[tokio::test]
async fn a_serial_no_device_is_held_under_is_not_found() {
    let router = router();
    let held = &PROVISIONED[1];
    let (held_serial, _, _) = provision(&router, held).await;
    let (status, body) = tag(&router, &held_serial, &[("owner", "ops")]).await;
    assert_eq!(
        status, 200,
        "the fixture tag write must be served, got {status}: {body}"
    );

    // A device the account DID hold, deleted again, so the serial is well formed and names
    // nothing.
    let deleted = Provisioned {
        case: "a device the account no longer holds",
        path: Some("/gone/"),
        name: "deleted-device",
        tags: &[],
    };
    let (deleted_serial, _, _) = provision(&router, &deleted).await;
    let (status, body) = call(
        &router,
        "DeleteVirtualMFADevice",
        &[("SerialNumber", deleted_serial.as_str())],
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture delete must be served, got {status}: {body}"
    );

    // Rows span the forms a serial number takes that name no device the account holds: an
    // `mfa` ARN never created, one created and deleted, one under another path, the ARN of
    // another IAM resource type, and a hardware device's own serial — `serialNumberType`
    // is len 9..256, pattern `^[\w+=/:,.@-]+$`, so each of these is a value the modelled
    // constraints admit and the HANDLER answers for.
    let never_created = format!("arn:aws:iam::{}:mfa/never-created", account_id());
    let other_path = format!("arn:aws:iam::{}:mfa/elsewhere/{}", account_id(), held.name);
    let user_arn = format!("arn:aws:iam::{}:user/not-a-device", account_id());
    let unheld: [&str; 5] = [
        never_created.as_str(),
        deleted_serial.as_str(),
        other_path.as_str(),
        user_arn.as_str(),
        "GAHT12345678",
    ];

    for serial_number in unheld {
        for (operation, answered) in [
            ("TagMFADevice", tag(&router, serial_number, &[("owner", "ops")]).await),
            ("UntagMFADevice", untag(&router, serial_number, &["owner"]).await),
            (
                "ListMFADeviceTags",
                list_tags(&router, serial_number, None, None).await,
            ),
        ] {
            let case = format!("{operation} on {serial_number:?}");
            let (status, body) = answered;
            assert_eq!(
                status, 404,
                "case {case:?}: expected HTTP 404, got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                "NoSuchEntity",
                "case {case:?}: expected NoSuchEntity, got {body}"
            );
            assert_eq!(
                element(&body, "Message"),
                not_found_message(serial_number),
                "case {case:?}: expected IAM's wording for a serial that names no device: \
                 {body}"
            );
        }

        // The refused write created nothing: the serial still names no device, and the
        // device the account DOES hold keeps the tags it had.
        let (status, body) = list_tags(&router, serial_number, None, None).await;
        assert_eq!(
            status, 404,
            "case {serial_number:?}: a refused tag write must store nothing, got {status}: \
             {body}"
        );
        let (tags, _) = persisted_tags(&router, serial_number, &held_serial).await;
        assert_eq!(
            tags,
            expected_map(&[("owner", "ops"), ("team", "billing")]),
            "case {serial_number:?}: the held device's tags must be untouched, got {tags:?}"
        );
    }
}

/// RULE: an invalid tag and a merged set past the published bound each fail the WHOLE
/// request — "if any one of the tags is invalid or if you exceed the allowed maximum number
/// of tags, then the entire request fails" — and leave the device's tags as they were,
/// while re-tagging a key the device already holds is served however full the set is.
#[tokio::test]
async fn an_invalid_tag_or_a_set_past_the_bound_stores_nothing() {
    let router = router();
    let device = &PROVISIONED[2];
    let (serial_number, _, _) = provision(&router, device).await;
    let before = expected_map(device.tags);

    // `aws:` is reserved for AWS, so a tag naming it is an invalid input — whatever else
    // the request carries, and whether the prefix is spelled in upper or lower case.
    for reserved in ["aws:managed", "AWS:Managed"] {
        let (status, body) = tag(
            &router,
            &serial_number,
            &[("fine", "yes"), (reserved, "no")],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {reserved:?}: expected HTTP 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidInput",
            "case {reserved:?}: expected InvalidInput, got {body}"
        );
        let (held, _) = persisted_tags(&router, reserved, &serial_number).await;
        assert_eq!(
            held, before,
            "case {reserved:?}: a refused write must leave the tags as they were, got \
             {held:?}"
        );
    }

    // Fill the set to the published maximum, then one more NEW key is past the bound.
    let owned = scrambled_tags(MAX_TAGS_PER_RESOURCE - device.tags.len());
    let filling: Vec<(&str, &str)> = owned
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let (status, body) = tag(&router, &serial_number, &filling).await;
    assert_eq!(
        status, 200,
        "the fixture fill must be served, got {status}: {body}"
    );
    let (full, _) = persisted_tags(&router, "the full set", &serial_number).await;
    assert_eq!(
        full.len(),
        MAX_TAGS_PER_RESOURCE,
        "the fixture must leave the device holding the maximum, got {full:?}"
    );

    let (status, body) = tag(&router, &serial_number, &[("one-too-many", "yes")]).await;
    assert_eq!(
        status, 409,
        "expected HTTP 409 past the tag bound, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "LimitExceeded",
        "expected LimitExceeded past the tag bound, got {body}"
    );
    assert_eq!(
        element(&body, "Message"),
        TAG_LIMIT_EXCEEDED_MESSAGE,
        "expected {TAG_LIMIT_EXCEEDED_MESSAGE:?}, got {body}"
    );
    let (after, _) = persisted_tags(&router, "past the bound", &serial_number).await;
    assert_eq!(
        after, full,
        "a refused write must leave the tags as they were, got {after:?}"
    );

    // The bound is taken against the MERGED set, so overwriting a held key is served even
    // at the maximum.
    let held_key = full
        .keys()
        .next()
        .expect("the full set holds keys")
        .clone();
    let (status, body) = tag(&router, &serial_number, &[(held_key.as_str(), "rewritten")]).await;
    assert_eq!(
        status, 200,
        "case {held_key:?}: re-tagging a held key at the bound must be served, got \
         {status}: {body}"
    );
    let (rewritten, _) = persisted_tags(&router, "the rewritten key", &serial_number).await;
    assert_eq!(
        rewritten.get(&held_key).map(String::as_str),
        Some("rewritten"),
        "case {held_key:?}: expected the held key to carry the new value, got {rewritten:?}"
    );
    assert_eq!(
        rewritten.len(),
        MAX_TAGS_PER_RESOURCE,
        "case {held_key:?}: an overwrite adds no tag, got {rewritten:?}"
    );
}

/// RULE: each of the three operations is decided against the caller's identity-based
/// policies on its own action — a call nothing allows is refused HTTP 403 `AccessDenied`
/// and writes nothing, while the same call under a statement naming its action on the
/// device's ARN is served.
#[tokio::test]
async fn the_devices_tag_operations_are_decided_by_the_callers_policies() {
    // Rows: the write, the removal and the read, each with the action a statement has to
    // name to govern it.
    let rows: [(&str, &str); 3] = [
        ("TagMFADevice", "iam:TagMFADevice"),
        ("UntagMFADevice", "iam:UntagMFADevice"),
        ("ListMFADeviceTags", "iam:ListMFADeviceTags"),
    ];

    for (operation, iam_action) in rows {
        for granted in [false, true] {
            let case = format!("{operation} with the grant present? {granted}");
            let router = router();
            let device = Provisioned {
                case: "a device whose tags are governed",
                path: None,
                name: "governed-device",
                tags: &[],
            };
            let (serial_number, _, _) = provision(&router, &device).await;
            let (status, body) = tag(&router, &serial_number, &[("owner", "ops")]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the fixture tag write must be served, got {status}: {body}"
            );

            // The caller holds ONE document: the row's own action when the row grants it,
            // and an action no row calls otherwise — so a refusal is the absence of a
            // grant for THIS action rather than the absence of any document.
            let grant = if granted {
                statement("Allow", iam_action, &serial_number)
            } else {
                statement("Allow", "iam:ListUsers", "*")
            };
            let access_key_id = principal(
                &router,
                &case,
                "tag-caller",
                &[("grant".to_string(), grant)],
            )
            .await;

            let owned = match operation {
                "TagMFADevice" => {
                    let mut params = vec![param("SerialNumber", &serial_number)];
                    params.extend(tag_members(&[("zone", "c")]));
                    params
                }
                "UntagMFADevice" => vec![
                    param("SerialNumber", &serial_number),
                    param("TagKeys.member.1", "owner"),
                ],
                _ => vec![param("SerialNumber", &serial_number)],
            };
            let (status, body) =
                call_signed_by(&router, Some(&access_key_id), operation, &borrowed(&owned)).await;

            if granted {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the granted call to be served, got {status}: \
                     {body}"
                );
                continue;
            }

            assert_eq!(
                status, 403,
                "case {case:?}: expected the ungranted call to be refused, got {status}: \
                 {body}"
            );
            assert_eq!(
                error_code(&body),
                "AccessDenied",
                "case {case:?}: expected AccessDenied, got {body}"
            );
            let message = element(&body, "Message");
            let expected = format!(
                "is not authorized to perform: {iam_action} on resource: mfa device \
                 {serial_number}"
            );
            assert!(
                message.contains(&expected),
                "case {case:?}: expected the refusal to read {expected:?}, got {message:?}"
            );
            // The refused call changed nothing: the device still holds the fixture tag.
            let (held, _) = persisted_tags(&router, &case, &serial_number).await;
            assert_eq!(
                held,
                expected_map(&[("owner", "ops")]),
                "case {case:?}: a refused call must leave the tags as they were, got \
                 {held:?}"
            );
        }
    }
}

/// The duplicate refusal's text is IAM's one FIXED sentence for the device kind: whatever
/// path the held device sits under, whatever its name is spelled with and whatever either
/// create carries, the whole message is the same and quotes nothing of the request back.
///
/// The rows are the table above — path, name alphabet and tag count all vary — and each
/// repeat carries a DIFFERENT tag set from the create that was accepted, so a message built
/// out of the request would have to differ somewhere across them.
#[tokio::test]
async fn the_duplicate_refusal_is_one_fixed_sentence_whatever_the_request_named() {
    for row in PROVISIONED {
        let router = router();
        provision(&router, row).await;

        let repeat = Provisioned {
            case: row.case,
            path: row.path,
            name: row.name,
            // Tags where the accepted create carried none and none where it carried some.
            tags: if row.tags.is_empty() {
                &[("attempted", "again")]
            } else {
                &[]
            },
        };
        let params = repeat.params();
        let (status, body) = call(&router, "CreateVirtualMFADevice", &borrowed(&params)).await;
        assert_eq!(
            status, 409,
            "case {:?}: expected HTTP 409 from the second create, got {status}: {body}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            "EntityAlreadyExists",
            "case {:?}: expected EntityAlreadyExists, got {:?}: {body}",
            row.case,
            error_code(&body)
        );
        let message = element(&body, "Message");
        assert_eq!(
            message, DEVICE_ALREADY_EXISTS,
            "case {:?}: expected {DEVICE_ALREADY_EXISTS:?}, got {message:?}",
            row.case
        );
        for quoted in [Some(row.name), row.path, repeat.tags.first().map(|(key, _)| *key)] {
            let Some(quoted) = quoted else { continue };
            assert!(
                !message.contains(quoted),
                "case {:?}: expected a sentence carrying nothing of the request, \
                 but {message:?} carries {quoted:?}",
                row.case
            );
        }
    }
}

/// One kind of entity a duplicate create addresses: the create that is refused twice over
/// and the whole sentence IAM answers the second one with.
struct Kind {
    /// The case this row stands for, named in every assertion it makes.
    case: &'static str,
    /// The create operation the row calls twice.
    action: &'static str,
    /// That create's form parameters.
    params: Vec<(String, String)>,
    /// IAM's whole sentence for a duplicate of this kind.
    expected: String,
}

/// The RULE one level up from the device: a create for an entity the account already holds
/// is refused `EntityAlreadyExists` (HTTP 409) with the sentence IAM publishes for THAT
/// KIND — the identifier the kind is held under where that kind's sentence carries such a
/// clause, and nothing of the request where it does not, which is the virtual MFA device's
/// case.
///
/// The rows span the create families this service serves: the entities held by name (user,
/// group, role, instance profile), the managed policy whose sentence words the collision
/// its own way, and the device whose sentence is entirely fixed. The expected text is
/// written out here, not read from the implementation.
#[tokio::test]
async fn each_kinds_duplicate_create_carries_that_kinds_sentence() {
    // A policy document with a single statement — the form `CreatePolicy`'s own modelled
    // example carries — and a trust policy of the same shape, which a role create needs.
    const POLICY_DOCUMENT: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]}"#;
    const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

    let owned = |name: &str, value: &str| (name.to_string(), value.to_string());
    let rows = vec![
        Kind {
            case: "a user",
            action: "CreateUser",
            params: vec![owned("UserName", "duplicate-user")],
            expected: "User with name duplicate-user already exists.".to_string(),
        },
        Kind {
            case: "a group",
            action: "CreateGroup",
            params: vec![owned("GroupName", "duplicate-group")],
            expected: "Group with name duplicate-group already exists.".to_string(),
        },
        Kind {
            case: "a role",
            action: "CreateRole",
            params: vec![
                owned("RoleName", "duplicate-role"),
                owned("AssumeRolePolicyDocument", TRUST_POLICY),
            ],
            expected: "Role with name duplicate-role already exists.".to_string(),
        },
        Kind {
            case: "an instance profile",
            action: "CreateInstanceProfile",
            params: vec![owned("InstanceProfileName", "duplicate-profile")],
            expected: "Instance Profile duplicate-profile already exists.".to_string(),
        },
        Kind {
            case: "a managed policy",
            action: "CreatePolicy",
            params: vec![
                owned("PolicyName", "duplicate-policy"),
                owned("PolicyDocument", POLICY_DOCUMENT),
            ],
            expected: "A policy called duplicate-policy already exists. Duplicate names are \
                       not allowed."
                .to_string(),
        },
        Kind {
            case: "a virtual MFA device",
            action: "CreateVirtualMFADevice",
            params: vec![owned("VirtualMFADeviceName", "duplicate-device")],
            expected: DEVICE_ALREADY_EXISTS.to_string(),
        },
    ];

    let router = router();
    for row in &rows {
        let (status, body) = call(&router, row.action, &borrowed(&row.params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from the first create, got {status}: {body}",
            row.case
        );

        let (status, body) = call(&router, row.action, &borrowed(&row.params)).await;
        assert_eq!(
            status, 409,
            "case {:?}: expected HTTP 409 from the second create, got {status}: {body}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            "EntityAlreadyExists",
            "case {:?}: expected EntityAlreadyExists, got {:?}: {body}",
            row.case,
            error_code(&body)
        );
        let message = element(&body, "Message");
        assert_eq!(
            message, row.expected,
            "case {:?}: expected {:?}, got {message:?}",
            row.case, row.expected
        );
    }
}
