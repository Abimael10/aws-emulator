// SPDX-License-Identifier: CC0-1.0

//! S3 Express One Zone: creating a DIRECTORY bucket, taking a session on it, and appending
//! to an object in it.
//!
//! Every assertion drives a real HTTP request through the router (the harness is the one the
//! bucket-lifecycle and object suites in this crate already own), so what is pinned is the
//! wire behaviour — status, bound headers, error code, the stored bytes a later read serves.
//!
//! The rules under test, with their sources:
//!
//! * a `CreateBucket` whose `CreateBucketConfiguration$Bucket` names the `Directory` type is
//!   placed in the ONE zone its `Location` names — "for directory buckets, the name of the
//!   location is the Zone ID of the Availability Zone (AZ) or Local Zone (LZ) where the
//!   bucket will be created" (`LocationInfo`) — the bucket's name has to be a directory
//!   bucket's and name that same zone (`<base>--<zone-id>--x-s3`, Amazon S3 User Guide,
//!   *Directory bucket naming rules*), and the create answers the bucket's ARN, which "is
//!   only supported for S3 directory buckets" (`CreateBucketOutput$BucketArn`);
//! * `CreateSession` "creates a session that establishes temporary security credentials to
//!   support fast authentication and authorization for the Zonal endpoint API operations on
//!   directory buckets", whose credentials "are scoped to the bucket and expire after 5
//!   minutes", and whose one modelled error is `NoSuchBucket` (404) for a bucket that is not
//!   there;
//! * `PutObject`'s `x-amz-write-offset-bytes` "specifies the offset for appending data to
//!   existing objects in bytes. The offset must be equal to the size of the existing object
//!   being appended to. If no object exists, setting this header to 0 will create a new
//!   object" (`PutObjectRequest$WriteOffsetBytes`), it "is only supported for objects in the
//!   Amazon S3 Express One Zone storage class in directory buckets", an offset that is not
//!   the current size is the modelled `InvalidWriteOffset` (400, "the write offset value
//!   that you specified does not match the current object size"), and the write reports "the
//!   size of the object in bytes … only present if you append to an object"
//!   (`PutObjectOutput$Size`);
//! * a session is a credential this process keeps ACCEPTING until it expires, so the
//!   sessions it holds at once are bounded and an expired one is released — the documented
//!   5-minute interval is enforced, not only reported;
//! * the name a create accepted is the name the session call resolves, for every zone such a
//!   bucket may be placed in and over both published addresses of the directory-bucket control
//!   plane — so `CreateSession`'s single modelled error, `NoSuchBucket` (404), is reserved for
//!   a name this account holds no bucket under;
//! * a bucket-configuration PUT on a directory bucket is refused for the DOCUMENT it submits —
//!   `CreateBucketConfiguration` is the one bucket-configuration document that endpoint parses
//!   — which is a different class of answer from the 501 naming a feature the bucket does not
//!   have (`crate::service::directory_bucket`);
//! * and the READ of such a subresource is a third class again: the Zonal endpoint serves no
//!   method at that address, so it answers the 405 `MethodNotAllowed` and the fixed sentence
//!   that code publishes.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use service_com_amazonaws_s3_amazons3::service::express_session::SESSION_TOKEN_HEADER;
use service_com_amazonaws_s3_amazons3::state::{
    ComAmazonawsS3Amazons3State, MAX_VENDED_SESSIONS,
};
use service_com_amazonaws_s3_amazons3::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send, send_bytes, send_bytes_signed, send_in_region, Answer};
use super::object_and_tagging_roundtrip::{bucket, get_object, put_object, text};

/// The `CreateBucketConfiguration` a directory-bucket create carries: the zone it is placed
/// in and the kind of bucket it is, in the model's own element order.
fn directory_configuration(
    location_type: Option<&str>,
    zone_id: Option<&str>,
    data_redundancy: Option<&str>,
    bucket_type: Option<&str>,
) -> String {
    let element = |name: &str, value: Option<&str>| match value {
        Some(value) => format!("<{name}>{value}</{name}>"),
        None => String::new(),
    };
    let location = match (location_type, zone_id) {
        (None, None) => String::new(),
        _ => format!(
            "<Location>{}{}</Location>",
            element("Type", location_type),
            element("Name", zone_id)
        ),
    };
    let bucket = match (data_redundancy, bucket_type) {
        (None, None) => String::new(),
        _ => format!(
            "<Bucket>{}{}</Bucket>",
            element("DataRedundancy", data_redundancy),
            element("Type", bucket_type)
        ),
    };
    format!(
        "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         {location}{bucket}</CreateBucketConfiguration>"
    )
}

/// `PUT /<bucket>` — `CreateBucket` carrying a directory-bucket configuration.
async fn create_directory_bucket(
    router: &MultiServiceRouter,
    name: &str,
    configuration: &str,
) -> Answer {
    send(router, "PUT", &format!("/{name}"), &[], configuration).await
}

/// `GET /<bucket>?session` — `CreateSession`.
async fn create_session(router: &MultiServiceRouter, name: &str) -> Answer {
    send(router, "GET", &format!("/{name}?session"), &[], "").await
}

/// The same `CreateSession`, signed for the S3 EXPRESS signing name instead of `s3` — the
/// scope a client that addresses a directory bucket's Zonal endpoint presents.
///
/// Sent with its own `Authorization` header rather than through [`send`], because the
/// credential scope is exactly what this case varies; neither the key id nor the signature
/// is a credential here (this suite's router verifies none, as the other suites' do).
async fn create_session_signed_for_s3_express(
    router: &MultiServiceRouter,
    name: &str,
) -> Answer {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!(
            "https://{name}.s3express-use1-az4.us-east-1.amazonaws.com/{name}?session"
        ))
        .header("host", "s3express-use1-az4.us-east-1.amazonaws.com")
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 \
             Credential=emulator-test-key/20260101/us-east-1/s3express/aws4_request, \
             SignedHeaders=host, Signature=not-verified",
        )
        .body(bytes::Bytes::new())
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// `PUT /<bucket>/<key>` carrying `x-amz-write-offset-bytes` — an APPENDING `PutObject`.
async fn append_object(
    router: &MultiServiceRouter,
    bucket_name: &str,
    key: &str,
    offset: i64,
    chunk: &[u8],
) -> Answer {
    let offset = offset.to_string();
    put_object(
        router,
        bucket_name,
        key,
        chunk,
        &[("x-amz-write-offset-bytes", offset.as_str())],
    )
    .await
}

