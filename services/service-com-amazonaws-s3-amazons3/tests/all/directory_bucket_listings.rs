// SPDX-License-Identifier: CC0-1.0

//! S3 Express One Zone: what the two bucket LISTINGS report, and what a listing of the
//! objects IN a directory bucket may ask for.
//!
//! Every assertion drives a real HTTP request through the router (the harness the
//! bucket-lifecycle and object suites in this crate already own), so what is pinned is the
//! wire behaviour — status, error code, error message and the names a listing reports.
//!
//! The rules under test, with their sources:
//!
//! * the two kinds of bucket are reported by two different operations, each on its own
//!   endpoint: `ListBuckets` "returns a list of all buckets owned by the authenticated
//!   sender of the request" and "is not supported for directory buckets", while
//!   `ListDirectoryBuckets` "returns a list of all Amazon S3 directory buckets owned by the
//!   authenticated sender of the request" (their own documentation) — so neither listing
//!   reports the other's buckets. The directory-bucket listing is served by
//!   `s3express-control.<region>.amazonaws.com`, whose signing name is `s3express`, which is
//!   what tells the two apart on one emulator host;
//! * that listing pages by `MaxDirectoryBuckets` (`range {min 0, max 1000}`) and resumes at
//!   the `ContinuationToken` the previous page handed back, which "is obfuscated and is not a
//!   real bucket name";
//! * a listing of the objects in a directory bucket may ask only for what that bucket type
//!   serves: "for directory buckets, / is the only supported delimiter"
//!   (`ListObjectsV2Request$Delimiter`), `StartAfter`'s "functionality is not supported for
//!   directory buckets" (`$StartAfter`), and "only prefixes that end in a delimiter (/) are
//!   supported" (`$Prefix`) — each refused with the code, status and sentence S3 answers it
//!   with, and each served as asked on a GENERAL PURPOSE bucket, which is the other half of
//!   the same rule;
//! * the partition holds for a bucket created the way a client creates a directory bucket —
//!   `PUT /<bucket>` on `s3express-control.<region>.amazonaws.com`, "you must use path-style
//!   requests in the format `https://s3express-control.region-code.amazonaws.com/bucket-name`"
//!   (`CreateBucket`) — and such a bucket is sessionable from the moment it exists, which is
//!   the positive control the partition is read against;
//! * a bucket this account does not hold is refused `NoSuchBucket` with HTTP 404 —
//!   `CreateSession`'s single modelled error (`httpError 404`) — in the sentence the SURFACE
//!   that name belongs to words it: the directory-bucket surface's ends in a full stop where
//!   the general purpose surface's does not.

use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send, send_bytes_signed, send_in_region, Answer};
use super::directory_bucket_sessions_and_appends::directory_bucket;
use super::object_and_tagging_roundtrip::{bucket, put_object, text};

/// The credential scope a client of the S3 EXPRESS control endpoint presents: the signing
/// name `s3express`, which is what tells that endpoint's `GET /` (the directory-bucket
/// listing) from the general purpose endpoint's. Neither the key id nor the signature is a
/// credential here (this suite's router verifies none, as the other suites' do).
const S3_EXPRESS_AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/s3express/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// The host the S3 Express control endpoint is addressed as.
const S3_EXPRESS_CONTROL_HOST: &str = "s3express-control.us-east-1.amazonaws.com";

/// `GET /` signed for the S3 EXPRESS signing name — `ListDirectoryBuckets`, as a client
/// addressing `s3express-control.<region>.amazonaws.com` sends it. Driven through the one
/// place this crate's harness puts a request on the wire.
async fn list_directory_buckets(router: &MultiServiceRouter, query: &str) -> Answer {
    send_bytes_signed(
        router,
        S3_EXPRESS_AUTHORIZATION,
        "GET",
        S3_EXPRESS_CONTROL_HOST,
        &format!("/{query}"),
        &[],
        b"",
    )
    .await
}

/// Every `<Name>` element of a bucket listing's body, in the order the document carries them
/// — read through the harness's own repeated-element reader ([`Answer::elements`]).
fn listed_names(answer: &Answer) -> Vec<String> {
    answer.elements("Name")
}

