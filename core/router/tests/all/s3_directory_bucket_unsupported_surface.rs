// SPDX-License-Identifier: CC0-1.0

//! The NARROWER API surface an S3 Express One Zone (directory) bucket serves, and the entity
//! tag an object a multipart upload assembled in one carries.
//!
//! RULE: the Smithy model states, operation by operation, that an operation "is not supported
//! for directory buckets"
//! (`service_com_amazonaws_s3_amazons3::service::directory_bucket::UNSUPPORTED_NOTE`), so a
//! request that addresses a directory bucket and names such an operation is REFUSED rather
//! than served, and the answer states WHAT refused it: a call asking for a FEATURE its KIND
//! does not have earns the 501 `NotImplemented` and a sentence naming that feature ("This
//! bucket does not support Object Tags." for the object tag-set plane), a PUT that
//! SUBMITS a bucket-configuration document earns the request-shape refusal for that document
//! (400 `InvalidArgument`, "<Element> is not valid, expected CreateBucketConfiguration", that
//! being the one bucket-configuration document such a bucket's endpoint parses), and a READ of
//! a subresource of the bucket earns the method refusal (405 `MethodNotAllowed`, "The specified
//! method is not allowed against this resource."), that endpoint serving no method at such an
//! address at all. Two further
//! members of the same rule are pinned here: a `CopyObject` into a directory bucket may write
//! no tags at all ("only the empty tag-set is supported … any requests that attempt to write
//! non-empty tags into directory buckets will receive a `501 Not Implemented` status code",
//! `CopyObjectRequest$TaggingDirective`), and such a bucket's completed multipart entity tag is
//! NOT the MD5 composite a general purpose bucket's is ("the entity tag may or may not be an
//! MD5 digest of the object data. If the entity tag is not an MD5 digest of the object data, it
//! will … consist of less than 32 or more than 32 hexadecimal digits",
//! `CompleteMultipartUploadOutput$ETag`).
//!
//! And the SESSION half: an S3 Express session token says WHICH session a Zonal request acts
//! under rather than being the credential it is signed with, so a session the clock has
//! expired authorizes nothing on the bucket and the request is refused 403 `AccessDenied`.
//!
//! Every row is driven as a real HTTP request against the router, so what is pinned is the wire
//! answer — the status, the error code and the message. Each assertion is read against the
//! GENERAL PURPOSE twin of the same call, so what is shown is the bucket KIND deciding it and
//! not the call being unservable.

use std::sync::Arc;
use std::time::SystemTime;

use bytes::Bytes;
use service_com_amazonaws_s3_amazons3::state::{
    ComAmazonawsS3Amazons3State, MIN_MULTIPART_PART_BYTES, SESSION_TOKEN_HEADER,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for, and the zone the directory buckets are placed in.
const REGION: &str = "us-east-1";
const ZONE: &str = "use1-az4";

/// The XML namespace a client puts on an Amazon S3 request payload.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The code S3 answers a request whose address implies functionality it does not implement for
/// the resource addressed, and the status it carries.
const NOT_IMPLEMENTED: &str = "NotImplemented";
const NOT_IMPLEMENTED_STATUS: u16 = 501;

/// The refusal an S3 Express session that authorizes nothing on the addressed bucket earns.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u16 = 403;
const ACCESS_DENIED_MESSAGE: &str = "Access Denied";

/// What one call answered.
struct Answer {
    status: u16,
    headers: ::http::HeaderMap,
    body: String,
}

impl Answer {
    /// The text of the first `<name>…</name>` of the body, or the empty string.
    ///
    /// XML character references are resolved, so what an assertion compares is the text the
    /// element CARRIES rather than the document's escaping of it — an SDK's XML reader
    /// resolves them too.
    fn element(&self, name: &str) -> String {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        let Some(start) = self.body.find(&open) else {
            return String::new();
        };
        let rest = &self.body[start + open.len()..];
        match rest.find(&close) {
            Some(end) => unescaped(&rest[..end]),
            None => String::new(),
        }
    }

    /// The `<Code>` of an error document, or the empty string for an answer that is not one.
    fn code(&self) -> String {
        self.element("Code")
    }

    /// The `<Message>` of an error document.
    fn message(&self) -> String {
        self.element("Message")
    }

    /// One response header's value.
    fn header(&self, name: &str) -> String {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }
}

/// `text` with the five XML character references resolved: an element's text content carries
/// them, and what every assertion here is about is the text itself.
fn unescaped(text: &str) -> String {
    text.replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The emulator for this suite: S3 mounted on one engine exactly as the router mounts it, with
/// request authentication off so each call carries a credential scope (which names the Region
/// and the service) without a computed signature. The engine is handed back as well, because
/// one row records a session the CLOCK has already expired, which no sequence of requests can
/// reach — the documented session lifetime is five minutes.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// One call of `method` at the path-style `path`, signed for S3 in [`REGION`].
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/{REGION}/s3/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        );
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body: String::from_utf8_lossy(&body).to_string(),
    }
}