/// A directory bucket in one zone, created and asserted created, so a suite about what is
/// served ON one does not repeat the create table.
///
/// `pub(crate)`: the conditional-delete suite ([`super::object_conditional_deletes`]) needs
/// one too, because the modification-time and size conditions of a delete are "only
/// supported for directory buckets" (`DeleteObjectRequest$IfMatchSize`).
pub(crate) async fn directory_bucket(router: &MultiServiceRouter, name: &str, zone_id: &str) {
    let created = create_directory_bucket(
        router,
        name,
        &directory_configuration(
            Some("AvailabilityZone"),
            Some(zone_id),
            None,
            Some("Directory"),
        ),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "the directory bucket {name:?} must be created first, got {} with body {:?}",
        created.status,
        text(&created)
    );
}

/// RULE: a create that names the `Directory` bucket type places the bucket in the zone its
/// `Location` names, and the name it asks for has to be a directory bucket's name for that
/// same zone.
///
/// The rows differ along what the rule generalises over: the location type (an Availability
/// Zone and a Local Zone), whether the redundancy is named or implied, and each way the
/// request can fail to describe one bucket in one zone.
#[tokio::test]
async fn a_directory_bucket_is_placed_in_the_zone_its_location_names() {
    let router = router();
    let account = engine::emulator_account_id();

    // (case, bucket name, configuration, status, the ARN the create must report when it
    // succeeds, the error code when it does not).
    let rows: [(&str, &str, String, u16, &str); 8] = [
        (
            "an Availability Zone bucket whose redundancy is implied",
            "appendable--use1-az4--x-s3",
            directory_configuration(
                Some("AvailabilityZone"),
                Some("use1-az4"),
                None,
                Some("Directory"),
            ),
            200,
            "",
        ),
        (
            "an Availability Zone bucket naming its redundancy",
            "explicit--use1-az5--x-s3",
            directory_configuration(
                Some("AvailabilityZone"),
                Some("use1-az5"),
                Some("SingleAvailabilityZone"),
                Some("Directory"),
            ),
            200,
            "",
        ),
        (
            // A Local Zone id carries the zone's own name before the AZ suffix, which the
            // `--`-separated bucket name still reads as one segment.
            "a Local Zone bucket",
            "zonal--usw2-lax1-az1--x-s3",
            directory_configuration(
                Some("LocalZone"),
                Some("usw2-lax1-az1"),
                Some("SingleLocalZone"),
                Some("Directory"),
            ),
            200,
            "",
        ),
        (
            "a name whose zone segment is another zone",
            "mismatched--use1-az4--x-s3",
            directory_configuration(
                Some("AvailabilityZone"),
                Some("use1-az5"),
                None,
                Some("Directory"),
            ),
            400,
            "InvalidBucketName",
        ),
        (
            "a name without the directory-bucket suffix",
            "no-suffix--use1-az4",
            directory_configuration(
                Some("AvailabilityZone"),
                Some("use1-az4"),
                None,
                Some("Directory"),
            ),
            400,
            "InvalidBucketName",
        ),
        (
            "a directory bucket that names no zone",
            "zoneless--use1-az4--x-s3",
            directory_configuration(None, None, None, Some("Directory")),
            400,
            "InvalidRequest",
        ),
        (
            "a zone named for a bucket that is not a directory bucket",
            "general-purpose-with-a-zone",
            directory_configuration(Some("AvailabilityZone"), Some("use1-az4"), None, None),
            400,
            "InvalidRequest",
        ),
        (
            "a redundancy that names the other kind of zone",
            "redundancy--use1-az4--x-s3",
            directory_configuration(
                Some("AvailabilityZone"),
                Some("use1-az4"),
                Some("SingleLocalZone"),
                Some("Directory"),
            ),
            400,
            "InvalidRequest",
        ),
    ];

    for (case, name, configuration, status, code) in rows {
        let created = create_directory_bucket(&router, name, &configuration).await;
        assert_eq!(
            created.status, status,
            "case {case:?}: expected HTTP {status}, got {} with body {:?}",
            created.status,
            text(&created)
        );
        if status != 200 {
            assert_eq!(
                created.error_code(),
                code,
                "case {case:?}: expected the {code} code, got {:?}",
                created.error_code()
            );
            // A refused create leaves no bucket behind, so a session on the name it asked
            // for reports the bucket is not there.
            let session = create_session(&router, name).await;
            assert_eq!(
                session.error_code(),
                "NoSuchBucket",
                "case {case:?}: a refused create must leave no bucket, got {:?} from \
                 CreateSession",
                session.error_code()
            );
            continue;
        }
        // "This parameter is only supported for S3 directory buckets"
        // (`CreateBucketOutput$BucketArn`): the ARN names the `s3express` namespace, the
        // endpoint's Region, this account and the bucket.
        let expected_arn = format!("arn:aws:s3express:us-east-1:{account}:bucket/{name}");
        assert_eq!(
            created.header("x-amz-bucket-arn"),
            Some(expected_arn.clone()),
            "case {case:?}: expected the bucket ARN {expected_arn:?}, got {:?}",
            created.header("x-amz-bucket-arn")
        );
    }

    // A general purpose bucket's create reports an ARN too, in the `s3` namespace and by its
    // globally unique name alone (recorded behaviour, pinned over a table of its own by
    // [`super::bucket_create_list_and_tagging_contracts`]) — so what the KIND of bucket
    // decides is which ARN is reported, not whether one is.
    let general = send(&router, "PUT", "/general-purpose-bucket", &[], "").await;
    assert_eq!(
        general.status, 200,
        "case \"a general purpose bucket\": expected HTTP 200, got {} with body {:?}",
        general.status,
        text(&general)
    );
    assert_eq!(
        general.header("x-amz-bucket-arn").as_deref(),
        Some("arn:aws:s3:::general-purpose-bucket"),
        "case \"a general purpose bucket\": expected the general purpose bucket's own ARN, \
         got {:?}",
        general.header("x-amz-bucket-arn")
    );
}