/// The `<ContinuationToken>` a page handed back, when it handed one back.
fn continuation_token(answer: &Answer) -> Option<String> {
    answer
        .element("ContinuationToken")
        .filter(|token| !token.is_empty())
}

/// The ONE Region the two tests below drive every call against, so what the two listings are
/// asserted to partition is the KIND of each bucket and nothing else.
const EXPRESS_REGION: &str = "us-west-2";

/// [`S3_EXPRESS_AUTHORIZATION`] scoped to `region`: the credential scope a client that
/// reached the S3 Express surface of that Region presents.
fn express_authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{region}/s3express/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// The host the S3 Express control endpoint of `region` is addressed as — "the HTTP Host
/// header syntax is `s3express-control.region.amazonaws.com`" (`CreateBucket`'s own
/// documentation of its directory-bucket surface).
fn express_control_host(region: &str) -> String {
    format!("s3express-control.{region}.amazonaws.com")
}

/// The `CreateBucketConfiguration` that asks for a DIRECTORY bucket in `zone_id`: the zone it
/// is placed in (`Location`) and the kind of bucket it is (`Bucket`), in the model's own
/// element order.
fn directory_configuration(location_type: &str, zone_id: &str, data_redundancy: &str) -> String {
    format!(
        "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Location><Type>{location_type}</Type><Name>{zone_id}</Name></Location>\
         <Bucket><DataRedundancy>{data_redundancy}</DataRedundancy><Type>Directory</Type>\
         </Bucket></CreateBucketConfiguration>"
    )
}

/// `PUT /<name>` on the S3 Express CONTROL endpoint of `region` — the create a client sends
/// for a directory bucket: "you must use path-style requests in the format
/// `https://s3express-control.region-code.amazonaws.com/bucket-name`" (`CreateBucket`).
async fn create_directory_bucket_on_the_control_endpoint(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    configuration: &str,
) -> Answer {
    send_bytes_signed(
        router,
        &express_authorization(region),
        "PUT",
        &express_control_host(region),
        &format!("/{name}"),
        &[("content-type", "application/xml")],
        configuration.as_bytes(),
    )
    .await
}

/// `GET /<name>?session` signed for the S3 Express surface of `region` — `CreateSession`, the
/// call a client makes before it reaches the Zonal data plane of a directory bucket.
async fn create_session(router: &MultiServiceRouter, region: &str, name: &str) -> Answer {
    send_bytes_signed(
        router,
        &express_authorization(region),
        "GET",
        &express_control_host(region),
        &format!("/{name}?session"),
        &[],
        b"",
    )
    .await
}

/// `GET /<query>` on the S3 Express control endpoint of `region` — `ListDirectoryBuckets`.
async fn list_directory_buckets_in(
    router: &MultiServiceRouter,
    region: &str,
    query: &str,
) -> Answer {
    send_bytes_signed(
        router,
        &express_authorization(region),
        "GET",
        &express_control_host(region),
        &format!("/{query}"),
        &[],
        b"",
    )
    .await
}

/// The `CreateBucketConfiguration` that places a GENERAL PURPOSE bucket in `region`.
fn location_constraint(region: &str) -> String {
    format!(
        "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <LocationConstraint>{region}</LocationConstraint></CreateBucketConfiguration>"
    )
}

/// The every-bucket name a page of `ListDirectoryBuckets` reports, walked to exhaustion the
/// way Express tooling walks it: a page size and the `ContinuationToken` the previous page
/// handed back ("you can use this ContinuationToken for the pagination of the list results",
/// `ListDirectoryBucketsRequest$ContinuationToken`).
async fn every_directory_bucket(
    router: &MultiServiceRouter,
    region: &str,
    page_size: i32,
) -> Vec<String> {
    // Generously more rounds than there are buckets to report, so a listing that handed back
    // a cursor for ever is a failed assertion rather than a test that never ends.
    const MOST_PAGES: usize = 64;
    let mut collected: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..MOST_PAGES {
        let query = match token.as_deref() {
            Some(resume) => {
                format!(
                    "?max-directory-buckets={page_size}&continuation-token={}",
                    super::continuation_token_alphabet::handed_back_in_query(resume)
                )
            }
            None => format!("?max-directory-buckets={page_size}"),
        };
        let page = list_directory_buckets_in(router, region, &query).await;
        assert_eq!(
            page.status,
            200,
            "case \"a page of max-directory-buckets={page_size}\": expected HTTP 200, got {} \
             with body {:?}",
            page.status,
            text(&page)
        );
        collected.extend(listed_names(&page));
        match continuation_token(&page) {
            Some(next) => token = Some(next),
            None => return collected,
        }
    }
    panic!("case \"paging the directory-bucket listing\": paging must terminate");
}

