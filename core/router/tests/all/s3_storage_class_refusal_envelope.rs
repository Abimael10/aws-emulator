// SPDX-License-Identifier: CC0-1.0

//! The envelope S3 refuses an object write's `x-amz-storage-class` with, over every write that
//! names one.
//!
//! The rule under test: **a value a request carries for a member bound to a modelled enum is
//! judged by the SERVICE, which answers its own published refusal for that member — not by the
//! generic constraint refusal a validating walk words. For a storage class the answer is HTTP
//! 400 `InvalidStorageClass` with the published sentence "The storage class you specified is
//! not valid" and the `<StorageClassRequested>` member echoing the class the request asked
//! for, which is the only place that refusal says WHICH class was rejected, since its sentence
//! names none.** One answer, whether the value is outside the enum the model publishes at all
//! or a published class the addressed bucket cannot host: either way the class the caller named
//! is not one this request can be served under.
//!
//! The rows span what the rule generalises over rather than one recorded call: the KIND of bad
//! value (a name no enum value spells, a published value in another case, a published value
//! with a suffix, and the two published classes that belong to another kind of bucket), and the
//! OPERATION that names it — `PutObject`, `CopyObject` and `CreateMultipartUpload` are each
//! bound to the same `x-amz-storage-class` header, so each must answer the same document. A
//! class a general purpose bucket does host is the served row, so the refusal is not
//! unconditional.
//!
//! The classes a write may not name although the bucket HOLDS objects in them (the
//! service-placed ones, refused `InvalidArgument` instead) are the S3 crate's own
//! `write_storage_class_allowance` suite's rows, and are not re-asserted here.

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::opt_element;

/// The Region every request here is signed for, and so the endpoint it addresses.
const REGION: &str = "us-east-1";

/// The bucket every row writes into, the object a copy row copies from, and its bytes.
const BUCKET: &str = "storage-class-envelope-bucket";
const SOURCE_KEY: &str = "source.bin";
const BODY: &[u8] = b"the object a storage-class row writes";

/// The header a write names the class it asks for in.
const HEADER: &str = "x-amz-storage-class";

/// The code, sentence and status S3 refuses a class the request cannot be served under with
/// (Amazon S3 *Error responses*, `ErrorCodeList`: "InvalidStorageClass — The storage class you
/// specified is not valid"). Spelled out here, so the suite pins the wire values rather than
/// reading them out of the code under test.
const INVALID_STORAGE_CLASS: &str = "InvalidStorageClass";
const INVALID_STORAGE_CLASS_MESSAGE: &str = "The storage class you specified is not valid";
const REFUSAL_STATUS: u16 = 400;

/// S3 alone, on one engine exactly as `core/router/src/lib.rs` mounts it.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Nothing here is signed: the `Authorization` header carries only the credential scope,
    // which is where a service reads the Region from.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// One answer, reduced to what a client branches on.
struct Answer {
    status: u16,
    body: String,
}

impl Answer {
    /// One element of the error document, when it carries one.
    fn element(&self, name: &str) -> Option<String> {
        opt_element(&self.body, name)
    }
}

/// One request to S3, addressed path-style to the Regional endpoint.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/s3/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        );
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router
        .dispatch(
            request
                .body(bytes::Bytes::copy_from_slice(body))
                .expect("a well-formed request"),
        )
        .await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// The write that names the class, which is what decides the request a row sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Write {
    /// `PUT /<bucket>/<key>`.
    Put,
    /// `PUT /<bucket>/<key>` carrying `x-amz-copy-source`.
    Copy,
    /// `POST /<bucket>/<key>?uploads`.
    InitiateUpload,
}

impl Write {
    /// Send this write, naming `class`, under the key `key`.
    async fn send(self, router: &MultiServiceRouter, key: &str, class: &str) -> Answer {
        let path = format!("/{BUCKET}/{key}");
        let named = [(HEADER, class)];
        match self {
            Self::Put => s3(router, "PUT", &path, &named, BODY).await,
            Self::Copy => {
                let source = format!("/{BUCKET}/{SOURCE_KEY}");
                let headers = [(HEADER, class), ("x-amz-copy-source", source.as_str())];
                s3(router, "PUT", &path, &headers, b"").await
            }
            Self::InitiateUpload => {
                s3(router, "POST", &format!("{path}?uploads"), &named, b"").await
            }
        }
    }
}