/// RULE: `CreateSession` vends temporary credentials for a directory bucket that exists,
/// each its own credential, expiring after the documented 5 minutes; it answers
/// `NoSuchBucket` for a bucket that is not there, and refuses a bucket no Zonal endpoint
/// addresses.
#[tokio::test]
async fn a_session_vends_scoped_credentials_for_a_directory_bucket() {
    let router = router();
    let name = "session--use1-az4--x-s3";
    directory_bucket(&router, name, "use1-az4").await;
    bucket(&router, "session-general-purpose").await;

    let mut vended: Vec<String> = Vec::new();
    for attempt in ["the first session", "a second session on the same bucket"] {
        let session = create_session(&router, name).await;
        assert_eq!(
            session.status, 200,
            "case {attempt:?}: expected HTTP 200, got {} with body {:?}",
            session.status,
            text(&session)
        );
        let access_key_id = session.element("AccessKeyId").unwrap_or_default();
        let secret = session.element("SecretAccessKey").unwrap_or_default();
        let token = session.element("SessionToken").unwrap_or_default();
        for (member, value) in [
            ("AccessKeyId", &access_key_id),
            ("SecretAccessKey", &secret),
            ("SessionToken", &token),
        ] {
            assert!(
                !value.is_empty(),
                "case {attempt:?}: the session must carry a {member}, got {value:?} in {:?}",
                text(&session)
            );
        }
        // Each session is its OWN credential: "you must use IAM credentials again to make a
        // CreateSession API request that generates a NEW set of temporary credentials".
        assert!(
            !vended.contains(&access_key_id),
            "case {attempt:?}: every session must vend its own access key id, but \
             {access_key_id:?} was vended before"
        );
        vended.push(access_key_id);

        // "Temporary security credentials … expire after 5 minutes": the instant reported
        // is parsed and compared against the window that interval puts it in, rather than
        // against any literal.
        let expiration = session
            .element("Expiration")
            .unwrap_or_else(|| panic!("case {attempt:?}: the session must carry an Expiration"));
        let parsed = DateTime::from_str(&expiration, Format::DateTime).unwrap_or_else(|error| {
            panic!("case {attempt:?}: the Expiration must be a timestamp ({error}): {expiration}")
        });
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock must be after the epoch")
            .as_secs() as i64;
        let remaining = parsed.secs() - now;
        assert!(
            (4 * 60..=5 * 60).contains(&remaining),
            "case {attempt:?}: the session must expire 5 minutes out, but {expiration} is \
             {remaining} seconds away"
        );
    }

    // The SAME call signed for the S3 Express signing name — the scope a client reaching a
    // directory bucket's Zonal endpoint presents — is this service's too, and is served
    // rather than reaching nobody.
    let zonal = create_session_signed_for_s3_express(&router, name).await;
    assert_eq!(
        zonal.status, 200,
        "case \"a session signed for s3express\": expected HTTP 200, got {} with body {:?}",
        zonal.status,
        text(&zonal)
    );
    assert!(
        zonal.element("AccessKeyId").is_some_and(|id| !id.is_empty()),
        "case \"a session signed for s3express\": expected session credentials, got {:?}",
        text(&zonal)
    );

    // A `?session` request against a bucket that HAS no session subresource is not a refused
    // session at all: S3 ignores a query parameter that names no subresource of the bucket it
    // was addressed to, so what is left is `GET /{Bucket}` — the bucket's own listing.
    let listed = create_session(&router, "session-general-purpose").await;
    assert_eq!(
        listed.status, 200,
        "case \"a general purpose bucket, which has no session subresource\": expected the \
         bucket's listing with HTTP 200, got {} with body {:?}",
        listed.status,
        text(&listed)
    );
    let body = text(&listed);
    for element in ["Name", "Prefix", "Marker", "MaxKeys", "IsTruncated"] {
        assert!(
            body.contains(&format!("<{element}>")),
            "case \"a general purpose bucket, which has no session subresource\": the listing \
             must carry <{element}>, got {body}"
        );
    }
    assert!(
        !body.contains("<Credentials>"),
        "case \"a general purpose bucket, which has no session subresource\": no session may \
         be vended, got {body}"
    );

    let rows: [(&str, &str, u16, &str); 1] = [(
        "a bucket this emulator does not hold",
        "absent--use1-az4--x-s3",
        404,
        "NoSuchBucket",
    )];
    for (case, bucket_name, status, code) in rows {
        let refused = create_session(&router, bucket_name).await;
        assert_eq!(
            refused.status, status,
            "case {case:?}: expected HTTP {status}, got {} with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_code(),
            code,
            "case {case:?}: expected the {code} code, got {:?}",
            refused.error_code()
        );
        assert!(
            !refused.error_message().is_empty(),
            "case {case:?}: the refusal must carry a message, got {:?}",
            text(&refused)
        );
    }
}

/// RULE: an appending write extends the object when its offset IS the object's current size,
/// and is refused `InvalidWriteOffset` when it is not — on a directory bucket only.
///
/// The rows differ along the chunk's size (empty, one byte, a long run) and along whether
/// the key already holds anything, so the rule is exercised over a sequence of appends
/// rather than one.
#[tokio::test]
async fn an_append_extends_the_object_only_at_its_current_size() {
    let router = router();
    let name = "appends--use1-az4--x-s3";
    directory_bucket(&router, name, "use1-az4").await;
    bucket(&router, "appends-general-purpose").await;

    let key = "logs/stream.bin";
    let chunks: [(&str, &[u8]); 4] = [
        ("the first chunk, into a key holding nothing", b"alpha"),
        ("one byte", b"b"),
        ("an empty chunk", b""),
        ("a long run", b"0123456789012345678901234567890123456789"),
    ];
    let mut expected: Vec<u8> = Vec::new();
    for (case, chunk) in chunks {
        let offset = expected.len() as i64;
        let appended = append_object(&router, name, key, offset, chunk).await;
        assert_eq!(
            appended.status, 200,
            "case {case:?}: expected the append at offset {offset} to be accepted, got {} \
             with body {:?}",
            appended.status,
            text(&appended)
        );
        expected.extend_from_slice(chunk);
        // "The size of the object in bytes. This will only be present if you append to an
        // object" (`PutObjectOutput$Size`) — the size the NEXT append's offset has to be.
        assert_eq!(
            appended.header("x-amz-object-size"),
            Some(expected.len().to_string()),
            "case {case:?}: expected the write to report {} bytes, got {:?}",
            expected.len(),
            appended.header("x-amz-object-size")
        );
        let read = get_object(&router, name, key, "", &[]).await;
        assert_eq!(
            read.body.as_ref(),
            expected.as_slice(),
            "case {case:?}: expected the appended object to read back as {} bytes, got {}",
            expected.len(),
            read.body.len()
        );
        // An object in a directory bucket is stored in the S3 Express One Zone class, which
        // is the class an append requires.
        assert_eq!(
            read.header("x-amz-storage-class"),
            Some("EXPRESS_ONEZONE".to_string()),
            "case {case:?}: expected the stored object's storage class, got {:?}",
            read.header("x-amz-storage-class")
        );
    }

    // Every offset that is NOT the object's current size is refused, and the object is left
    // exactly as it was.
    let size = expected.len() as i64;
    let offsets: [(&str, i64); 4] = [
        ("an offset of zero on a key that holds bytes", 0),
        ("an offset below the current size", size - 1),
        ("an offset above the current size", size + 1),
        ("an offset far past the current size", size + 4096),
    ];
    for (case, offset) in offsets {
        let refused = append_object(&router, name, key, offset, b"rejected").await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400, got {} with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_code(),
            "InvalidWriteOffset",
            "case {case:?}: expected the InvalidWriteOffset code, got {:?}",
            refused.error_code()
        );
        let read = get_object(&router, name, key, "", &[]).await;
        assert_eq!(
            read.body.as_ref(),
            expected.as_slice(),
            "case {case:?}: a refused append must store nothing, got {} bytes",
            read.body.len()
        );
    }

    // The same header on a GENERAL PURPOSE bucket names a parameter that is not valid for
    // the request: appending "is only supported for objects in the Amazon S3 Express One
    // Zone storage class in directory buckets".
    for (case, offset) in [("offset zero", 0), ("a non-zero offset", 7)] {
        let refused =
            append_object(&router, "appends-general-purpose", "stream.bin", offset, b"x").await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected a general purpose bucket to refuse an append with HTTP \
             400, got {} with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequest",
            "case {case:?}: expected the InvalidRequest code, got {:?}",
            refused.error_code()
        );
    }

    // A write that carries NO offset still replaces the whole object, whichever kind of
    // bucket it is in.
    let replaced = put_object(&router, name, key, b"replaced", &[]).await;
    assert_eq!(
        replaced.status, 200,
        "case \"a write with no offset\": expected HTTP 200, got {} with body {:?}",
        replaced.status,
        text(&replaced)
    );
    assert_eq!(
        replaced.header("x-amz-object-size"),
        None,
        "case \"a write with no offset\": the size is reported only for an append, got {:?}",
        replaced.header("x-amz-object-size")
    );
    let read = get_object(&router, name, key, "", &[]).await;
    assert_eq!(
        read.body.as_ref(),
        b"replaced",
        "case \"a write with no offset\": expected the object to be replaced, got {} bytes",
        read.body.len()
    );
}

