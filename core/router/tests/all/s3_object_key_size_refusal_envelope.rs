// SPDX-License-Identifier: CC0-1.0

//! The envelope S3 refuses an object KEY past the published ceiling with, over every write
//! that names a key.
//!
//! The rule under test: **a request refused because a value of its own is too large is
//! answered with that class's fixed sentence plus the `<Size>` / `<MaxSizeAllowed>` pair — the
//! size the request's own value has and the ceiling it was judged against — because the
//! sentence names neither number, so the pair is the only place the refusal says by how much
//! the caller has to trim it.** For an object key the ceiling is the published one: "the name
//! for a key is a sequence of Unicode characters whose UTF-8 encoding is at most 1,024 bytes
//! long" (Amazon S3 User Guide, *Creating object key names*), the code is `KeyTooLongError`
//! and the sentence is "Your key is too long" (Amazon S3 *Error responses*), and `<Size>` is
//! the key's length in BYTES, which is what the ceiling bounds.
//!
//! The rows span what the rule generalises over rather than one recorded call: the size of the
//! name (at the ceiling, one byte past it, far past it), its ENCODING (a name whose characters
//! are each several bytes, so the byte count and the character count differ), and the
//! OPERATION that names it — `PutObject`, `CopyObject`'s destination and
//! `CreateMultipartUpload` are each bound to the same `{Key+}` label and judged by the same
//! rule, so each must answer the same document.
//!
//! The suite lives here because this crate's integration-test target is the one a new suite
//! can be added to without touching a declaration list.

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{opt_element, path_encode};

/// The Region every request here is signed for, and so the endpoint it addresses.
const REGION: &str = "us-east-1";

/// The bucket every row writes into.
const BUCKET: &str = "key-size-envelope-bucket";

/// The key of the object a copy row copies FROM, and the bytes it holds.
const SOURCE_KEY: &str = "source.txt";
const BODY: &[u8] = b"the object a copy row copies";

/// The published ceiling on an object key, in bytes — "the name for a key is a sequence of
/// Unicode characters whose UTF-8 encoding is at most 1,024 bytes long" (Amazon S3 User Guide,
/// *Creating object key names*). Spelled out here, so the suite pins the number rather than
/// reading it out of the code under test.
const MAX_KEY_BYTES: usize = 1_024;

/// The code and the sentence S3 refuses a longer name with (Amazon S3 *Error responses*,
/// `ErrorCodeList`: "KeyTooLongError — Your key is too long"), and the status it carries.
const KEY_TOO_LONG: &str = "KeyTooLongError";
const KEY_TOO_LONG_MESSAGE: &str = "Your key is too long";
const KEY_TOO_LONG_STATUS: u16 = 400;

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

/// The write that names the key, which is what decides the request a row sends. Each is bound
/// to the same `{Key+}` label and judged by the same rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Write {
    /// `PUT /<bucket>/<key>`.
    Put,
    /// `PUT /<bucket>/<key>` carrying `x-amz-copy-source` — the key is the DESTINATION.
    Copy,
    /// `POST /<bucket>/<key>?uploads`.
    InitiateUpload,
}

impl Write {
    /// Send this write of `key` into `BUCKET`. The path is percent-encoded through the one
    /// encoder this test crate escapes a path with, so a key made of multi-byte characters is
    /// addressed as the bytes it is made of — which the `{Key+}` label decodes exactly once.
    async fn send(self, router: &MultiServiceRouter, key: &str) -> Answer {
        let path = path_encode(&format!("/{BUCKET}/{key}"));
        match self {
            Self::Put => s3(router, "PUT", &path, &[], BODY).await,
            Self::Copy => {
                let source = format!("/{BUCKET}/{SOURCE_KEY}");
                s3(
                    router,
                    "PUT",
                    &path,
                    &[("x-amz-copy-source", source.as_str())],
                    b"",
                )
                .await
            }
            Self::InitiateUpload => s3(router, "POST", &format!("{path}?uploads"), &[], b"").await,
        }
    }
}

/// The status every one of those writes answers a key it accepts with: each is bound to a
/// modelled 200.
const SERVED_STATUS: u16 = 200;

/// One row: the name, the key it writes and whether the ceiling admits it.
struct Row {
    case: &'static str,
    key: String,
}

impl Row {
    /// How many BYTES the key is made of, which is what the ceiling bounds.
    fn bytes(&self) -> usize {
        self.key.len()
    }

    /// Whether the published ceiling admits it.
    fn served(&self) -> bool {
        self.bytes() <= MAX_KEY_BYTES
    }
}