/// The `CreateBucketConfiguration` a directory-bucket create carries: the zone it is placed in
/// and the kind of bucket it is.
fn directory_configuration() -> String {
    format!(
        "<CreateBucketConfiguration xmlns=\"{XMLNS}\">\
         <Location><Type>AvailabilityZone</Type><Name>{ZONE}</Name></Location>\
         <Bucket><DataRedundancy>SingleAvailabilityZone</DataRedundancy><Type>Directory</Type>\
         </Bucket></CreateBucketConfiguration>"
    )
}

/// Create the directory bucket `name` — whose own name carries the zone and the `--x-s3`
/// suffix — and assert it came into being.
async fn directory_bucket(router: &MultiServiceRouter, name: &str) {
    let created = call(
        router,
        "PUT",
        &format!("/{name}"),
        &[("content-type", "application/xml")],
        directory_configuration().as_bytes(),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "the directory bucket {name:?} must be created, got {} with {:?}",
        created.status, created.body
    );
}

/// Create the general purpose bucket `name` and assert it came into being.
async fn general_purpose_bucket(router: &MultiServiceRouter, name: &str) {
    let created = call(router, "PUT", &format!("/{name}"), &[], b"").await;
    assert_eq!(
        created.status, 200,
        "the bucket {name:?} must be created, got {} with {:?}",
        created.status, created.body
    );
}

/// WHICH refusal a call of this surface earns, which is not one answer for all of them.
///
/// A call that asks for a FEATURE the bucket's kind does not have is answered 501
/// `NotImplemented` with the sentence naming that feature. A READ of a subresource of the
/// bucket is answered for the METHOD — HTTP 405 `MethodNotAllowed` with the fixed sentence that
/// code publishes — because the Zonal endpoint serves no method at that address (recorded
/// against this surface for the `?accelerate` read of a directory bucket). A PUT that SUBMITS a
/// bucket-configuration document is answered for the DOCUMENT instead — HTTP 400
/// `InvalidArgument` naming the root element the request carried beside
/// `CreateBucketConfiguration`, the one bucket-configuration document a directory bucket's
/// endpoint parses — which is the answer recorded against that very surface (the behavioural
/// check `s3.directory-bucket-rejects-object-tagging-versioning-notification-and-replication`
/// records `('InvalidArgument', 'VersioningConfiguration is not valid, expected
/// CreateBucketConfiguration')` for `PutBucketVersioning` on a directory bucket). The model's
/// note says such an operation is not supported; WHICH refusal AWS answers it with is recorded
/// behaviour, and the two classes differ because a caller branches on them differently: a
/// document it sent is a terminal caller error, an unimplemented feature is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The feature the bucket's kind does not have.
    Feature(&'static str),
    /// The root element of the configuration document the request submitted.
    Document(&'static str),
    /// The method the endpoint serves none of at the subresource the request addressed.
    Method,
}

/// The code and status a submitted document the endpoint does not parse is refused with.
const INVALID_ARGUMENT: &str = "InvalidArgument";
const INVALID_ARGUMENT_STATUS: u16 = 400;

/// The code, status and fixed sentence a method the addressed resource does not serve is
/// refused with.
const METHOD_NOT_ALLOWED: &str = "MethodNotAllowed";
const METHOD_NOT_ALLOWED_STATUS: u16 = 405;
const METHOD_NOT_ALLOWED_MESSAGE: &str =
    "The specified method is not allowed against this resource.";

impl Refusal {
    /// The status, the code and the whole sentence this refusal is answered with.
    fn answer(&self) -> (u16, &'static str, String) {
        match self {
            Self::Feature(feature) => (
                NOT_IMPLEMENTED_STATUS,
                NOT_IMPLEMENTED,
                format!("This bucket does not support {feature}."),
            ),
            Self::Document(element) => (
                INVALID_ARGUMENT_STATUS,
                INVALID_ARGUMENT,
                format!("{element} is not valid, expected CreateBucketConfiguration"),
            ),
            Self::Method => (
                METHOD_NOT_ALLOWED_STATUS,
                METHOD_NOT_ALLOWED,
                METHOD_NOT_ALLOWED_MESSAGE.to_string(),
            ),
        }
    }
}

/// One call of the surface, as `(case, method, the path and query under the bucket, the body,
/// the refusal it earns)`.
type Call = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Refusal,
);