/// One contention row: the case name and the two chunks the racing appends carry.
type RaceRow<'a> = (&'a str, &'a [u8], &'a [u8]);

/// RULE: two appends racing at the SAME offset resolve to exactly one accepted write and one
/// `InvalidWriteOffset`, and the object ends up exactly one chunk longer — the offset is read
/// and the appended bytes written under one guard, so no interleaving can lose a chunk or
/// accept both.
#[tokio::test]
async fn two_appends_at_one_offset_resolve_to_exactly_one_write() {
    let router = router();
    let name = "contention--use1-az4--x-s3";
    directory_bucket(&router, name, "use1-az4").await;

    let key = "contended.bin";
    let base = b"base-content";
    let seeded = append_object(&router, name, key, 0, base).await;
    assert_eq!(
        seeded.status, 200,
        "the seeding append must be accepted, got {} with body {:?}",
        seeded.status,
        text(&seeded)
    );

    let rows: [RaceRow<'_>; 2] = [
        ("equal chunks", b"one-chunk", b"one-chunk"),
        ("chunks of different sizes", b"a", b"bbbbbbbb"),
    ];
    for (case, left, right) in rows {
        let before = get_object(&router, name, key, "", &[]).await.body.len();
        let offset = before as i64;
        let (first, second) = tokio::join!(
            append_object(&router, name, key, offset, left),
            append_object(&router, name, key, offset, right)
        );
        let accepted = [&first, &second]
            .iter()
            .filter(|answer| answer.status == 200)
            .count();
        assert_eq!(
            accepted, 1,
            "case {case:?}: exactly one of two appends at offset {offset} may be accepted, \
             got {} and {}",
            first.status, second.status
        );
        let refused = if first.status == 200 { &second } else { &first };
        assert_eq!(
            refused.status, 400,
            "case {case:?}: the other append must be refused with HTTP 400, got {} with \
             body {:?}",
            refused.status,
            text(refused)
        );
        assert_eq!(
            refused.error_code(),
            "InvalidWriteOffset",
            "case {case:?}: expected the InvalidWriteOffset code, got {:?}",
            refused.error_code()
        );
        // Exactly ONE chunk longer: whichever write won, the other added nothing.
        let winner = if first.status == 200 { left } else { right };
        let after = get_object(&router, name, key, "", &[]).await;
        assert_eq!(
            after.body.len(),
            before + winner.len(),
            "case {case:?}: the object must be exactly one chunk longer, expected {} bytes, \
             got {}",
            before + winner.len(),
            after.body.len()
        );
    }
}