/// RULE: a bucket's KIND is part of its identity, so the two account-wide listings partition
/// the account between them — `ListDirectoryBuckets` reports the S3 Express One Zone
/// directory buckets and `ListBuckets` the general purpose ones, neither reporting the
/// other's — whatever endpoint the bucket was created through, and however the directory
/// listing is paged.
///
/// The rows differ along what the rule generalises over: the kind of bucket, and for a
/// directory bucket the kind of zone it is placed in (two Availability Zones and a Local
/// Zone), each created through the S3 Express control endpoint of its own Region the way a
/// client creates one. Each directory bucket is sessioned right after its create, which is
/// the positive control the partition is read against: a name the directory listing reports
/// is a name the Express surface serves.
#[tokio::test]
async fn the_two_account_listings_partition_the_account_by_bucket_kind() {
    let router = router();

    // (case, name, location type, zone id, data redundancy)
    let directory_rows: [(&str, &str, &str, &str, &str); 3] = [
        (
            "an Availability Zone bucket",
            "expart-a--usw2-az1--x-s3",
            "AvailabilityZone",
            "usw2-az1",
            "SingleAvailabilityZone",
        ),
        (
            "another Availability Zone's bucket",
            "expart-b--usw2-az2--x-s3",
            "AvailabilityZone",
            "usw2-az2",
            "SingleAvailabilityZone",
        ),
        (
            "a Local Zone bucket",
            "expart-c--usw2-lax1-az1--x-s3",
            "LocalZone",
            "usw2-lax1-az1",
            "SingleLocalZone",
        ),
    ];
    for (case, name, location_type, zone_id, redundancy) in directory_rows {
        let created = create_directory_bucket_on_the_control_endpoint(
            &router,
            EXPRESS_REGION,
            name,
            &directory_configuration(location_type, zone_id, redundancy),
        )
        .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: the create must be served on the Express control endpoint, got {} \
             with body {:?}",
            created.status,
            text(&created)
        );
        let session = create_session(&router, EXPRESS_REGION, name).await;
        assert_eq!(
            session.status,
            200,
            "case {case:?}: the bucket must be sessionable from the moment it exists, got {} \
             with body {:?}",
            session.status,
            text(&session)
        );
        assert!(
            !session
                .element("AccessKeyId")
                .unwrap_or_default()
                .is_empty(),
            "case {case:?}: the session must carry credentials, got {:?}",
            text(&session)
        );
    }

    // The GENERAL PURPOSE buckets of the same account in the same Region, created through
    // that Region's own endpoint.
    let general_purpose_rows: [(&str, &str); 2] = [
        ("a general purpose bucket", "expart-general-purpose"),
        ("another general purpose bucket", "expart-plain-bucket"),
    ];
    for (case, name) in general_purpose_rows {
        let created = send_in_region(
            &router,
            EXPRESS_REGION,
            "PUT",
            &format!("/{name}"),
            &[],
            &location_constraint(EXPRESS_REGION),
        )
        .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: expected HTTP 200, got {} with body {:?}",
            created.status,
            text(&created)
        );
    }

    let expected_directory: Vec<String> = directory_rows
        .iter()
        .map(|(_, name, _, _, _)| (*name).to_string())
        .collect();
    let expected_general_purpose: Vec<String> = general_purpose_rows
        .iter()
        .map(|(_, name)| (*name).to_string())
        .collect();

    // The directory listing, paged every way the model admits: the member's own ceiling (the
    // page size Express tooling asks for), a page that holds one bucket, and the page the
    // request names no size at all for.
    for (case, page_size) in [("the model's maximum page", 1_000), ("one bucket a page", 1)] {
        let listed = every_directory_bucket(&router, EXPRESS_REGION, page_size).await;
        assert_eq!(
            listed, expected_directory,
            "case {case:?}: ListDirectoryBuckets must report this account's directory buckets \
             and no general purpose bucket, got {listed:?}"
        );
    }
    let unpaged = list_directory_buckets_in(&router, EXPRESS_REGION, "").await;
    assert_eq!(
        listed_names(&unpaged),
        expected_directory,
        "case \"no page size at all\": ListDirectoryBuckets must report this account's \
         directory buckets and no general purpose bucket, got {:?}",
        listed_names(&unpaged)
    );

    let general_purpose = send_in_region(&router, EXPRESS_REGION, "GET", "/", &[], "").await;
    assert_eq!(
        general_purpose.status,
        200,
        "case \"the general purpose listing\": expected HTTP 200, got {} with body {:?}",
        general_purpose.status,
        text(&general_purpose)
    );
    assert_eq!(
        listed_names(&general_purpose),
        expected_general_purpose,
        "case \"the general purpose listing\": ListBuckets must report this account's general \
         purpose buckets and no directory bucket, got {:?}",
        listed_names(&general_purpose)
    );
}