/// The calls this suite drives against both kinds of bucket.
///
/// The rows span what the rule generalises over: a WRITE of a bucket configuration and a READ
/// of one, a write and a read of an OBJECT subresource, and a LISTING — across the tagging,
/// versioning, notification, replication, ACL, location, request-payment, logging and
/// public-access planes, so the refusal is pinned for the surface the model marks rather than
/// for the four operations the report cited. Beside each is the refusal it earns ([`Refusal`]):
/// a call asking for one plane of an OBJECT, and a call addressed at the bucket itself, name the
/// plane the bucket does not have; a write that submits a configuration DOCUMENT is answered for
/// that document; and a read of a SUBRESOURCE of the bucket is answered for the method, there
/// being none at that address.
const UNSUPPORTED_CALLS: [Call; 17] = [
    (
        "PutObjectTagging",
        "PUT",
        "/held.txt?tagging",
        "<Tagging><TagSet><Tag><Key>team</Key><Value>storage</Value></Tag></TagSet></Tagging>",
        Refusal::Feature("Object Tags"),
    ),
    (
        "GetObjectTagging",
        "GET",
        "/held.txt?tagging",
        "",
        Refusal::Feature("Object Tags"),
    ),
    (
        "PutBucketVersioning",
        "PUT",
        "?versioning",
        "<VersioningConfiguration><Status>Enabled</Status></VersioningConfiguration>",
        Refusal::Document("VersioningConfiguration"),
    ),
    ("GetBucketVersioning", "GET", "?versioning", "", Refusal::Method),
    (
        "PutBucketNotificationConfiguration",
        "PUT",
        "?notification",
        "<NotificationConfiguration></NotificationConfiguration>",
        Refusal::Document("NotificationConfiguration"),
    ),
    (
        "GetBucketNotificationConfiguration",
        "GET",
        "?notification",
        "",
        Refusal::Method,
    ),
    (
        "PutBucketReplication",
        "PUT",
        "?replication",
        "<ReplicationConfiguration>\
         <Role>arn:aws:iam::000000000000:role/s3-replication-role</Role>\
         <Rule><Priority>1</Priority><Filter></Filter><Status>Enabled</Status>\
         <Destination><Bucket>arn:aws:s3:::somewhere</Bucket></Destination></Rule>\
         </ReplicationConfiguration>",
        Refusal::Document("ReplicationConfiguration"),
    ),
    (
        "GetBucketReplication",
        "GET",
        "?replication",
        "",
        Refusal::Method,
    ),
    (
        "PutBucketTagging",
        "PUT",
        "?tagging",
        "<Tagging><TagSet><Tag><Key>team</Key><Value>storage</Value></Tag></TagSet></Tagging>",
        Refusal::Document("Tagging"),
    ),
    ("GetBucketTagging", "GET", "?tagging", "", Refusal::Method),
    ("GetBucketAcl", "GET", "?acl", "", Refusal::Method),
    ("GetBucketLocation", "GET", "?location", "", Refusal::Method),
    ("GetBucketRequestPayment", "GET", "?requestPayment", "", Refusal::Method),
    ("GetBucketLogging", "GET", "?logging", "", Refusal::Method),
    ("GetPublicAccessBlock", "GET", "?publicAccessBlock", "", Refusal::Method),
    (
        "GetBucketAccelerateConfiguration",
        "GET",
        "?accelerate",
        "",
        Refusal::Method,
    ),
    (
        "ListObjects",
        "GET",
        "",
        "",
        Refusal::Feature("ListObjects"),
    ),
];