/// RULE: a session is a credential this process keeps accepting until it expires, so the
/// number held at once is bounded and an expired one is RELEASED — the release is what the
/// documented 5-minute interval buys, so a caller that lets its sessions expire is never
/// stopped by the bound.
///
/// The store is seeded directly here, the way the account's bucket-quota suite seeds it, so
/// the bound is exercised without dispatching hundreds of requests; the engine is the SAME
/// one the router hands the service, so what is seeded is what the handler sees.
#[tokio::test]
async fn the_sessions_held_at_once_are_bounded_and_expired_ones_are_released() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));

    let name = "quota--use1-az4--x-s3";
    directory_bucket(&router, name, "use1-az4").await;

    // The release reports exactly the sessions the clock has already expired, and keeps the
    // ones it has not.
    {
        let state = engine.state::<ComAmazonawsS3Amazons3State>();
        let mut store = state.lock();
        let now = SystemTime::now();
        let rows: [(&str, &str, SystemTime, bool); 3] = [
            ("expired a minute ago", "session-expired", now - Duration::from_secs(60), true),
            ("expiring at this instant", "session-due", now, true),
            ("live for another minute", "session-live", now + Duration::from_secs(60), false),
        ];
        for (case, id, expires_at, _) in rows {
            assert!(
                store.record_vended_session(id, expires_at),
                "case {case:?}: an empty store must take the session {id:?}"
            );
        }
        let released = store.release_expired_sessions(now);
        for (case, id, _, expired) in rows {
            assert_eq!(
                released.contains(&id.to_string()),
                expired,
                "case {case:?}: expected released={expired} for {id:?}, got {released:?}"
            );
            assert_eq!(
                store.holds_vended_session(id),
                !expired,
                "case {case:?}: expected the store to hold {id:?} only while it is live"
            );
        }
    }

    // At the bound, a vend is refused rather than growing the store — and the credential it
    // would have returned is not left behind.
    {
        let state = engine.state::<ComAmazonawsS3Amazons3State>();
        let mut store = state.lock();
        let live = SystemTime::now() + Duration::from_secs(300);
        for index in 0..MAX_VENDED_SESSIONS {
            store.record_vended_session(&format!("filler-{index}"), live);
        }
        assert!(
            !store.record_vended_session("one-too-many", live),
            "the store must refuse a session past {MAX_VENDED_SESSIONS}"
        );
    }
    let refused = create_session(&router, name).await;
    assert_eq!(
        refused.status, 400,
        "case \"a vend at the bound\": expected HTTP 400, got {} with body {:?}",
        refused.status,
        text(&refused)
    );
    assert!(
        refused.element("Credentials").is_none(),
        "case \"a vend at the bound\": a refused vend must return no credentials, got {:?}",
        text(&refused)
    );

    // Once those sessions have expired, the next vend releases them and succeeds: the bound
    // is on what is LIVE, which is what makes the documented lifetime enforced rather than
    // reported.
    {
        let state = engine.state::<ComAmazonawsS3Amazons3State>();
        let mut store = state.lock();
        // The sessions the bound test left behind go the way the clock takes them: released
        // once their expiry has passed, which is what asking for the release at a later
        // instant means.
        store.release_expired_sessions(SystemTime::now() + Duration::from_secs(3_600));
        let past = SystemTime::now() - Duration::from_secs(1);
        for index in 0..MAX_VENDED_SESSIONS {
            store.record_vended_session(&format!("stale-{index}"), past);
        }
    }
    let vended = create_session(&router, name).await;
    assert_eq!(
        vended.status, 200,
        "case \"a vend once the held sessions have expired\": expected HTTP 200, got {} with \
         body {:?}",
        vended.status,
        text(&vended)
    );
    assert!(
        vended
            .element("AccessKeyId")
            .is_some_and(|id| !id.is_empty()),
        "case \"a vend once the held sessions have expired\": expected session credentials, \
         got {:?}",
        text(&vended)
    );
}

/// How a client addresses the S3 EXPRESS surface of `region`: the signing name `s3express`,
/// which is what tells that surface from the general purpose one on a single emulator host.
/// Neither the key id nor the signature is a credential here (this suite's router verifies
/// none, as the other suites' do).
fn express_authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{region}/s3express/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// The host the S3 Express control endpoint of `region` is addressed as — "the HTTP Host header
/// syntax is `s3express-control.region.amazonaws.com`" (`CreateBucket`'s own documentation of
/// its directory-bucket surface).
fn express_control_host(region: &str) -> String {
    format!("s3express-control.{region}.amazonaws.com")
}

/// WHICH published address a client sent a directory-bucket control-plane call to. Both serve
/// that plane, and the rule holds for either, so each row of the table below names one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Address {
    /// `s3express-control.<region>.amazonaws.com`, path-style — "you must use path-style
    /// requests in the format `https://s3express-control.region-code.amazonaws.com/bucket-name`"
    /// (`CreateBucket`).
    ExpressControl,
    /// That Region's own S3 endpoint, path-style, signed for `s3`.
    Regional,
}