/// The sentence the DIRECTORY-bucket surface refuses a bucket it does not hold with, as
/// recorded against `CreateSession` — the same condition the general purpose surface words
/// below, terminated with a full stop.
const DIRECTORY_BUCKET_ABSENT_SENTENCE: &str = "The specified bucket does not exist.";

/// The sentence the GENERAL PURPOSE surface words the same condition in (Amazon S3's
/// published error list).
const GENERAL_PURPOSE_ABSENT_SENTENCE: &str = "The specified bucket does not exist";

/// RULE: a request about a bucket this account does not hold is refused `NoSuchBucket` with
/// HTTP 404 and the sentence the SURFACE that bucket name belongs to words — the
/// directory-bucket surface's ends in a full stop where the general purpose surface's does
/// not — and the refusal leaves no bucket behind.
///
/// The rows differ along what the rule generalises over: which surface the addressed name
/// belongs to (a directory-bucket name, in each of two zones, against a general purpose
/// one) — asked of `CreateSession`, whose single modelled error this is (`NoSuchBucket`,
/// `httpError 404`) — and then of a second operation of the general purpose surface, so what
/// decides the wording is the name's surface rather than the operation.
#[tokio::test]
async fn an_absent_bucket_is_refused_in_the_wording_of_its_own_surface() {
    let router = router();
    let held = "expmiss-held--usw2-az1--x-s3";
    let created = create_directory_bucket_on_the_control_endpoint(
        &router,
        EXPRESS_REGION,
        held,
        &directory_configuration("AvailabilityZone", "usw2-az1", "SingleAvailabilityZone"),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "the directory bucket {held:?} must be created first, got {} with body {:?}",
        created.status,
        text(&created)
    );

    // (case, the name the call addresses, the sentence that surface words this condition in)
    let rows: [(&str, &str, &str); 4] = [
        (
            "a directory-bucket name in the zone the held bucket is in",
            "expmiss-gone--usw2-az1--x-s3",
            DIRECTORY_BUCKET_ABSENT_SENTENCE,
        ),
        (
            "a directory-bucket name in another zone",
            "expmiss-gone--use1-az4--x-s3",
            DIRECTORY_BUCKET_ABSENT_SENTENCE,
        ),
        (
            "a directory-bucket name in a Local Zone",
            "expmiss-gone--usw2-lax1-az1--x-s3",
            DIRECTORY_BUCKET_ABSENT_SENTENCE,
        ),
        (
            "a general purpose bucket name",
            "expmiss-gone-general-purpose",
            GENERAL_PURPOSE_ABSENT_SENTENCE,
        ),
    ];
    for (case, name, sentence) in rows {
        let refused = create_session(&router, EXPRESS_REGION, name).await;
        assert_eq!(
            (
                refused.status,
                refused.error_code().as_str(),
                refused.error_message().as_str()
            ),
            (404, "NoSuchBucket", sentence),
            "case {case:?}: expected 404 NoSuchBucket {sentence:?}, got {} {:?} {:?}",
            refused.status,
            refused.error_code(),
            refused.error_message()
        );
    }

    // The general purpose surface words the same condition the same way on another of its
    // operations, so the wording belongs to the surface and not to `CreateSession`.
    let listed = send_in_region(
        &router,
        EXPRESS_REGION,
        "GET",
        "/expmiss-gone-general-purpose?list-type=2",
        &[],
        "",
    )
    .await;
    assert_eq!(
        (
            listed.status,
            listed.error_code().as_str(),
            listed.error_message().as_str()
        ),
        (404, "NoSuchBucket", GENERAL_PURPOSE_ABSENT_SENTENCE),
        "case \"a listing of a general purpose bucket nothing holds\": expected 404 \
         NoSuchBucket {GENERAL_PURPOSE_ABSENT_SENTENCE:?}, got {} {:?} {:?}",
        listed.status,
        listed.error_code(),
        listed.error_message()
    );

    // A refused call creates nothing and takes nothing away: the held bucket is still the
    // one directory bucket of the account, and it still sessions.
    let still_listed = list_directory_buckets_in(&router, EXPRESS_REGION, "").await;
    assert_eq!(
        listed_names(&still_listed),
        vec![held.to_string()],
        "case \"after the refusals\": no refused name may become a bucket, got {:?}",
        listed_names(&still_listed)
    );
    let session = create_session(&router, EXPRESS_REGION, held).await;
    assert_eq!(
        session.status,
        200,
        "case \"the held bucket after the refusals\": expected HTTP 200, got {} with body {:?}",
        session.status,
        text(&session)
    );
}