/// RULE: a request addressing a DIRECTORY bucket and naming an operation the model says such a
/// bucket does not serve is REFUSED — for the FEATURE that bucket's kind does not have (501
/// `NotImplemented`), or, where the call submits a bucket-configuration document, for that
/// DOCUMENT (400 `InvalidArgument`, naming it beside `CreateBucketConfiguration`), or, where it
/// READS a subresource of the bucket, for the METHOD that endpoint serves none of there (405
/// `MethodNotAllowed`) — while the SAME call against a general purpose bucket is not refused
/// that way at all.
#[tokio::test]
async fn an_operation_a_directory_bucket_does_not_serve_is_refused() {
    let (router, _engine) = emulator();
    let directory = format!("xsvc-unsupported--{ZONE}--x-s3");
    let general = "xsvc-unsupported-twin";
    directory_bucket(&router, &directory).await;
    general_purpose_bucket(&router, general).await;

    // An object in each, so a row addressing an object subresource addresses something that is
    // there — which keeps "the key is absent" out of the refusals below.
    for bucket in [directory.as_str(), general] {
        let written = call(
            &router,
            "PUT",
            &format!("/{bucket}/held.txt"),
            &[],
            b"held bytes",
        )
        .await;
        assert_eq!(
            written.status, 200,
            "the object in {bucket:?} must be stored first, got {} with {:?}",
            written.status, written.body
        );
    }

    for (case, method, path, body, refusal) in UNSUPPORTED_CALLS {
        let (status, code, message) = refusal.answer();
        let refused = call(
            &router,
            method,
            &format!("/{directory}{path}"),
            &[("content-type", "application/xml")],
            body.as_bytes(),
        )
        .await;
        assert_eq!(
            (refused.status, refused.code().as_str()),
            (status, code),
            "case {case:?}: a directory bucket does not serve this call, expected {status} \
             {code:?}, got {} {:?} with {:?}",
            refused.status,
            refused.code(),
            refused.body
        );
        assert_eq!(
            refused.message(),
            message,
            "case {case:?}: the refusal must state what refused it, got {:?}",
            refused.message()
        );

        // The same call against the general purpose twin: whatever it answers, it is not this
        // refusal — which is what shows the bucket KIND deciding it.
        let served = call(
            &router,
            method,
            &format!("/{general}{path}"),
            &[("content-type", "application/xml")],
            body.as_bytes(),
        )
        .await;
        assert_ne!(
            served.message(),
            message,
            "case {case:?}: a general purpose bucket serves this operation, got {} with {:?}",
            served.status,
            served.body
        );
    }

    // And the directory bucket still serves what it does support, so the refusals above are
    // the catalogued operations and not the bucket: the object data plane reads back.
    let read = call(&router, "GET", &format!("/{directory}/held.txt"), &[], b"").await;
    assert_eq!(
        (read.status, read.body.as_str()),
        (200, "held bytes"),
        "case \"a supported operation\": the directory bucket's own data plane must still be \
         served, got {} with {:?}",
        read.status,
        read.body
    );
    let listed = call(
        &router,
        "GET",
        &format!("/{directory}?list-type=2"),
        &[],
        b"",
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case \"the listing a directory bucket serves\": expected 200, got {} with {:?}",
        listed.status, listed.body
    );
}

