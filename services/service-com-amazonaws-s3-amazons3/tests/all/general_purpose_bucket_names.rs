// SPDX-License-Identifier: CC0-1.0

//! `CreateBucket` refuses a name Amazon S3 holds no general purpose bucket under.
//!
//! The rule under test: a create whose bucket name is outside the published *General
//! purpose bucket naming rules* is refused and nothing is recorded, so the account holds no
//! bucket under a name it could never address. WHICH refusal depends on which kind of
//! sentence the name broke:
//!
//! * a name in the namespace AWS keeps for its OWN buckets — the `sthree-` prefix, Amazon
//!   S3's internal one — is spelled the way a bucket name is spelled and what the caller may
//!   not do is hold it, so the refusal is the authorization one: HTTP 403 `AccessDenied` with
//!   the sentence that code publishes;
//! * every other sentence is about how the name is SPELLED — including the prefixes and
//!   suffixes reserved to OTHER S3 resource kinds, which no general purpose bucket exists
//!   under at all — which is HTTP 400 `InvalidBucketName` with the sentence S3's published
//!   error list states for that code and the rejected name in the `<BucketName>` member of the
//!   error document.
//!
//! A client branching on "the name is malformed, fix it" against "the name is not yours to
//! have" reads the two apart from the code, and retry logic keyed on one does not fire for the
//! other. The rules themselves are the ones every service in this workspace judges a
//! caller-supplied bucket name by (`service_common::s3_bucket_name`), which is where the
//! length bounds and the reserved prefix and suffix catalogs below come from.
//!
//! The tables span what the rule generalises over:
//!
//! * which SENTENCE of the rules the name breaks — the character class (an upper-case
//!   letter, an underscore, a space), the first and last character (a leading or trailing
//!   hyphen or period), two adjacent periods, a name formatted as an IP address, and every
//!   reserved prefix and suffix the catalog publishes, not only the one the report cited;
//! * the LENGTH bound, at each published edge and one character past it;
//! * names the rules ADMIT, including ones that carry a reserved prefix or suffix somewhere
//!   other than at the end they are reserved at, so the refusals above are about the rule
//!   and not about the text appearing at all.
//!
//! Every assertion drives a real HTTP request through the router (the harness
//! [`super::bucket_lifecycle`] owns), so what is pinned is the wire document.

use service_common::s3_bucket_name::{
    MAX_BUCKET_NAME_CHARS, MIN_BUCKET_NAME_CHARS, RESERVED_PREFIXES, RESERVED_SUFFIXES,
};

use super::bucket_lifecycle::{create_bucket, head_bucket, router};
use super::list_buckets::list_buckets;
use super::object_and_tagging_roundtrip::text;

/// The code S3's published error list states for a name whose SPELLING is outside the naming
/// rules, and the sentence it publishes beside it.
const INVALID_BUCKET_NAME: &str = "InvalidBucketName";

/// See [`INVALID_BUCKET_NAME`] — the fixed text of that error class.
const INVALID_BUCKET_NAME_MESSAGE: &str = "The specified bucket is not valid.";

/// The code S3 refuses a name in AWS's OWN bucket namespace with, and the sentence that code
/// publishes: the name is AWS's, not the caller's.
const ACCESS_DENIED: &str = "AccessDenied";

/// See [`ACCESS_DENIED`].
const ACCESS_DENIED_MESSAGE: &str = "Access Denied";

/// One row: the case, the name the create carries, whether S3 holds a bucket under it, the
/// refusal the create earns (`None` for a name the rules admit), and the status a `HeadBucket`
/// of that same name answers — 404 for a name a request can still ADDRESS a bucket by (it just
/// holds none), and 400 for one it cannot, which is refused for the name itself
/// ([`super::bucket_create_list_and_tagging_contracts`] pins that split across the
/// bucket-addressed calls). Stated per row rather than computed, so the rule and not the
/// implementation decides it.
#[derive(Debug, Clone)]
struct Row {
    case: String,
    bucket: String,
    held: bool,
    refusal: Option<Refusal>,
    head_status: u16,
}

/// What a refused create answers: the status, the code, the message, and whether the error
/// document names the rejected bucket in its `<BucketName>` member.
#[derive(Debug, Clone, Copy)]
struct Refusal {
    status: u16,
    code: &'static str,
    message: &'static str,
    names_the_bucket: bool,
}

/// The refusal a name whose SPELLING the rules do not admit earns.
const MALFORMED: Refusal = Refusal {
    status: 400,
    code: INVALID_BUCKET_NAME,
    message: INVALID_BUCKET_NAME_MESSAGE,
    names_the_bucket: true,
};