/// RULE: the directory-bucket listing reports this account's directory buckets and nothing
/// else, and the general purpose listing reports its general purpose buckets and nothing
/// else.
///
/// The rows differ along the kind of bucket (directory in three different zones, general
/// purpose in two Regions' namespaces), so what is asserted is the PARTITION of the account's
/// buckets between the two listings rather than one name appearing in one of them.
#[tokio::test]
async fn each_listing_reports_its_own_kind_of_bucket() {
    let router = router();
    // (case, name, zone id when it is a directory bucket)
    let rows: [(&str, &str, Option<&str>); 5] = [
        ("a directory bucket in use1-az4", "dirlist-a--use1-az4--x-s3", Some("use1-az4")),
        ("a directory bucket in use1-az5", "dirlist-b--use1-az5--x-s3", Some("use1-az5")),
        ("a directory bucket in usw2-az1", "dirlist-c--usw2-az1--x-s3", Some("usw2-az1")),
        ("a general purpose bucket", "dirlist-general-purpose", None),
        ("another general purpose bucket", "dirlist-plain-bucket", None),
    ];
    for (case, name, zone) in rows {
        match zone {
            Some(zone) => directory_bucket(&router, name, zone).await,
            None => bucket(&router, name).await,
        }
        let _ = case;
    }

    let expected_directory: Vec<String> = rows
        .iter()
        .filter(|(_, _, zone)| zone.is_some())
        .map(|(_, name, _)| name.to_string())
        .collect();
    let expected_general_purpose: Vec<String> = rows
        .iter()
        .filter(|(_, _, zone)| zone.is_none())
        .map(|(_, name, _)| name.to_string())
        .collect();

    // The directory-bucket listing, reached both ways a client names it: by the signing name
    // of the endpoint that serves it, and by the `x-id` discriminator the model's own URI
    // pattern carries (`/?x-id=ListDirectoryBuckets`).
    for (case, query) in [
        ("signed for s3express", ""),
        ("naming the model's x-id", "?x-id=ListDirectoryBuckets"),
    ] {
        let listed = list_directory_buckets(&router, query).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected HTTP 200, got {} with body {:?}",
            listed.status,
            text(&listed)
        );
        assert_eq!(
            listed_names(&listed),
            expected_directory,
            "case {case:?}: ListDirectoryBuckets must report the account's directory buckets \
             in name order, got {:?}",
            listed_names(&listed)
        );
    }

    let general_purpose = send(&router, "GET", "/", &[], "").await;
    assert_eq!(
        general_purpose.status, 200,
        "case \"the general purpose listing\": expected HTTP 200, got {} with body {:?}",
        general_purpose.status,
        text(&general_purpose)
    );
    assert_eq!(
        listed_names(&general_purpose),
        expected_general_purpose,
        "case \"the general purpose listing\": ListBuckets must report the account's general \
         purpose buckets alone, got {:?}",
        listed_names(&general_purpose)
    );

    // A request that merely LOOKS root-path at the routing seam is not this listing: a Zonal
    // `GET /{Bucket}?list-type=2` against a directory bucket this emulator does not hold
    // carries that bucket in its HOST and nothing in its path-style address, and its query
    // names another operation's listing — so it is answered about the address it named and
    // never with the account's directory buckets.
    let absent = send_bytes_signed(
        &router,
        S3_EXPRESS_AUTHORIZATION,
        "GET",
        "dirlist-absent--use1-az4--x-s3.s3express-use1-az4.us-east-1.amazonaws.com",
        "/?list-type=2",
        &[],
        b"",
    )
    .await;
    for name in &expected_directory {
        assert!(
            !listed_names(&absent).contains(name),
            "case \"a zonal listing of a bucket this emulator does not hold\": it must not \
             report the account's directory buckets, got {:?}",
            listed_names(&absent)
        );
    }
}