/// RULE: a `CopyObject` whose destination is a directory bucket may write no tags — the tag set
/// the copy WOULD write is what decides it, whichever member asked for it — and the empty cases
/// the same member lists as allowed are served. The sentence names WHICH restriction fired: the
/// carrying over of the SOURCE's tag set for a copy that would copy one (recorded against this
/// surface as "This bucket does not support copying source objects with Object Tags."), and the
/// object tag-set plane for a request that states a set of its own.
///
/// The rows span what the rule generalises over: which member asks (the directive stated as
/// `COPY`, no directive at all, and a `REPLACE` with a non-empty `x-amz-tagging`), and the
/// empty-tag-set forms (`REPLACE` with no tagging header, and a `COPY` of an untagged source).
#[tokio::test]
async fn a_copy_into_a_directory_bucket_may_write_no_tags() {
    let (router, _engine) = emulator();
    let directory = format!("xsvc-copytags--{ZONE}--x-s3");
    let source = "xsvc-copytags-source";
    directory_bucket(&router, &directory).await;
    general_purpose_bucket(&router, source).await;

    // Two source objects: one carrying a non-empty tag set, one carrying none.
    for (key, tagging) in [("tagged.txt", "team=storage"), ("untagged.txt", "")] {
        let headers: Vec<(&str, &str)> = match tagging.is_empty() {
            true => Vec::new(),
            false => vec![("x-amz-tagging", tagging)],
        };
        let written = call(
            &router,
            "PUT",
            &format!("/{source}/{key}"),
            &headers,
            b"source bytes",
        )
        .await;
        assert_eq!(
            written.status, 200,
            "the source object {key:?} must be stored, got {} with {:?}",
            written.status, written.body
        );
    }

    // (case, source key, the headers the copy carries beyond the source, the sentence it is
    // refused with — `None` for a copy that is served)
    let rows: [CopyRow; 5] = [
        (
            "COPY of a tagged source",
            "tagged.txt",
            &[("x-amz-tagging-directive", "COPY")],
            Some(COPYING_SOURCE_TAGS),
        ),
        (
            "no directive over a tagged source",
            "tagged.txt",
            &[],
            Some(COPYING_SOURCE_TAGS),
        ),
        (
            "REPLACE with a non-empty tag set",
            "untagged.txt",
            &[
                ("x-amz-tagging-directive", "REPLACE"),
                ("x-amz-tagging", "team=storage"),
            ],
            Some(OBJECT_TAGS),
        ),
        (
            "REPLACE naming no tag set",
            "tagged.txt",
            &[("x-amz-tagging-directive", "REPLACE")],
            None,
        ),
        (
            "COPY of an untagged source",
            "untagged.txt",
            &[("x-amz-tagging-directive", "COPY")],
            None,
        ),
    ];

    for (index, (case, key, extra, refused)) in rows.into_iter().enumerate() {
        let destination_key = format!("copied-{index}.txt");
        let mut headers: Vec<(&str, &str)> = vec![];
        let copy_source = format!("/{source}/{key}");
        headers.push(("x-amz-copy-source", copy_source.as_str()));
        headers.extend_from_slice(extra);
        let copied = call(
            &router,
            "PUT",
            &format!("/{directory}/{destination_key}"),
            &headers,
            b"",
        )
        .await;
        if let Some(message) = refused {
            assert_eq!(
                (copied.status, copied.code().as_str()),
                (NOT_IMPLEMENTED_STATUS, NOT_IMPLEMENTED),
                "case {case:?}: a copy into a directory bucket may write no tags, expected \
                 {NOT_IMPLEMENTED_STATUS} {NOT_IMPLEMENTED:?}, got {} {:?} with {:?}",
                copied.status,
                copied.code(),
                copied.body
            );
            assert_eq!(
                copied.message(),
                message,
                "case {case:?}: the refusal must name the restriction that fired, got {:?}",
                copied.message()
            );
            // Refused before anything was written: the destination key holds nothing.
            let absent = call(
                &router,
                "GET",
                &format!("/{directory}/{destination_key}"),
                &[],
                b"",
            )
            .await;
            assert_eq!(
                absent.status, 404,
                "case {case:?}: a refused copy must store no object, got {} with {:?}",
                absent.status, absent.body
            );
            continue;
        }
        assert_eq!(
            copied.status, 200,
            "case {case:?}: a copy writing the empty tag-set is served, got {} with {:?}",
            copied.status, copied.body
        );
        // Round trip: the copy really stored the object.
        let read = call(
            &router,
            "GET",
            &format!("/{directory}/{destination_key}"),
            &[],
            b"",
        )
        .await;
        assert_eq!(
            (read.status, read.body.as_str()),
            (200, "source bytes"),
            "case {case:?}: the copied object must read back, got {} with {:?}",
            read.status,
            read.body
        );
    }
}