/// `PUT /<name>` carrying a directory-bucket configuration, sent to `address`.
async fn create_directory_bucket_at(
    router: &MultiServiceRouter,
    region: &str,
    address: Address,
    name: &str,
    configuration: &str,
) -> Answer {
    match address {
        Address::ExpressControl => {
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
        Address::Regional => {
            send_in_region(router, region, "PUT", &format!("/{name}"), &[], configuration).await
        }
    }
}

/// `GET /<name>?session` — `CreateSession`, sent to `address`.
async fn create_session_at(
    router: &MultiServiceRouter,
    region: &str,
    address: Address,
    name: &str,
) -> Answer {
    match address {
        Address::ExpressControl => {
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
        Address::Regional => {
            send_in_region(router, region, "GET", &format!("/{name}?session"), &[], "").await
        }
    }
}

/// The documented interval a session's credentials live for: "temporary security credentials
/// are scoped to the bucket and expire after 5 minutes" (`CreateSession`).
const SESSION_SECONDS: i64 = 5 * 60;

/// The sentence the DIRECTORY bucket surface words `NoSuchBucket` with, spelled out here rather
/// than read from the crate so this suite pins the wording a client branches on and not merely
/// that the handler and the test agree. It states the condition and interpolates nothing of the
/// request.
const NO_SUCH_DIRECTORY_BUCKET_MESSAGE: &str = "The specified bucket does not exist.";

/// One zone a directory bucket may be placed in, and the address its calls reached:
/// `(case, region, location type, zone id, data redundancy, address)`.
type ZoneRow = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Address,
);

/// RULE: a directory bucket created in ANY zone can be sessioned straight away —
/// `CreateSession` on the name the create accepted answers HTTP 200 carrying the whole
/// `Credentials` member, whose token then authorizes the Zonal endpoint calls on that bucket —
/// and `NoSuchBucket` is reserved for a name this account holds no bucket under.
///
/// The rows differ along everything the rule generalises over and nothing it is keyed to: the
/// Region the zone belongs to, the KIND of zone (an Availability Zone and a Local Zone, whose
/// zone ids are shaped differently), WHICH zone of a Region it is, whether the redundancy is
/// the Availability Zone's or the Local Zone's, and which of the two published addresses the
/// create and the session call reached.
#[tokio::test]
async fn a_directory_bucket_in_any_zone_is_sessionable_as_soon_as_it_exists() {
    let router = router();

    let rows: [ZoneRow; 5] = [
        (
            "an Availability Zone of us-west-2, on the Express control endpoint",
            "us-west-2",
            "AvailabilityZone",
            "usw2-az3",
            "SingleAvailabilityZone",
            Address::ExpressControl,
        ),
        (
            "another Availability Zone of the same Region, on the Regional endpoint",
            "us-west-2",
            "AvailabilityZone",
            "usw2-az4",
            "SingleAvailabilityZone",
            Address::Regional,
        ),
        (
            "an Availability Zone of us-east-1",
            "us-east-1",
            "AvailabilityZone",
            "use1-az6",
            "SingleAvailabilityZone",
            Address::ExpressControl,
        ),
        (
            "an Availability Zone of eu-west-1",
            "eu-west-1",
            "AvailabilityZone",
            "euw1-az2",
            "SingleAvailabilityZone",
            Address::Regional,
        ),
        (
            // A Local Zone id carries the zone's own name before the AZ suffix, which the
            // `--`-separated bucket name still reads as one segment.
            "a Local Zone of us-east-1",
            "us-east-1",
            "LocalZone",
            "use1-atl1-az1",
            "SingleLocalZone",
            Address::ExpressControl,
        ),
    ];

    for (case, region, location_type, zone_id, redundancy, address) in rows {
        let name = format!("reachable--{zone_id}--x-s3");
        let created = create_directory_bucket_at(
            &router,
            region,
            address,
            &name,
            &directory_configuration(
                Some(location_type),
                Some(zone_id),
                Some(redundancy),
                Some("Directory"),
            ),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the create of {name:?} must be served, got {} with body {:?}",
            created.status,
            text(&created)
        );

        let session = create_session_at(&router, region, address, &name).await;
        assert_eq!(
            session.status, 200,
            "case {case:?}: a session on the bucket just created must be vended, got {} with \
             body {:?}",
            session.status,
            text(&session)
        );
        let token = session.element("SessionToken").unwrap_or_default();
        for member in [
            "AccessKeyId",
            "SecretAccessKey",
            "SessionToken",
            "Expiration",
        ] {
            let value = session.element(member).unwrap_or_default();
            assert!(
                !value.is_empty(),
                "case {case:?}: the session must carry a {member}, got {:?}",
                text(&session)
            );
        }
        // The interval is the documented one, read off the reported instant rather than
        // compared with any literal: the session is live NOW and expires no later than the
        // published five minutes out.
        let expiration = session.element("Expiration").unwrap_or_default();
        let parsed = DateTime::from_str(&expiration, Format::DateTime).unwrap_or_else(|error| {
            panic!("case {case:?}: the Expiration must be a timestamp ({error}): {expiration}")
        });
        let now = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock must be after the epoch")
            .as_secs() as i64;
        let remaining = parsed.secs() - now;
        assert!(
            (0..=SESSION_SECONDS).contains(&remaining),
            "case {case:?}: the session must be live and expire within the published \
             {SESSION_SECONDS} seconds, but {expiration} is {remaining} seconds away"
        );

        // And what the token is FOR: "you also apply the session token to the
        // x-amz-s3session-token request header for authorization" — so the Zonal data plane of
        // the bucket is reachable under it, which is the whole point of taking a session.
        let key = "reachable/object.txt";
        let body = format!("stored in {zone_id}").into_bytes();
        let written = send_bytes(
            &router,
            "PUT",
            &format!("/{name}/{key}"),
            &[(SESSION_TOKEN_HEADER, token.as_str())],
            &body,
        )
        .await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: a write under the vended session must be served, got {} with body \
             {:?}",
            written.status,
            text(&written)
        );
        let read = send_bytes(
            &router,
            "GET",
            &format!("/{name}/{key}"),
            &[(SESSION_TOKEN_HEADER, token.as_str())],
            b"",
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: a read under the vended session must be served, got {} with body \
             {:?}",
            read.status,
            text(&read)
        );
        assert_eq!(
            read.body.as_ref(),
            body.as_slice(),
            "case {case:?}: the read must serve the bytes written under the session, got {:?}",
            String::from_utf8_lossy(&read.body)
        );

        // The other half of the rule, in the same zone and at the same address: `NoSuchBucket`
        // is what a name this account holds NO bucket under earns, and nothing else is.
        let absent = format!("absent--{zone_id}--x-s3");
        let refused = create_session_at(&router, region, address, &absent).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: a session on a bucket that is not there must be refused 404, got {} \
             with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_code(),
            "NoSuchBucket",
            "case {case:?}: expected the NoSuchBucket code, got {:?}",
            refused.error_code()
        );
        assert_eq!(
            refused.error_message(),
            NO_SUCH_DIRECTORY_BUCKET_MESSAGE,
            "case {case:?}: expected the sentence the directory-bucket surface words that \
             condition with, got {:?}",
            refused.error_message()
        );
    }
}

/// One bucket-configuration PUT: `(case, the subresource and query it is addressed by, the root
/// element of the document it submits, that document)`.
type ConfigurationPutRow = (&'static str, &'static str, &'static str, &'static str);

/// Every bucket-configuration PUT whose document a directory bucket's endpoint does not parse,
/// one row per subresource, each submitting the document the model binds for that call.
///
/// The documents are the smallest well-formed ones of their shape: what is under test is the
/// ROOT element the refusal names, which is the member the model binds as the call's payload.
const CONFIGURATION_PUTS: [ConfigurationPutRow; 13] = [
    (
        "versioning",
        "?versioning",
        "VersioningConfiguration",
        "<VersioningConfiguration><Status>Enabled</Status></VersioningConfiguration>",
    ),
    (
        "event notifications",
        "?notification",
        "NotificationConfiguration",
        "<NotificationConfiguration></NotificationConfiguration>",
    ),
    (
        "replication",
        "?replication",
        "ReplicationConfiguration",
        "<ReplicationConfiguration><Role>arn:aws:iam::000000000000:role/replication</Role>\
         <Rule><Status>Enabled</Status><Prefix></Prefix>\
         <Destination><Bucket>arn:aws:s3:::replica</Bucket></Destination></Rule>\
         </ReplicationConfiguration>",
    ),
    (
        "transfer acceleration",
        "?accelerate",
        "AccelerateConfiguration",
        "<AccelerateConfiguration><Status>Enabled</Status></AccelerateConfiguration>",
    ),
    (
        "cors",
        "?cors",
        "CORSConfiguration",
        "<CORSConfiguration><CORSRule><AllowedMethod>GET</AllowedMethod>\
         <AllowedOrigin>*</AllowedOrigin></CORSRule></CORSConfiguration>",
    ),
    (
        "the bucket tag set",
        "?tagging",
        "Tagging",
        "<Tagging><TagSet><Tag><Key>stage</Key><Value>curated</Value></Tag></TagSet></Tagging>",
    ),
    (
        "static website hosting",
        "?website",
        "WebsiteConfiguration",
        "<WebsiteConfiguration><IndexDocument><Suffix>index.html</Suffix></IndexDocument>\
         </WebsiteConfiguration>",
    ),
    (
        "request payment",
        "?requestPayment",
        "RequestPaymentConfiguration",
        "<RequestPaymentConfiguration><Payer>Requester</Payer></RequestPaymentConfiguration>",
    ),
    (
        "server access logging",
        "?logging",
        "BucketLoggingStatus",
        "<BucketLoggingStatus></BucketLoggingStatus>",
    ),
    (
        "object ownership",
        "?ownershipControls",
        "OwnershipControls",
        "<OwnershipControls><Rule><ObjectOwnership>BucketOwnerEnforced</ObjectOwnership></Rule>\
         </OwnershipControls>",
    ),
    (
        "the public access block",
        "?publicAccessBlock",
        "PublicAccessBlockConfiguration",
        "<PublicAccessBlockConfiguration><BlockPublicAcls>true</BlockPublicAcls>\
         </PublicAccessBlockConfiguration>",
    ),
    (
        "an analytics configuration",
        "?analytics&id=refused",
        "AnalyticsConfiguration",
        "<AnalyticsConfiguration><Id>refused</Id>\
         <StorageClassAnalysis></StorageClassAnalysis></AnalyticsConfiguration>",
    ),
    (
        "an S3 Intelligent-Tiering configuration",
        "?intelligent-tiering&id=refused",
        "IntelligentTieringConfiguration",
        "<IntelligentTieringConfiguration><Id>refused</Id><Status>Enabled</Status>\
         <Tiering><Days>90</Days><AccessTier>ARCHIVE_ACCESS</AccessTier></Tiering>\
         </IntelligentTieringConfiguration>",
    ),
];

