// SPDX-License-Identifier: CC0-1.0

//! The virtual MFA devices an account provisions: what a create records, what it reports
//! once and only once, and what a later read of the account's devices reports.
//!
//! The rule under test: `CreateVirtualMFADevice` records a device the ACCOUNT holds,
//! identified by its path and name together — the `mfa` ARN of the two, which is the
//! serial number every later call addresses the device by — and answers with that serial
//! beside the two secrets the device is provisioned with: a base32 seed and a QR code PNG
//! of the `otpauth://` URI carrying that seed. A second create under one path and name is
//! a conflict rather than a second device, and a later read of the account's devices
//! reports what was recorded without the secrets.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the path
//! the device is created under, the characters its name is spelled with, how many tags it
//! carries, and how many devices the account holds. The recorded case — one device at the
//! default path — is one row.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns and the other suites
//! in this crate share.

use crate::user_lifecycle::{account_id, call, element, elements, error_code, router};

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

