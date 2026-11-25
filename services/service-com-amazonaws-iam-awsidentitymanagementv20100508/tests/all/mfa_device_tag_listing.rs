// SPDX-License-Identifier: CC0-1.0

//! `ListMFADeviceTags`, end to end over the awsQuery wire.
//!
//! Every assertion drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, so what is under test is the
//! HTTP behaviour (status, awsQuery error code, XML members) of tags that were PERSISTED
//! by `CreateVirtualMFADevice` and `TagMFADevice`, never a handler called directly. The
//! awsQuery harness is the one `tests/all/user_lifecycle.rs` owns and the device helpers
//! are `tests/all/virtual_mfa_devices.rs`'s, so a device is provisioned and its tags are
//! written and read exactly one way across this crate.
//!
//! The rule under test is that the listing reports the tag set the device the SERIAL
//! NUMBER names persists — that device's own set and no other's, "sorted by tag key" and
//! paged over that order — while a value the published form of either addressing parameter
//! does not admit is refused as a VALUE before any device is consulted, a well-formed
//! serial the account holds no device under is refused as an entity that does not exist,
//! and a well-formed `Marker` this service did not issue for THESE tags is refused as a
//! token.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: how many
//! tags a device holds, whether two devices share a key, the length and alphabet of the
//! serial number, the length and alphabet of the marker, and which collection a marker was
//! issued for. The recorded case — one device, one tag set, no marker — is one row.

use std::collections::BTreeMap;

use smithy_server_runtime::MultiServiceRouter;

use crate::role_lifecycle::reported_tags;
use crate::user_lifecycle::{account_id, call, element, elements, error_code, router};
use crate::virtual_mfa_devices::{list_tags, not_found_message, tag, Provisioned};

/// The code IAM answers a value its published form does not admit with, decided by the
/// centralized request walk before the handler runs.
const VALUE_REFUSAL_CODE: &str = "ValidationError";

/// The code the operation's own modelled client-input error renders on the query wire:
/// `InvalidInputException` carries `awsQueryError` code `InvalidInput`
/// (`smithy_query(query="operation", operation="ListMFADeviceTags")`).
const INPUT_REFUSAL_CODE: &str = "InvalidInput";

/// IAM's whole sentence for a `Marker` it did not issue, written out here rather than read
/// from the implementation: what this suite pins is the sentence the service answers, so a
/// reworded constant has to fail here rather than agree with itself.
const INVALID_MARKER: &str = "Invalid Marker.";

/// The published bounds of `SerialNumber`: `serialNumberType` is len 9..256, pattern
/// `^[\w+=/:,.@-]+$` (`smithy_query(query="operation", operation="ListMFADeviceTags")`).
const MIN_SERIAL_CHARACTERS: usize = 9;
const MAX_SERIAL_CHARACTERS: usize = 256;

/// The published upper bound of `Marker`: `markerType` is len 1..320, pattern
/// `^[\u0020-\u00FF]+$` (the same model query).
const MAX_MARKER_CHARACTERS: usize = 320;

/// One device of the account, with the tag set it is expected to report.
struct Device {
    /// The case this row stands for, named in every assertion it makes.
    case: &'static str,
    /// The `Path` the device is created under, or `None` for the account root.
    path: Option<&'static str>,
    /// The `VirtualMFADeviceName` the device is created with.
    name: &'static str,
    /// The tags the create attaches, in REQUEST order — which is not the sorted order for
    /// most rows, because the sorting is part of the rule under test.
    tags: &'static [(&'static str, &'static str)],
}

/// The devices one account holds at once. Rows span the tag count (none, one, several),
/// the path, and — this is what the per-device half of the rule generalises over — the
/// SAME key carrying a different value on more than one device, so a listing that reported
/// the account's tags rather than the device's would differ here.
const DEVICES: &[Device] = &[
    Device {
        case: "a device carrying no tags at all",
        path: None,
        name: "untagged-device",
        tags: &[],
    },
    Device {
        case: "one tag, under a nested path",
        path: Some("/engineering/"),
        name: "single-tag-device",
        tags: &[("owner", "sre")],
    },
    Device {
        case: "a shared key carrying another value, arriving out of key order",
        path: Some("/engineering/platform/"),
        name: "shared_key-device",
        tags: &[("owner", "platform"), ("Department", "Engineering")],
    },
    Device {
        case: "several tags spanning the published alphabet",
        path: Some("/contractors/"),
        name: "a.b+c=d,e@f-g_0",
        tags: &[
            ("zone", "c"),
            ("Cost Center", "Human Resources"),
            ("owner", "contractors"),
            ("Ünicode", "Grüße"),
            ("7", "seven"),
            ("empty-value", ""),
        ],
    },
];