/// The refusal a name in AWS's OWN bucket namespace earns: the sentence states that the caller
/// may not have the name, and names nothing of the request.
const RESERVED: Refusal = Refusal {
    status: 403,
    code: ACCESS_DENIED,
    message: ACCESS_DENIED_MESSAGE,
    names_the_bucket: false,
};

impl Row {
    /// A row whose name the published rules ADMIT.
    fn admitted(case: &str, bucket: &str) -> Self {
        Self {
            case: case.to_string(),
            bucket: bucket.to_string(),
            held: true,
            refusal: None,
            head_status: 200,
        }
    }

    /// A row whose SPELLING the published create rules refuse and a request can still address
    /// a bucket by.
    fn refused(case: &str, bucket: impl Into<String>) -> Self {
        Self {
            case: case.to_string(),
            bucket: bucket.into(),
            held: false,
            refusal: Some(MALFORMED),
            head_status: 404,
        }
    }

    /// A row whose name is in the namespace AWS keeps for its own buckets.
    fn reserved(case: &str, bucket: impl Into<String>) -> Self {
        Self {
            case: case.to_string(),
            bucket: bucket.into(),
            held: false,
            refusal: Some(RESERVED),
            head_status: 404,
        }
    }

    /// A row whose name no request can address a bucket by either.
    fn unaddressable(case: &str, bucket: impl Into<String>) -> Self {
        Self {
            case: case.to_string(),
            bucket: bucket.into(),
            held: false,
            refusal: Some(MALFORMED),
            head_status: 400,
        }
    }
}

/// Every row of the naming table: one per sentence of the published rules, plus the names
/// those sentences admit.
///
/// The reserved prefix and suffix rows are generated from the catalogs themselves
/// ([`RESERVED_PREFIXES`], [`RESERVED_SUFFIXES`]), so each published entry is exercised —
/// including the ones the report that asked for this rule never mentioned.
fn rows() -> Vec<Row> {
    let mut rows = vec![
        Row::admitted("the shortest name the rules admit", &"a".repeat(MIN_BUCKET_NAME_CHARS)),
        Row::admitted("the longest name the rules admit", &"b".repeat(MAX_BUCKET_NAME_CHARS)),
        Row::admitted("lowercase letters, digits and hyphens", "team-7-data"),
        Row::admitted("periods between letters", "logs.2026.archive"),
        Row::admitted("both kinds of punctuation", "a-b.c-d"),
        Row::admitted("digits at both ends", "2026-quarterly-7"),
        // A reserved prefix is reserved at the START of the name and a reserved suffix at
        // its END: the same text elsewhere in the name is just text.
        Row::admitted("a reserved prefix somewhere other than the start", "logs-xn--archive"),
        Row::admitted("a reserved suffix somewhere other than the end", "bucket-s3alias-logs"),
        Row::admitted("a prefix one character short of a reserved one", "xn-bucket"),
        // The recorded case of the report this rule was asked for: a name carrying both an
        // upper-case letter and an underscore.
        Row::refused("an upper-case letter and an underscore", "Invalid_Bucket_Name"),
        Row::refused("upper-case letters alone", "uppercaseletterS"),
        Row::refused("an underscore alone", "under_score"),
        Row::unaddressable("a tilde", "tilde~name"),
        Row::unaddressable("a character outside the class", "br(acket)s"),
        Row::unaddressable("a leading hyphen", "-leading-hyphen"),
        Row::refused("a trailing hyphen", "trailing-hyphen-"),
        Row::unaddressable("a leading period", ".leading-period"),
        Row::refused("a trailing period", "trailing-period."),
        Row::refused("two adjacent periods", "two..periods"),
        Row::refused("a name formatted as an IP address", "192.168.5.4"),
        Row::refused("a name formatted as an IP address of long octets", "255.255.255.255"),
    ];
    for prefix in RESERVED_PREFIXES {
        // `sthree-` is Amazon S3's OWN internal namespace, so a name in it is refused the
        // authorization refusal; the other published prefixes belong to another spelling or
        // another S3 resource kind, under which no general purpose bucket exists at all.
        let name = format!("{prefix}quarterly-logs");
        let case = format!("the reserved prefix {prefix:?}");
        rows.push(match prefix {
            "sthree-" => Row::reserved(&case, name),
            _ => Row::refused(&case, name),
        });
    }
    for suffix in RESERVED_SUFFIXES {
        rows.push(Row::refused(
            &format!("the reserved suffix {suffix:?}"),
            format!("quarterly-logs{suffix}"),
        ));
    }
    rows
}