/// The status each of those writes answers a class it accepts with: each is bound to a
/// modelled 200.
const SERVED_STATUS: u16 = 200;

/// One row: the class a write names, and whether a general purpose bucket can be written under
/// it.
struct Row {
    case: &'static str,
    class: &'static str,
    served: bool,
}

/// The rows: the ways a value can fail to name a class this request can be served under, and
/// the published classes that do.
fn rows() -> [Row; 8] {
    [
        Row {
            case: "a name no published value spells",
            class: "BOGUS",
            served: false,
        },
        Row {
            case: "a published value in another case",
            class: "standard",
            served: false,
        },
        Row {
            case: "a published value with a suffix",
            class: "STANDARD_IAX",
            served: false,
        },
        Row {
            case: "a value that is a number",
            class: "1",
            served: false,
        },
        // Both published, and both another kind of bucket's: "directory buckets only support
        // EXPRESS_ONEZONE … and ONEZONE_IA …" and "Amazon S3 on Outposts only uses the
        // OUTPOSTS Storage Class" (`PutObjectRequest$StorageClass`).
        Row {
            case: "a directory bucket's own class",
            class: "EXPRESS_ONEZONE",
            served: false,
        },
        Row {
            case: "an Outposts bucket's own class",
            class: "OUTPOSTS",
            served: false,
        },
        Row {
            case: "a published class a general purpose bucket hosts",
            class: "STANDARD_IA",
            served: true,
        },
        Row {
            case: "another published class it hosts",
            class: "GLACIER",
            served: true,
        },
    ]
}

/// Create the bucket and the object a copy row copies from.
async fn fixture(router: &MultiServiceRouter) {
    let created = s3(router, "PUT", &format!("/{BUCKET}"), &[], b"").await;
    assert_eq!(
        created.status, SERVED_STATUS,
        "setup: the bucket must be created, got {} with {:?}",
        created.status, created.body
    );
    let stored = s3(
        router,
        "PUT",
        &format!("/{BUCKET}/{SOURCE_KEY}"),
        &[],
        BODY,
    )
    .await;
    assert_eq!(
        stored.status, SERVED_STATUS,
        "setup: the copy source must be stored, got {} with {:?}",
        stored.status, stored.body
    );
}

#[tokio::test]
async fn a_class_a_write_cannot_be_served_under_is_refused_with_the_class_it_named() {
    for write in [Write::Put, Write::Copy, Write::InitiateUpload] {
        let router = build_router();
        fixture(&router).await;
        for row in rows() {
            let case = format!("{} ({:?}) under {write:?}", row.case, row.class);
            let key = format!("{}.bin", row.class.to_ascii_lowercase());
            let answer = write.send(&router, &key, row.class).await;
            if row.served {
                assert_eq!(
                    answer.status, SERVED_STATUS,
                    "case {case:?}: a class the bucket hosts must be served, got {} with {:?}",
                    answer.status, answer.body
                );
                continue;
            }
            assert_eq!(
                answer.status, REFUSAL_STATUS,
                "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {} with {:?}",
                answer.status, answer.body
            );
            let code = answer.element("Code");
            assert_eq!(
                code.as_deref(),
                Some(INVALID_STORAGE_CLASS),
                "case {case:?}: expected the code {INVALID_STORAGE_CLASS:?}, got {code:?} in \
                 {:?}",
                answer.body
            );
            let message = answer.element("Message");
            assert_eq!(
                message.as_deref(),
                Some(INVALID_STORAGE_CLASS_MESSAGE),
                "case {case:?}: expected the published sentence \
                 {INVALID_STORAGE_CLASS_MESSAGE:?}, got {message:?}"
            );
            // The one place the refusal says which class it rejected.
            let requested = answer.element("StorageClassRequested");
            assert_eq!(
                requested.as_deref(),
                Some(row.class),
                "case {case:?}: expected <StorageClassRequested> {:?}, got {requested:?} in \
                 {:?}",
                row.class,
                answer.body
            );
            // Nothing was stored under the key the refused write named.
            let read = s3(&router, "HEAD", &format!("/{BUCKET}/{key}"), &[], b"").await;
            assert_eq!(
                read.status, 404,
                "case {case:?}: a refused write stores nothing, got {} with {:?}",
                read.status, read.body
            );
        }
    }
}