impl Device {
    /// This row as the device helper the create suite owns takes it.
    fn provisioned(&self) -> Provisioned {
        Provisioned {
            case: self.case,
            path: self.path,
            name: self.name,
            tags: self.tags,
        }
    }

    /// The tag set this device must report, as a map.
    fn expected(&self) -> BTreeMap<String, String> {
        self.tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    /// The tag keys in the order the listing must report them: ascending by key.
    fn sorted_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.tags.iter().map(|(key, _)| (*key).to_string()).collect();
        keys.sort();
        keys
    }
}

/// Provision every row of [`DEVICES`] in one account, answering with each row's serial
/// number in table order.
async fn provision_all(router: &MultiServiceRouter) -> Vec<String> {
    let mut serials = Vec::new();
    for row in DEVICES {
        let (serial_number, _, _) =
            crate::virtual_mfa_devices::provision(router, &row.provisioned()).await;
        serials.push(serial_number);
    }
    serials
}

/// The whole tag set a listing reports for `serial_number`, with the key order it reported.
async fn reported(
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
        "case {case:?}: a set this size fits one default page: {body}"
    );
    assert!(
        elements(&body, "Marker").is_empty(),
        "case {case:?}: an untruncated page carries no Marker: {body}"
    );
    (reported_tags(&body), elements(&body, "Key"))
}

/// RULE: the listing reports the tag set the device the serial number names persists — that
/// device's own set, sorted by tag key — whatever tags the other devices of the account
/// hold, and a write to one device moves no other device's set.
#[tokio::test]
async fn each_device_reports_only_the_tags_it_persists() {
    let router = router();
    let serials = provision_all(&router).await;

    for (row, serial_number) in DEVICES.iter().zip(&serials) {
        let (held, order) = reported(&router, row.case, serial_number).await;
        assert_eq!(
            held,
            row.expected(),
            "case {:?}: expected the listing to report exactly that device's tags, got \
             {held:?}",
            row.case
        );
        assert_eq!(
            order,
            row.sorted_keys(),
            "case {:?}: expected the keys sorted by key, got {order:?}",
            row.case
        );
    }

    // A write addressed to ONE device: every other device's listing must be exactly what it
    // was, which a set keyed by the account rather than the device could not be.
    let written = &DEVICES[1];
    let target = &serials[1];
    let (status, body) = tag(&router, target, &[("owner", "rewritten"), ("new", "key")]).await;
    assert_eq!(
        status, 200,
        "case {:?}: the fixture tag write must be served, got {status}: {body}",
        written.case
    );

    for (index, (row, serial_number)) in DEVICES.iter().zip(&serials).enumerate() {
        let case = format!("{} / after a write to another device", row.case);
        let (held, _) = reported(&router, &case, serial_number).await;
        let mut expected = row.expected();
        if index == 1 {
            expected.insert("owner".to_string(), "rewritten".to_string());
            expected.insert("new".to_string(), "key".to_string());
        }
        assert_eq!(
            held, expected,
            "case {case:?}: expected only the written device's set to have moved, got \
             {held:?}"
        );
    }
}

/// One serial number the listing is addressed by, and what it is answered with.
struct Serial {
    /// The case this row stands for, named in every assertion it makes.
    case: &'static str,
    /// The `SerialNumber` the request carries.
    value: String,
    /// `true` when the published form admits the value, so the HANDLER answers it — which
    /// for a serial the account holds no device under is an entity that does not exist.
    well_formed: bool,
}