/// RULE: the directory-bucket listing pages by `MaxDirectoryBuckets` and resumes at the
/// `ContinuationToken` the previous page handed back, so paging through it reports every
/// directory bucket exactly once whatever the page size.
///
/// The rows differ along the page size: each bound the model's `range {min 0, max 1000}`
/// admits, one inside it, and the absent member.
#[tokio::test]
async fn paging_the_directory_bucket_listing_reports_every_bucket_once() {
    let router = router();
    let zones = ["use1-az4", "use1-az5", "usw2-az1", "usw2-az2"];
    let names: Vec<String> = zones
        .iter()
        .enumerate()
        .map(|(index, zone)| format!("dirpage-{index}--{zone}--x-s3"))
        .collect();
    for (name, zone) in names.iter().zip(zones) {
        directory_bucket(&router, name, zone).await;
    }

    // (case, the `max-directory-buckets` the request names, the most buckets a page may then
    // carry). The model's minimum is its own case below: a page of zero carries no bucket, so
    // paging with it never advances.
    let rows: [(&str, Option<i32>, usize); 4] = [
        ("one bucket a page", Some(1), 1),
        ("two buckets a page", Some(2), 2),
        ("the model's maximum", Some(1_000), 4),
        ("no page size at all", None, 4),
    ];
    for (case, page_size, page_limit) in rows {
        let mut collected: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut query: Vec<String> = Vec::new();
            if let Some(size) = page_size {
                query.push(format!("max-directory-buckets={size}"));
            }
            if let Some(resume) = token.as_deref() {
                query.push(format!(
                    "continuation-token={}",
                    super::continuation_token_alphabet::handed_back_in_query(resume)
                ));
            }
            let page = list_directory_buckets(
                &router,
                &match query.is_empty() {
                    true => String::new(),
                    false => format!("?{}", query.join("&")),
                },
            )
            .await;
            assert_eq!(
                page.status, 200,
                "case {case:?}: expected HTTP 200 for page {pages}, got {} with body {:?}",
                page.status,
                text(&page)
            );
            let listed = listed_names(&page);
            assert!(
                listed.len() <= page_limit,
                "case {case:?}: a page may carry at most {page_limit} buckets, got {:?}",
                listed
            );
            collected.extend(listed);
            pages += 1;
            assert!(
                pages <= names.len() + 1,
                "case {case:?}: paging must terminate, got {pages} pages for {} buckets",
                names.len()
            );
            match continuation_token(&page) {
                Some(next) => token = Some(next),
                None => break,
            }
        }
        assert_eq!(
            collected, names,
            "case {case:?}: paging must report every directory bucket once, in name order, \
             got {collected:?}"
        );
    }

    // The model's minimum page size, which the analogous listing of this service answers the
    // same way: a page that carries no bucket, and a token saying there is more.
    let empty = list_directory_buckets(&router, "?max-directory-buckets=0").await;
    assert_eq!(
        (empty.status, listed_names(&empty).len()),
        (200, 0),
        "case \"the model's minimum page size\": expected HTTP 200 with no bucket, got {} and \
         {:?}",
        empty.status,
        listed_names(&empty)
    );
    assert!(
        continuation_token(&empty).is_some(),
        "case \"the model's minimum page size\": a page that carried none of the {} buckets \
         must hand back the cursor they start at, got {:?}",
        names.len(),
        text(&empty)
    );

    // A token this listing did not mint names no cursor into it, and is refused rather than
    // read as a position the caller chose.
    let refused = list_directory_buckets(&router, "?continuation-token=not-a-token").await;
    assert_eq!(
        (refused.status, refused.error_code().as_str()),
        (400, "InvalidArgument"),
        "case \"a continuation token this listing did not mint\": expected 400 \
         InvalidArgument, got {} {:?} with body {:?}",
        refused.status,
        refused.error_code(),
        text(&refused)
    );
}