/// One row of the copy table: the case, the source key, the headers the copy carries beyond
/// the source reference, and the sentence it is refused with (`None` for a copy that is served).
type CopyRow = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    Option<&'static str>,
);

/// The two sentences a copy that would write tags into a directory bucket is refused with, each
/// naming the restriction that fired: the carrying over of the SOURCE's tag set, whose remedy is
/// `TaggingDirective=REPLACE` over an empty set, and the object tag-set plane a request that
/// states a set of its own asks for. Both are fixed text, with nothing of the request in them.
const COPYING_SOURCE_TAGS: &str =
    "This bucket does not support copying source objects with Object Tags.";
/// See [`COPYING_SOURCE_TAGS`].
const OBJECT_TAGS: &str = "This bucket does not support Object Tags.";

/// One row of the session table: the case, the token presented, and the status and code it
/// earns — `None` for a call that is served.
type SessionRow<'token> = (&'static str, &'token str, Option<(u16, &'static str)>);

/// `POST /<bucket>/<key>?uploads`, then one `UploadPart` per body, then
/// `CompleteMultipartUpload` — answering the entity tag the completion reported.
async fn multipart_etag(router: &MultiServiceRouter, bucket: &str, key: &str, parts: &[Vec<u8>]) -> String {
    let initiated = call(
        router,
        "POST",
        &format!("/{bucket}/{key}?uploads"),
        &[],
        b"",
    )
    .await;
    assert_eq!(
        initiated.status, 200,
        "the upload in {bucket:?} must be initiated, got {} with {:?}",
        initiated.status, initiated.body
    );
    let upload_id = initiated.element("UploadId");
    assert!(
        !upload_id.is_empty(),
        "the initiation must answer an UploadId, got {:?}",
        initiated.body
    );

    let mut completion = String::from("<CompleteMultipartUpload>");
    for (position, body) in parts.iter().enumerate() {
        let number = position + 1;
        let uploaded = call(
            router,
            "PUT",
            &format!("/{bucket}/{key}?partNumber={number}&uploadId={upload_id}"),
            &[],
            body,
        )
        .await;
        assert_eq!(
            uploaded.status, 200,
            "part {number} of the upload in {bucket:?} must be stored, got {} with {:?}",
            uploaded.status, uploaded.body
        );
        let etag = uploaded.header("etag");
        assert!(
            !etag.is_empty(),
            "part {number} must answer an ETag, got the headers {:?}",
            uploaded.headers
        );
        completion.push_str(&format!(
            "<Part><PartNumber>{number}</PartNumber><ETag>{etag}</ETag></Part>"
        ));
    }
    completion.push_str("</CompleteMultipartUpload>");

    let completed = call(
        router,
        "POST",
        &format!("/{bucket}/{key}?uploadId={upload_id}"),
        &[("content-type", "application/xml")],
        completion.as_bytes(),
    )
    .await;
    assert_eq!(
        completed.status, 200,
        "the upload in {bucket:?} must complete, got {} with {:?}",
        completed.status, completed.body
    );
    let etag = completed.element("ETag");
    assert!(
        !etag.is_empty(),
        "the completion must answer an ETag, got {:?}",
        completed.body
    );
    etag
}