/// RULE: a `SerialNumber` outside the form the model publishes is refused as a VALUE before
/// any device is consulted, while one the form admits is answered by the handler — an
/// entity that does not exist when the account holds no device under it.
#[tokio::test]
async fn a_serial_number_is_answered_by_its_published_form_first() {
    let router = router();
    let serials = provision_all(&router).await;
    let held = &serials[2];

    // Rows span each published length bound and one past it, and the alphabet: a space and
    // a punctuation mark `^[\w+=/:,.@-]+$` does not admit, against the `:` `/` and `-` it
    // does (which every `mfa` ARN is spelled with).
    let rows = vec![
        Serial {
            case: "one character short of the published minimum",
            value: "a".repeat(MIN_SERIAL_CHARACTERS - 1),
            well_formed: false,
        },
        Serial {
            case: "exactly the published minimum",
            value: "a".repeat(MIN_SERIAL_CHARACTERS),
            well_formed: true,
        },
        Serial {
            case: "exactly the published maximum",
            value: "a".repeat(MAX_SERIAL_CHARACTERS),
            well_formed: true,
        },
        Serial {
            case: "one character past the published maximum",
            value: "a".repeat(MAX_SERIAL_CHARACTERS + 1),
            well_formed: false,
        },
        Serial {
            case: "a serial carrying a space",
            value: "GAHT1234 5678".to_string(),
            well_formed: false,
        },
        Serial {
            case: "a serial carrying a character the pattern does not admit",
            value: "serial!number".to_string(),
            well_formed: false,
        },
        Serial {
            case: "a well-formed mfa ARN the account holds no device under",
            value: format!("arn:aws:iam::{}:mfa/never-created", account_id()),
            well_formed: true,
        },
    ];

    for row in &rows {
        let (status, body) = list_tags(&router, &row.value, None, None).await;
        if row.well_formed {
            assert_eq!(
                status, 404,
                "case {:?}: expected HTTP 404 from the handler, got {status}: {body}",
                row.case
            );
            assert_eq!(
                error_code(&body),
                "NoSuchEntity",
                "case {:?}: expected NoSuchEntity, got {body}",
                row.case
            );
            assert_eq!(
                element(&body, "Message"),
                not_found_message(&row.value),
                "case {:?}: expected IAM's wording for a serial that names no device: {body}",
                row.case
            );
        } else {
            assert_eq!(
                status, 400,
                "case {:?}: expected HTTP 400 for a value the form does not admit, got \
                 {status}: {body}",
                row.case
            );
            assert_eq!(
                error_code(&body),
                VALUE_REFUSAL_CODE,
                "case {:?}: expected {VALUE_REFUSAL_CODE}, got {body}",
                row.case
            );
        }
    }

    // None of those refusals touched the account: the device that DOES hold tags still
    // reports exactly them.
    let (after, _) = reported(&router, "after the refusals", held).await;
    assert_eq!(
        after,
        DEVICES[2].expected(),
        "expected the refused reads to have left the device's tags alone, got {after:?}"
    );
}

/// One `Marker` the listing is paged with, and what it is answered with.
struct Marker {
    /// The case this row stands for, named in every assertion it makes.
    case: String,
    /// The `Marker` the request carries.
    value: String,
    /// `true` when the published form admits the value, so the handler answers it — which
    /// for a token it did not issue for THESE tags is an invalid input.
    well_formed: bool,
}

/// RULE: a `Marker` outside the form the model publishes is refused as a VALUE, one the form
/// admits but this service did not issue while paging THESE tags is refused as an invalid
/// input under the operation's own modelled error, and the token the previous page carried
/// resumes strictly after the last key that page reported.
#[tokio::test]
async fn a_marker_is_answered_by_its_published_form_then_by_what_issued_it() {
    let router = router();
    let serials = provision_all(&router).await;
    let row = &DEVICES[3];
    let serial_number = &serials[3];

    // The token a truncated page of ANOTHER device's tags carries, and one a truncated page
    // of ANOTHER COLLECTION (the account's devices) carries: both are tokens this service
    // issued, neither names a position in this device's tag order.
    let (status, other_page) = list_tags(&router, &serials[2], Some(1), None).await;
    assert_eq!(
        status, 200,
        "the fixture page of another device's tags must be served, got {status}: {other_page}"
    );
    let other_device_token = element(&other_page, "Marker");
    let (status, devices_page) = call(&router, "ListVirtualMFADevices", &[("MaxItems", "1")]).await;
    assert_eq!(
        status, 200,
        "the fixture page of the account's devices must be served, got {status}: {devices_page}"
    );
    let other_collection_token = element(&devices_page, "Marker");

    let rows = vec![
        Marker {
            case: "one character past the published maximum length".to_string(),
            value: "A".repeat(MAX_MARKER_CHARACTERS + 1),
            well_formed: false,
        },
        Marker {
            case: "a marker carrying a character outside the published range".to_string(),
            value: "page-\u{2603}".to_string(),
            well_formed: false,
        },
        Marker {
            case: "the shortest marker the published form admits".to_string(),
            value: "A".to_string(),
            well_formed: true,
        },
        Marker {
            case: "exactly the published maximum length".to_string(),
            value: "A".repeat(MAX_MARKER_CHARACTERS),
            well_formed: true,
        },
        Marker {
            case: "a token that is not this service's at all".to_string(),
            value: "not-a-marker".to_string(),
            well_formed: true,
        },
        Marker {
            case: "a token issued while paging another device's tags".to_string(),
            value: other_device_token,
            well_formed: true,
        },
        Marker {
            case: "a token issued while paging another collection".to_string(),
            value: other_collection_token,
            well_formed: true,
        },
    ];

    for marker in &rows {
        let (status, body) = list_tags(&router, serial_number, Some(1), Some(&marker.value)).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status}: {body}",
            marker.case
        );
        let expected_code = if marker.well_formed {
            INPUT_REFUSAL_CODE
        } else {
            VALUE_REFUSAL_CODE
        };
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {:?}: expected {expected_code}, got {body}",
            marker.case
        );
        if marker.well_formed {
            assert_eq!(
                element(&body, "Message"),
                INVALID_MARKER,
                "case {:?}: expected {INVALID_MARKER:?}, got {body}",
                marker.case
            );
        }
    }

    // The token the page itself carried resumes STRICTLY after the last key it reported, so
    // the two pages together are the device's set, each key once.
    let (status, first) = list_tags(&router, serial_number, Some(1), None).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from the first page, got {status}: {first}"
    );
    let first_keys = elements(&first, "Key");
    let token = element(&first, "Marker");
    let (status, second) = list_tags(
        &router,
        serial_number,
        Some(row.tags.len() as i32),
        Some(&token),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from the resumed page, got {status}: {second}"
    );
    let mut walked = first_keys;
    walked.extend(elements(&second, "Key"));
    assert_eq!(
        walked,
        row.sorted_keys(),
        "expected the resumed walk to report every key exactly once in key order, got \
         {walked:?}"
    );
}