/// One call refused for a FEATURE a directory bucket does not have rather than for a document
/// it submitted: `(case, method, the subresource under the bucket, the headers, the feature the
/// refusal names)`.
type FeatureRefusalRow = (
    &'static str,
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static str,
);

/// RULE: a bucket-configuration PUT on a DIRECTORY bucket is refused for the DOCUMENT it
/// submits — HTTP 400 `InvalidArgument`, naming that document's root element beside
/// `CreateBucketConfiguration`, the one bucket-configuration document a directory bucket's
/// endpoint parses — while a call that submits no such document keeps the 501 naming the
/// feature that kind of bucket does not have, and the same PUT on a general purpose bucket is
/// served.
///
/// The rows span every subresource the rule covers (and so every configuration member name the
/// message interpolates), a document whose root is NOT the member the model binds for the call,
/// and three refusals of the feature class — a canned-ACL write that submits no document, an
/// object-level tag write, and a configuration PUT carrying no document at all — so what is
/// pinned is which refusal each class of call earns rather than one recorded call's answer.
#[tokio::test]
async fn a_bucket_configuration_put_on_a_directory_bucket_names_the_document_it_submitted() {
    let router = router();
    let directory = "configrefusal--use1-az4--x-s3";
    directory_bucket(&router, directory, "use1-az4").await;
    bucket(&router, "configrefusal-general-purpose").await;

    for (case, subresource, element, document) in CONFIGURATION_PUTS {
        let refused = send(
            &router,
            "PUT",
            &format!("/{directory}{subresource}"),
            &[],
            document,
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400 for a document this endpoint does not parse, got \
             {} with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_code(),
            "InvalidArgument",
            "case {case:?}: expected the InvalidArgument code, got {:?}",
            refused.error_code()
        );
        assert_eq!(
            refused.error_message(),
            format!("{element} is not valid, expected CreateBucketConfiguration"),
            "case {case:?}: the refusal must name the submitted document, got {:?}",
            refused.error_message()
        );
    }

    // The variable part comes from the REQUEST: a document whose root is not the member the
    // model binds for the call is still the document the caller sent, and the refusal names it.
    let other_document = send(
        &router,
        "PUT",
        &format!("/{directory}?versioning"),
        &[],
        "<SomeOtherConfiguration><Status>Enabled</Status></SomeOtherConfiguration>",
    )
    .await;
    assert_eq!(
        other_document.error_message(),
        "SomeOtherConfiguration is not valid, expected CreateBucketConfiguration",
        "case \"a document of another shape\": the refusal must name the element the request \
         carried, got {:?}",
        other_document.error_message()
    );

    // A call that submits no such document is refused for what the bucket does not HAVE, which
    // is a different class of answer: the published 501 naming that feature. The last row is a
    // configuration PUT whose body carries no document at all, which therefore submits none for
    // the refusal above to name. A READ of one of these subresources is a third class again —
    // the method refusal, pinned by
    // [`a_read_of_a_subresource_a_directory_bucket_does_not_serve_is_refused_for_the_method`].
    // (case, method, the subresource under the bucket, the headers, the feature named)
    let feature_rows: [FeatureRefusalRow; 3] = [
        (
            "an ACL named in headers, which submits no document",
            "PUT",
            "?acl",
            &[("x-amz-acl", "private")],
            "ACL",
        ),
        (
            "an object's tag set, which is not a bucket configuration",
            "PUT",
            "/tagged.txt?tagging",
            &[],
            "Object Tags",
        ),
        (
            "a configuration PUT carrying no document at all",
            "PUT",
            "?notification",
            &[],
            "Notifications",
        ),
    ];
    for (case, method, subresource, headers, feature) in feature_rows {
        let body = match subresource.ends_with("/tagged.txt?tagging") {
            true => "<Tagging><TagSet><Tag><Key>stage</Key><Value>curated</Value></Tag></TagSet>\
                     </Tagging>",
            false => "",
        };
        let refused = send(
            &router,
            method,
            &format!("/{directory}{subresource}"),
            headers,
            body,
        )
        .await;
        assert_eq!(
            refused.status, 501,
            "case {case:?}: expected the published 501 for a feature this bucket kind does not \
             have, got {} with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_code(),
            "NotImplemented",
            "case {case:?}: expected the NotImplemented code, got {:?}",
            refused.error_code()
        );
        assert_eq!(
            refused.error_message(),
            format!("This bucket does not support {feature}."),
            "case {case:?}: the refusal must name the feature the bucket does not have, got {:?}",
            refused.error_message()
        );
    }

    // And the other half of the rule: the same documents are CONFIGURATION on a general purpose
    // bucket, so the refusal belongs to the bucket's kind and not to the document.
    for case in [
        "versioning",
        "event notifications",
        "transfer acceleration",
        "cors",
        "the bucket tag set",
        "the public access block",
    ] {
        let (_, subresource, _, document) = CONFIGURATION_PUTS
            .into_iter()
            .find(|(named, _, _, _)| *named == case)
            .unwrap_or_else(|| panic!("case {case:?} must be one of the configuration rows"));
        let served = send(
            &router,
            "PUT",
            &format!("/configrefusal-general-purpose{subresource}"),
            &[],
            document,
        )
        .await;
        assert!(
            (200..300).contains(&served.status),
            "case {case:?}: a general purpose bucket takes this configuration, got {} with body \
             {:?}",
            served.status,
            text(&served)
        );
    }
}