/// The digest part of an entity tag, and the part count after the dash: a multipart tag is
/// `"<digest>-<parts>"`, quoted as HTTP's quoted-string syntax wraps it — and a document that
/// carries the tag as element text may escape those quotes, so both spellings are stripped.
fn tag_parts(etag: &str) -> (String, String) {
    let value = etag_value(etag);
    match value.rsplit_once('-') {
        Some((digest, parts)) => (digest.to_string(), parts.to_string()),
        None => (value, String::new()),
    }
}

/// An entity tag's own VALUE: the digest, without the double quotes HTTP's quoted-string
/// syntax wraps it in however a document spelled them.
fn etag_value(etag: &str) -> String {
    etag.replace("&quot;", "").replace('"', "")
}

/// RULE: an object a multipart upload assembled in a DIRECTORY bucket carries an entity tag
/// that is not the MD5 composite a general purpose bucket's is — the model's own test for a tag
/// which is no MD5 digest of the object data being a digit count other than 32 — while it still
/// identifies the data (the same bytes give the same tag, different bytes a different one).
#[tokio::test]
async fn a_directory_buckets_multipart_etag_is_not_an_md5_composite() {
    let (router, _engine) = emulator();
    let directory = format!("xsvc-mpuetag--{ZONE}--x-s3");
    let general = "xsvc-mpuetag-twin";
    directory_bucket(&router, &directory).await;
    general_purpose_bucket(&router, general).await;

    // Two parts over the published floor for every part but the last, so the completion is the
    // real multipart path rather than a one-part upload.
    let parts = vec![
        vec![b'a'; MIN_MULTIPART_PART_BYTES],
        b"the trailing part".to_vec(),
    ];
    let other = vec![
        vec![b'b'; MIN_MULTIPART_PART_BYTES],
        b"the trailing part".to_vec(),
    ];

    let composite = multipart_etag(&router, general, "assembled.bin", &parts).await;
    let directory_tag = multipart_etag(&router, &directory, "assembled.bin", &parts).await;

    // The general purpose bucket's tag is the MD5 composite: 32 hexadecimal digits and the
    // part count after the dash.
    let (composite_digest, composite_count) = tag_parts(&composite);
    assert_eq!(
        (composite_digest.len(), composite_count.as_str()),
        (32, "2"),
        "case \"the general purpose twin\": expected the MD5 composite form, got {composite:?}"
    );
    assert!(
        composite_digest.chars().all(|digit| digit.is_ascii_hexdigit()),
        "case \"the general purpose twin\": the composite is hexadecimal, got {composite:?}"
    );

    // The directory bucket's is not that tag, and not a 32-digit one either.
    assert_ne!(
        etag_value(&directory_tag),
        etag_value(&composite),
        "case \"the directory bucket\": the same bytes must not produce the general purpose \
         MD5 composite, got {directory_tag:?}"
    );
    let (directory_digest, directory_count) = tag_parts(&directory_tag);
    assert_ne!(
        directory_digest.len(),
        32,
        "case \"the directory bucket\": a tag that is no MD5 digest carries a digit count \
         other than 32, got {directory_tag:?}"
    );
    assert_eq!(
        directory_count, "2",
        "case \"the directory bucket\": the object was assembled from two parts, got \
         {directory_tag:?}"
    );

    // It identifies the DATA: a read reports the tag the write settled, and different bytes
    // carry a different tag.
    let read = call(
        &router,
        "HEAD",
        &format!("/{directory}/assembled.bin"),
        &[],
        b"",
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case \"the directory bucket\": the assembled object must read back, got {}",
        read.status
    );
    assert_eq!(
        etag_value(&read.header("etag")),
        etag_value(&directory_tag),
        "case \"the directory bucket\": every read reports the tag the write recorded, got {:?}",
        read.header("etag")
    );
    let changed = multipart_etag(&router, &directory, "changed.bin", &other).await;
    assert_ne!(
        etag_value(&changed),
        etag_value(&directory_tag),
        "case \"different object data\": objects with different data have different entity \
         tags, got {changed:?}"
    );
}