/// RULE: the listing is a READ — the tags it reports, the device record the account's own
/// read reports and the page a later caller is served are all exactly what they were before
/// it was called, whether it was called once, paged, or refused.
#[tokio::test]
async fn the_listing_leaves_the_account_as_a_later_caller_finds_it() {
    let router = router();
    let serials = provision_all(&router).await;
    // The account's own read, as the serials it reports: the request id of an envelope is
    // minted per call, so what is compared is the state the read reports.
    let (status, body) = call(&router, "ListVirtualMFADevices", &[]).await;
    assert_eq!(
        status, 200,
        "the account's devices must read back before the listings, got {status}: {body}"
    );
    let before = elements(&body, "SerialNumber");

    for (row, serial_number) in DEVICES.iter().zip(&serials) {
        // One whole read, one paged walk at the smallest page the form admits, and one
        // refused read — none of which may move anything.
        let (first, first_order) = reported(&router, row.case, serial_number).await;
        let mut marker: Option<String> = None;
        let mut pages = 0;
        loop {
            let (status, body) = list_tags(&router, serial_number, Some(1), marker.as_deref()).await;
            assert_eq!(
                status, 200,
                "case {:?}: expected HTTP 200 from a page, got {status}: {body}",
                row.case
            );
            if element(&body, "IsTruncated") != "true" {
                break;
            }
            marker = Some(element(&body, "Marker"));
            pages += 1;
            assert!(
                pages <= row.tags.len() + 1,
                "case {:?}: pagination did not terminate after {pages} pages",
                row.case
            );
        }
        let (status, body) = list_tags(&router, serial_number, Some(1), Some("not-a-marker")).await;
        assert_eq!(
            status, 400,
            "case {:?}: the fixture refusal must be a refusal, got {status}: {body}",
            row.case
        );

        let (second, second_order) = reported(&router, row.case, serial_number).await;
        assert_eq!(
            first, second,
            "case {:?}: two listings must report the same tags, got {first:?} then {second:?}",
            row.case
        );
        assert_eq!(
            first_order, second_order,
            "case {:?}: two listings must report the same key order",
            row.case
        );
    }

    let (status, body) = call(&router, "ListVirtualMFADevices", &[]).await;
    assert_eq!(
        status, 200,
        "the account's devices must read back after the listings, got {status}: {body}"
    );
    let after = elements(&body, "SerialNumber");
    assert_eq!(
        after, before,
        "expected the account's devices to read back exactly as they did before the tags \
         were listed, got {after:?}"
    );
}

/// RULE: the `Tags` member is REQUIRED, so a device holding none reports the member as an
/// empty list rather than withholding the page — "if no tags are attached to the specified
/// resource, the response contains an empty list" — and the page is not truncated.
#[tokio::test]
async fn a_device_holding_no_tags_reports_an_empty_page() {
    let router = router();
    let serials = provision_all(&router).await;

    // Rows: a device created without tags, and one whose every tag has since been removed —
    // the two ways a set is empty.
    let untagged = &serials[0];
    let emptied = &serials[1];
    let (status, body) = call(
        &router,
        "UntagMFADevice",
        &[
            ("SerialNumber", emptied.as_str()),
            ("TagKeys.member.1", "owner"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture untag must be served, got {status}: {body}"
    );

    for (case, serial_number) in [
        ("a device created without tags", untagged),
        ("a device whose tags were all removed", emptied),
    ] {
        // The page size the request carries: absent (the documented default of 100) and
        // the published minimum of 1, so neither reading can truncate an empty set.
        for page_size in [None, Some(1)] {
            let (status, body) = list_tags(&router, serial_number, page_size, None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status}: {body}"
            );
            assert!(
                elements(&body, "Key").is_empty(),
                "case {case:?}: expected no tag to be reported, got {body}"
            );
            assert_eq!(
                element(&body, "IsTruncated"),
                "false",
                "case {case:?}: an empty set is never truncated: {body}"
            );
            assert!(
                elements(&body, "Marker").is_empty(),
                "case {case:?}: an untruncated page carries no Marker: {body}"
            );
        }
    }
}