/// RULE: a listing of the objects in a DIRECTORY bucket may ask only for what that bucket
/// type serves — one delimiter, no start-after, and a prefix that ends in the delimiter —
/// and the same request against a general purpose bucket is served as asked.
///
/// The rows differ along the member asked with and its value: the delimiter S3 Express
/// supports and two it does not, a prefix with and without the delimiter, and `start-after`
/// at all.
#[tokio::test]
async fn a_directory_bucket_listing_serves_only_what_that_bucket_type_supports() {
    let router = router();
    let directory = "dirlisting--use1-az4--x-s3";
    let general_purpose = "dirlisting-general-purpose";
    directory_bucket(&router, directory, "use1-az4").await;
    bucket(&router, general_purpose).await;
    for name in [directory, general_purpose] {
        for key in ["k/part-1", "k/part-2"] {
            let stored = put_object(&router, name, key, b"body", &[]).await;
            assert_eq!(
                stored.status, 200,
                "the object {key:?} must be stored in {name:?} first, got {} with body {:?}",
                stored.status,
                text(&stored)
            );
        }
    }

    // (case, the listing's query, the refusal a DIRECTORY bucket answers it with as
    // `(code, status, message)`, or `None` for a listing it serves)
    type Refusal = Option<(&'static str, u16, &'static str)>;
    let rows: [(&str, &str, Refusal); 7] = [
        ("no member at all", "", None),
        ("the one delimiter directory buckets support", "&delimiter=%2F", None),
        (
            "a prefix that ends in the delimiter",
            "&prefix=k%2F",
            None,
        ),
        (
            "a delimiter directory buckets do not support",
            "&delimiter=-",
            Some((
                "InvalidRequest",
                400,
                "This bucket only supports '/' as a delimiter",
            )),
        ),
        (
            "another delimiter directory buckets do not support",
            "&delimiter=%7C",
            Some((
                "InvalidRequest",
                400,
                "This bucket only supports '/' as a delimiter",
            )),
        ),
        (
            "a prefix that does not end in the delimiter",
            "&prefix=k",
            Some((
                "InvalidRequest",
                400,
                "This bucket does not support a prefix that does not end in a delimiter. \
                 Specify a prefix path ending with a delimiter and try again.",
            )),
        ),
        (
            "a start-after key",
            "&start-after=k%2Fpart-1",
            Some((
                "NotSupported",
                501,
                "This bucket does not support start-after query parameter for ListObjectsV2 \
                 API.",
            )),
        ),
    ];
    for (case, query, refusal) in rows {
        let listed = send(
            &router,
            "GET",
            &format!("/{directory}?list-type=2{query}"),
            &[],
            "",
        )
        .await;
        match refusal {
            None => assert_eq!(
                listed.status, 200,
                "case {case:?}: a directory bucket serves this listing, got {} with body {:?}",
                listed.status,
                text(&listed)
            ),
            Some((code, status, message)) => assert_eq!(
                (
                    listed.status,
                    listed.error_code().as_str(),
                    listed.error_message().as_str()
                ),
                (status, code, message),
                "case {case:?}: expected {status} {code:?} {message:?}, got {} {:?} {:?}",
                listed.status,
                listed.error_code(),
                listed.error_message()
            ),
        }

        // The other half of the rule: a GENERAL PURPOSE bucket serves every one of them, so
        // what is refused is the directory bucket's own constraint and not the member.
        let served = send(
            &router,
            "GET",
            &format!("/{general_purpose}?list-type=2{query}"),
            &[],
            "",
        )
        .await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: a general purpose bucket serves this listing, got {} with body \
             {:?}",
            served.status,
            text(&served)
        );
    }
}
