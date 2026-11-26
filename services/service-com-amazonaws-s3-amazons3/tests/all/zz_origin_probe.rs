// SPDX-License-Identifier: CC0-1.0

//! End-to-end probe of what the `Origin` header does to a request this service SERVES: a
//! served read that carries none carries no `Access-Control-*` header either, whatever the
//! bucket's cors configuration says.
//!
//! Every assertion drives a real HTTP request through the router, so what is pinned is the
//! wire behaviour (status, headers) rather than any handler's internals.
//!
//! The rule under test: a cors configuration governs CROSS-ORIGIN access — "a browser sends a
//! preflight `OPTIONS` request" and S3 answers it from the bucket's rules, and the
//! `Access-Control-*` headers of a real request are what a browser applies to a response it
//! asked for on another origin's behalf. A request that states no `Origin` asks for nothing of
//! the kind, so its answer carries no such header: inventing one would tell a browser a
//! resource is shared when nothing asked whether it is.
//!
//! The rows differ along what the rule generalises over: the method the read is made with
//! (the bytes and the metadata alone), the address (an object and the bucket's own listing),
//! and whether the configuration that is in place names the origin the preflight asks about.
//! The PREFLIGHT for the same bucket is asserted to carry those headers in the same run, so
//! the suite distinguishes "no decoration without an `Origin`" from "no configuration at all".

use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send, send_bytes, Answer};
use super::object_and_tagging_roundtrip::{bucket, text};

/// The bucket every row addresses, and the key it holds.
const BUCKET: &str = "origin-probe-bucket";
/// See [`BUCKET`].
const KEY: &str = "shared/asset.json";
/// The bytes that key holds.
const BODY: &[u8] = b"{\"shared\":true}";

/// The origin the configuration below names, and the one the preflight asks about.
const ORIGIN: &str = "https://reader.example.org";

/// The `Access-Control-*` response headers a cross-origin answer carries — the ones a read
/// that asked nothing of the kind must carry none of.
const CORS_HEADERS: [&str; 5] = [
    "access-control-allow-origin",
    "access-control-allow-methods",
    "access-control-allow-headers",
    "access-control-allow-credentials",
    "access-control-max-age",
];

/// The cors configuration the bucket carries: one rule naming [`ORIGIN`] and the methods a
/// browser may read it with.
fn configuration() -> String {
    format!(
        "<CORSConfiguration><CORSRule><AllowedOrigin>{ORIGIN}</AllowedOrigin>\
         <AllowedMethod>GET</AllowedMethod><AllowedMethod>HEAD</AllowedMethod>\
         <ExposeHeader>ETag</ExposeHeader><MaxAgeSeconds>3000</MaxAgeSeconds>\
         </CORSRule></CORSConfiguration>"
    )
}

/// `OPTIONS <address>` with the two headers a browser's preflight asks its question in.
async fn preflight(router: &MultiServiceRouter, address: &str, method: &str) -> Answer {
    send(
        router,
        "OPTIONS",
        address,
        &[
            ("origin", ORIGIN),
            ("access-control-request-method", method),
        ],
        "",
    )
    .await
}

#[tokio::test]
async fn a_served_read_that_states_no_origin_carries_no_cors_headers() {
    let router = router();
    bucket(&router, BUCKET).await;
    let stored = send_bytes(&router, "PUT", &format!("/{BUCKET}/{KEY}"), &[], BODY).await;
    assert_eq!(
        stored.status,
        200,
        "the key must be stored first, got {} with body {:?}",
        stored.status,
        text(&stored)
    );
    let configured = send(
        &router,
        "PUT",
        &format!("/{BUCKET}?cors"),
        &[],
        &configuration(),
    )
    .await;
    assert_eq!(
        configured.status,
        200,
        "the bucket must take its cors configuration, got {} with body {:?}",
        configured.status,
        text(&configured)
    );

    // The configuration IS live and the headers ARE reachable: the preflight the rule admits
    // carries them. Without this the rows below would hold for a bucket sharing nothing.
    let answered = preflight(&router, &format!("/{BUCKET}/{KEY}"), "GET").await;
    assert_eq!(
        answered.status,
        200,
        "the preflight the rule admits must be answered 200, got {} with body {:?}",
        answered.status,
        text(&answered)
    );
    assert_eq!(
        answered.header("access-control-allow-origin").as_deref(),
        Some(ORIGIN),
        "the admitted preflight must name the origin it is for, got {:?}",
        answered.header("access-control-allow-origin")
    );

    // (case, method, address) — the reads a browser would make cross-origin, each made here
    // with no `Origin` at all.
    let rows: [(&str, &str, String); 3] = [
        ("the object's bytes", "GET", format!("/{BUCKET}/{KEY}")),
        ("the object's metadata", "HEAD", format!("/{BUCKET}/{KEY}")),
        ("the bucket's own listing", "GET", format!("/{BUCKET}")),
    ];
    for (case, method, address) in rows {
        let served = send(&router, method, &address, &[], "").await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: the read must be served, got {} with body {:?}",
            served.status,
            text(&served)
        );
        for header in CORS_HEADERS {
            assert_eq!(
                served.header(header),
                None,
                "case {case:?}: a read stating no Origin must carry no {header}, got {:?}",
                served.header(header)
            );
        }
    }
}