/// RULE: an S3 Express session the clock has expired authorizes nothing on the bucket it was
/// created for, so a Zonal request presenting its token is refused 403 `AccessDenied` — the
/// same refusal a live session created for ANOTHER bucket earns, and not the credential-level
/// `ExpiredToken` a security token presented as the credential itself would earn.
#[tokio::test]
async fn an_expired_express_session_token_is_denied() {
    let (router, engine) = emulator();
    let bucket = format!("xsvc-expiredsession--{ZONE}--x-s3");
    directory_bucket(&router, &bucket).await;
    let written = call(&router, "PUT", &format!("/{bucket}/held.txt"), &[], b"held").await;
    assert_eq!(
        written.status, 200,
        "the object must be stored first, got {} with {:?}",
        written.status, written.body
    );

    // The live session a client takes on the bucket, and its token.
    let vended = call(&router, "GET", &format!("/{bucket}?session"), &[], b"").await;
    assert_eq!(
        vended.status, 200,
        "a session on the bucket must be vended, got {} with {:?}",
        vended.status, vended.body
    );
    let live = vended.element("SessionToken");
    assert!(
        !live.is_empty(),
        "the session must carry a token, got {:?}",
        vended.body
    );

    // A session the CLOCK has already expired, recorded directly in the store: no sequence of
    // requests can reach that state, because the documented lifetime is five minutes. Its
    // expiry is the epoch — an instant in the past on every host, so nothing here depends on
    // how fast this one runs or on reading its clock.
    let expired = "an-expired-sessions-token";
    {
        let state = engine.state::<ComAmazonawsS3Amazons3State>();
        let mut store = state.lock();
        assert!(
            store.record_vended_session_on(
                "EMULATORAKEXPIREDSESSION",
                SystemTime::UNIX_EPOCH,
                &bucket,
                expired,
            ),
            "the store must take the expired session this case is about"
        );
    }

    // (case, the token presented, the status and code expected — `None` for a call served)
    let rows: [SessionRow<'_>; 3] = [
        ("the bucket's own live session", live.as_str(), None),
        (
            "a session the clock has expired",
            expired,
            Some((ACCESS_DENIED_STATUS, ACCESS_DENIED)),
        ),
        (
            "a token no session carries",
            "not-a-token-this-emulator-vended",
            Some((400, "InvalidToken")),
        ),
    ];
    for (case, token, refused) in rows {
        let answered = call(
            &router,
            "GET",
            &format!("/{bucket}/held.txt"),
            &[(SESSION_TOKEN_HEADER, token)],
            b"",
        )
        .await;
        match refused {
            None => assert_eq!(
                answered.status, 200,
                "case {case:?}: expected the call to be served, got {} with {:?}",
                answered.status, answered.body
            ),
            Some((status, code)) => {
                assert_eq!(
                    (answered.status, answered.code().as_str()),
                    (status, code),
                    "case {case:?}: expected {status} {code:?}, got {} {:?} with {:?}",
                    answered.status,
                    answered.code(),
                    answered.body
                );
                assert!(
                    !answered.message().contains(token),
                    "case {case:?}: a refusal must not echo the token, got {:?}",
                    answered.message()
                );
            }
        }
    }

    // And the sentence the expired session earns is the published one for that code, so a
    // client branching on it reads the same answer it reads for a session scoped elsewhere.
    let denied = call(
        &router,
        "GET",
        &format!("/{bucket}/held.txt"),
        &[(SESSION_TOKEN_HEADER, expired)],
        b"",
    )
    .await;
    assert_eq!(
        denied.message(),
        ACCESS_DENIED_MESSAGE,
        "case \"a session the clock has expired\": expected the message \
         {ACCESS_DENIED_MESSAGE:?}, got {:?}",
        denied.message()
    );
}