/// The rows: the published bound, one byte past it, far past it, and a name whose characters
/// are each several bytes — where the byte count the refusal states is not the character count.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a short key",
            key: "reports/held.csv".to_string(),
        },
        Row {
            case: "a key of exactly the published ceiling",
            key: "k".repeat(MAX_KEY_BYTES),
        },
        Row {
            case: "a key one byte past the ceiling",
            key: "k".repeat(MAX_KEY_BYTES + 1),
        },
        Row {
            case: "a key far past the ceiling",
            key: "k".repeat(MAX_KEY_BYTES * 4),
        },
        Row {
            // 700 two-byte characters: inside the ceiling by CHARACTERS, past it by bytes.
            case: "a key of two-byte characters past the ceiling in bytes",
            key: "é".repeat(700),
        },
        Row {
            // 512 two-byte characters is exactly the ceiling in bytes.
            case: "a key of two-byte characters at exactly the ceiling in bytes",
            key: "é".repeat(MAX_KEY_BYTES / 2),
        },
    ]
}

/// What `answer` must carry for `row`.
fn assert_envelope(case: &str, row: &Row, answer: &Answer) {
    if row.served() {
        assert_eq!(
            answer.status, SERVED_STATUS,
            "case {case:?}: a key of {} bytes is inside the ceiling and must be served, got \
             {} with {:?}",
            row.bytes(),
            answer.status,
            answer.body
        );
        return;
    }
    assert_eq!(
        answer.status, KEY_TOO_LONG_STATUS,
        "case {case:?}: a key of {} bytes is past the ceiling, got {} with {:?}",
        row.bytes(),
        answer.status,
        answer.body
    );
    let code = answer.element("Code");
    assert_eq!(
        code.as_deref(),
        Some(KEY_TOO_LONG),
        "case {case:?}: expected the code {KEY_TOO_LONG:?}, got {code:?} in {:?}",
        answer.body
    );
    let message = answer.element("Message");
    assert_eq!(
        message.as_deref(),
        Some(KEY_TOO_LONG_MESSAGE),
        "case {case:?}: expected the sentence {KEY_TOO_LONG_MESSAGE:?}, got {message:?}"
    );
    // The two numbers the sentence does not name: the key's own byte count and the ceiling.
    let size = answer.element("Size");
    assert_eq!(
        size.as_deref(),
        Some(row.bytes().to_string().as_str()),
        "case {case:?}: expected <Size> {}, got {size:?} in {:?}",
        row.bytes(),
        answer.body
    );
    let allowed = answer.element("MaxSizeAllowed");
    assert_eq!(
        allowed.as_deref(),
        Some(MAX_KEY_BYTES.to_string().as_str()),
        "case {case:?}: expected <MaxSizeAllowed> {MAX_KEY_BYTES}, got {allowed:?} in {:?}",
        answer.body
    );
}

/// Create the bucket and the object a copy row copies from.
async fn fixture(router: &MultiServiceRouter) {
    let created = s3(router, "PUT", &format!("/{BUCKET}"), &[], b"").await;
    assert_eq!(
        created.status, 200,
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
        stored.status, 200,
        "setup: the copy source must be stored, got {} with {:?}",
        stored.status, stored.body
    );
}

#[tokio::test]
async fn every_write_that_names_a_key_states_the_size_and_the_ceiling_it_refused_it_by() {
    for write in [Write::Put, Write::Copy, Write::InitiateUpload] {
        let router = build_router();
        fixture(&router).await;
        for row in rows() {
            let case = format!("{} under {write:?}", row.case);
            let answer = write.send(&router, &row.key).await;
            assert_envelope(&case, &row, &answer);
        }
    }
}

#[tokio::test]
async fn a_key_past_the_ceiling_stores_nothing() {
    let router = build_router();
    fixture(&router).await;
    let key = "k".repeat(MAX_KEY_BYTES + 1);
    let refused = Write::Put.send(&router, &key).await;
    assert_eq!(
        refused.status, KEY_TOO_LONG_STATUS,
        "case \"a refused write\": got {} with {:?}",
        refused.status, refused.body
    );
    // Nothing was stored under it: the bucket still holds the fixture object alone.
    let listed = s3(&router, "GET", &format!("/{BUCKET}?list-type=2"), &[], b"").await;
    assert_eq!(
        listed.status, 200,
        "case \"the listing after a refused write\": got {} with {:?}",
        listed.status, listed.body
    );
    let keys = crate::query_wire::elements(&listed.body, "Key");
    assert_eq!(
        keys,
        vec![SOURCE_KEY.to_string()],
        "case \"the listing after a refused write\": expected the fixture object alone, got \
         {keys:?}"
    );
}