#[tokio::test]
async fn a_create_is_refused_for_every_name_the_published_rules_do_not_admit() {
    let router = router();

    // A neighbour held throughout, so a refusal below that wrote something would show up as
    // a change to the account's buckets rather than passing unnoticed.
    let neighbour = "naming-rules-neighbour";
    let held = create_bucket(&router, neighbour, None, None).await;
    assert_eq!(
        held.status,
        200,
        "the neighbour must be created first, got {} with body {:?}",
        held.status,
        text(&held)
    );

    let rows = rows();
    for row in &rows {
        let Row {
            case,
            bucket,
            held,
            refusal,
            head_status,
        } = row;
        let created = create_bucket(&router, bucket, None, None).await;
        if *held {
            assert_eq!(
                created.status, 200,
                "case {case:?}: S3 holds a bucket under {bucket:?}, expected HTTP 200, got \
                 {} with body {:?}",
                created.status,
                text(&created)
            );
            let read = head_bucket(&router, bucket).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the created bucket must read back, expected HTTP 200, got {}",
                read.status
            );
            continue;
        }

        let Refusal {
            status,
            code,
            message,
            names_the_bucket,
        } = refusal.expect("a row S3 holds no bucket for states the refusal it earns");
        assert_eq!(
            created.status, status,
            "case {case:?}: {bucket:?} is not a name S3 holds a bucket under, expected HTTP \
             {status}, got {} with body {:?}",
            created.status,
            text(&created)
        );
        assert_eq!(
            created.error_code(),
            code,
            "case {case:?}: expected the error code {code:?}, got {:?}",
            created.error_code()
        );
        assert_eq!(
            created.error_message(),
            message,
            "case {case:?}: expected the message {message:?}, got {:?}",
            created.error_message()
        );
        let named = created.element("BucketName");
        match names_the_bucket {
            true => assert_eq!(
                named.as_deref(),
                Some(bucket.as_str()),
                "case {case:?}: the error document must name the rejected bucket, got {named:?} \
                 in {:?}",
                text(&created)
            ),
            // The namespace refusal says nothing about the request: the sentence states that
            // the caller may not have the name, and no member echoes it back.
            false => assert_eq!(
                named, None,
                "case {case:?}: the refusal must echo nothing of the request, got {named:?} in \
                 {:?}",
                text(&created)
            ),
        }
        // Refused BEFORE anything is written: the bucket is not there afterwards. WHICH of
        // the two "no bucket here" answers a HEAD of the same name earns is the row's own
        // ([`Row`]): one S3 can ADDRESS no bucket by is refused for the name (400), and one
        // that is addressable but cannot be created — an upper-case letter, an underscore, a
        // reserved prefix — addresses a bucket that is simply not there (404).
        let read = head_bucket(&router, bucket).await;
        assert_eq!(
            read.status, *head_status,
            "case {case:?}: a refused create must leave no bucket behind, expected HTTP \
             {head_status}, got {}",
            read.status
        );
    }

    // The account holds exactly the neighbour and the names the rules admit — so no refused
    // create recorded a bucket under a name that cannot be addressed.
    let listing = list_buckets(&router, "").await;
    let document = text(&listing);
    for row in &rows {
        let Row {
            case,
            bucket,
            held,
            ..
        } = row;
        let reported = document.contains(&format!("<Name>{bucket}</Name>"));
        assert_eq!(
            reported, *held,
            "case {case:?}: expected the listing to report {bucket:?}={held}, got \
             {reported} in {document:?}"
        );
    }
    assert!(
        document.contains(&format!("<Name>{neighbour}</Name>")),
        "the neighbour must still be listed, got {document:?}"
    );
}

#[tokio::test]
async fn the_published_length_bound_holds_at_each_edge() {
    let router = router();

    // Each published edge of "between 3 (min) and 63 (max) characters long", and one
    // character past it on either side.
    for (length, admitted) in [
        (MIN_BUCKET_NAME_CHARS - 1, false),
        (MIN_BUCKET_NAME_CHARS, true),
        (MIN_BUCKET_NAME_CHARS + 1, true),
        (MAX_BUCKET_NAME_CHARS - 1, true),
        (MAX_BUCKET_NAME_CHARS, true),
        (MAX_BUCKET_NAME_CHARS + 1, false),
    ] {
        let bucket = "c".repeat(length);
        let created = create_bucket(&router, &bucket, None, None).await;
        let expected = if admitted { 200 } else { 400 };
        assert_eq!(
            created.status, expected,
            "case {length} characters: expected HTTP {expected}, got {} with body {:?}",
            created.status,
            text(&created)
        );
        if admitted {
            continue;
        }
        assert_eq!(
            created.error_code(),
            INVALID_BUCKET_NAME,
            "case {length} characters: expected the error code {INVALID_BUCKET_NAME:?}, got \
             {:?}",
            created.error_code()
        );
        assert_eq!(
            created.element("BucketName").as_deref(),
            Some(bucket.as_str()),
            "case {length} characters: the error document must name the rejected bucket, got \
             {:?}",
            created.element("BucketName")
        );
    }
}