/// One READ of a subresource a directory bucket does not serve: `(case, the subresource query
/// under the bucket)`.
///
/// The queries are the ones the model binds the catalogued reads at
/// (`smithy_query(query="operation",operation="GetBucketAccelerateConfiguration,…")` ->
/// `GET /{Bucket}?accelerate` and its siblings); every catalogued read of a bucket subresource
/// is here, including the two whose subresource is shared between a read of one configuration
/// (`&id=`) and the listing of them all, and the version listing.
const SUBRESOURCE_READS: [(&str, &str); 20] = [
    ("transfer acceleration", "?accelerate"),
    ("the versioning state", "?versioning"),
    ("the bucket ACL", "?acl"),
    ("the CORS configuration", "?cors"),
    ("the bucket tag set", "?tagging"),
    ("static website hosting", "?website"),
    ("server access logging", "?logging"),
    ("event notifications", "?notification"),
    ("request payment", "?requestPayment"),
    ("the replication configuration", "?replication"),
    ("the bucket location", "?location"),
    ("the bucket policy status", "?policyStatus"),
    ("the Object Lock configuration", "?object-lock"),
    ("the public access block", "?publicAccessBlock"),
    ("object ownership", "?ownershipControls"),
    ("one analytics configuration", "?analytics&id=absent"),
    ("the analytics configurations of the bucket", "?analytics"),
    (
        "one S3 Intelligent-Tiering configuration",
        "?intelligent-tiering&id=absent",
    ),
    (
        "the S3 Intelligent-Tiering configurations of the bucket",
        "?intelligent-tiering",
    ),
    ("the version listing", "?versions"),
];

/// RULE: a READ of a subresource a DIRECTORY bucket does not serve is refused for the METHOD —
/// HTTP 405 `MethodNotAllowed` with the fixed sentence that code publishes — because the Zonal
/// endpoint serves no method at that address, while a call it does route (the bucket's own
/// listing, an object's subresource) keeps the 501 naming the feature the bucket lacks, the
/// write of the same subresource keeps the 400 naming the document it submitted, and the same
/// reads on a general purpose bucket are served.
///
/// The rows span the whole class the rule covers — every catalogued read of a bucket
/// subresource, so every subresource query the model binds one at, the two that are shared with
/// a listing of the same configurations included — rather than the one recorded call; the rows
/// below them are the neighbouring classes, which the rule must leave where they are.
#[tokio::test]
async fn a_read_of_a_subresource_a_directory_bucket_does_not_serve_is_refused_for_the_method() {
    let router = router();
    let directory = "subresourceread--use1-az4--x-s3";
    directory_bucket(&router, directory, "use1-az4").await;
    bucket(&router, "subresourceread-general-purpose").await;

    for (case, subresource) in SUBRESOURCE_READS {
        let refused = send(
            &router,
            "GET",
            &format!("/{directory}{subresource}"),
            &[],
            "",
        )
        .await;
        assert_eq!(
            refused.status, 405,
            "case {case:?}: expected HTTP 405 for a method this endpoint does not serve, got {} \
             with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_code(),
            "MethodNotAllowed",
            "case {case:?}: expected the MethodNotAllowed code, got {:?}",
            refused.error_code()
        );
        assert_eq!(
            refused.error_message(),
            "The specified method is not allowed against this resource.",
            "case {case:?}: the sentence that code publishes is fixed text, got {:?}",
            refused.error_message()
        );
    }

    // The neighbouring classes, which this rule does not move: a call the Zonal endpoint does
    // route is refused for the FEATURE the bucket does not have (the v1 listing of the bucket
    // itself, and an OBJECT's subresource — recorded verbatim for the object tag-set plane),
    // and the WRITE of one of the subresources above is refused for the document it submitted.
    // (case, method, the path under the bucket, the body, status, code, message)
    let neighbours: [(&str, &str, &str, &str, u16, &str, &str); 3] = [
        (
            "the v1 listing of the bucket itself, which addresses no subresource",
            "GET",
            "",
            "",
            501,
            "NotImplemented",
            "This bucket does not support ListObjects.",
        ),
        (
            "an object's tag set, which is not a subresource of the bucket",
            "GET",
            "/tagged.txt?tagging",
            "",
            501,
            "NotImplemented",
            "This bucket does not support Object Tags.",
        ),
        (
            "the write of the same subresource, which submits a document",
            "PUT",
            "?accelerate",
            "<AccelerateConfiguration><Status>Enabled</Status></AccelerateConfiguration>",
            400,
            "InvalidArgument",
            "AccelerateConfiguration is not valid, expected CreateBucketConfiguration",
        ),
    ];
    for (case, method, path, body, status, code, message) in neighbours {
        let refused = send(&router, method, &format!("/{directory}{path}"), &[], body).await;
        assert_eq!(
            refused.status, status,
            "case {case:?}: expected HTTP {status}, got {} with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_code(),
            code,
            "case {case:?}: expected the {code} code, got {:?}",
            refused.error_code()
        );
        assert_eq!(
            refused.error_message(),
            message,
            "case {case:?}: expected {message:?}, got {:?}",
            refused.error_message()
        );
    }

    // And the other half of the rule: the same reads are SERVED on a general purpose bucket, so
    // the refusal belongs to the bucket's kind and not to the subresource — including the
    // round trip of the recorded case, whose configuration is written and read back.
    let general_purpose = "subresourceread-general-purpose";
    let configured = send(
        &router,
        "PUT",
        &format!("/{general_purpose}?accelerate"),
        &[],
        "<AccelerateConfiguration><Status>Enabled</Status></AccelerateConfiguration>",
    )
    .await;
    assert!(
        (200..300).contains(&configured.status),
        "case \"transfer acceleration on a general purpose bucket\": the configuration must be \
         taken, got {} with body {:?}",
        configured.status,
        text(&configured)
    );
    for (case, subresource, expected) in [
        (
            "transfer acceleration",
            "?accelerate",
            Some("<Status>Enabled</Status>"),
        ),
        ("the versioning state", "?versioning", None),
        ("the bucket location", "?location", None),
    ] {
        let served = send(
            &router,
            "GET",
            &format!("/{general_purpose}{subresource}"),
            &[],
            "",
        )
        .await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: a general purpose bucket serves this read, got {} with body {:?}",
            served.status,
            text(&served)
        );
        if let Some(expected) = expected {
            let body = text(&served);
            assert!(
                body.contains(expected),
                "case {case:?}: the read must report the configuration that was written \
                 ({expected}), got {body:?}"
            );
        }
    }
}
