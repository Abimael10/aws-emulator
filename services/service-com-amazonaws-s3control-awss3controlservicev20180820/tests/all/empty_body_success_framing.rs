// SPDX-License-Identifier: CC0-1.0

//! What an S3 Control success whose answer carries NO document puts on the wire,
//! driven through the SAME framing path a client's HTTP stack reads: the status
//! line and the `content-length` the response is framed with.
//!
//! The rule under test: the success status of an empty-output member is that
//! MEMBER's own answer, not a property of the empty output shape it shares with
//! its siblings. The account-level `PublicAccessBlock` put answers the `200` its
//! model declares, and a 200 with an empty body is framed with
//! `content-length: 0`; the delete beside it answers `204 No Content`, which
//! carries no `content-length` at all. A client that keys on the status class, or
//! on the presence of the framing header, therefore sees two different answers
//! from the two calls — and must see the same two the real service gives.
//!
//! The sibling suite [`crate::empty_body_success_status`] pins the statuses over
//! this service's whole empty-output surface; this one pins what the transport
//! then FRAMES, which the buffered router hands to its caller unframed, so the
//! requests here go through [`::emulator_server::http_connector_call`] — the one
//! conversion both the in-process client and the network listeners put a response
//! through.
//!
//! The rows differ along what the rule generalises over: which of the four
//! settings the request carries at all and their values, whether the put creates
//! the configuration or replaces one, and the Region the request is signed for
//! (the configuration is account-level, so the signing Region decides nothing).

use smithy_server_runtime::MultiServiceRouter;

use crate::access_point_lifecycle::{Emulator, Reply, ACCOUNT, NO_CONTENT, REGION, XMLNS};
// The four members of `PublicAccessBlockConfiguration`, in the order the shape
// declares them, and the status the put answers: both stated once by the suite
// that owns this surface rather than restated here.
use crate::public_access_block::{PUT_OK, SETTINGS};

/// A second signing Region, so a row can show the framing is not a property of
/// the Region the request is signed for.
const OTHER_REGION: &str = "eu-central-1";

/// The URI the account-level public-access block is bound to.
const URI: &str = "/v20180820/configuration/publicAccessBlock";

/// The status a success that reports a document carries.
const WITH_DOCUMENT: u16 = 200;

/// The framing header a response that carries a body — or an empty body with an
/// OK status — is framed with.
const CONTENT_LENGTH: &str = "content-length";

/// Send one request through the router and the response conversion a client's
/// HTTP stack reads, signed for `region`.
///
/// The answer is the sibling suites' [`Reply`], so the XML elements and the
/// headers are read with their readers rather than a second pair. The request is
/// built here rather than through [`crate::access_point_lifecycle::call_in_region`]
/// because this path needs the body as the streaming `SdkBody` the connector
/// takes, and the reply's headers as the connector framed them.
async fn call(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    account_id: &str,
    body: &str,
) -> Reply {
    let request = ::http::Request::builder()
        .method(method)
        .uri(URI)
        .header(
            "host",
            format!("{account_id}.s3-control.{region}.amazonaws.com"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{region}/s3/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .header("x-amz-account-id", account_id)
        .header("content-type", "application/xml")
        .body(::emulator_server::aws_smithy_types::body::SdkBody::from(
            body.to_owned(),
        ))
        .expect("build the request");
    let request =
        ::emulator_server::aws_smithy_runtime_api::client::orchestrator::HttpRequest::try_from(
            request,
        )
        .expect("the request is one the connector carries");
    let response = ::emulator_server::http_connector_call(router, request)
        .await
        .expect("the emulator answers the request");
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.to_string()))
        .collect();
    let body = response
        .into_body()
        .bytes()
        .map(|bytes| String::from_utf8_lossy(bytes).to_string())
        .unwrap_or_default();
    Reply {
        status,
        headers,
        body,
    }
}

/// A request body carrying `settings` for the members `present` names.
fn configuration(settings: [bool; 4], present: [bool; 4]) -> String {
    let members: String = SETTINGS
        .iter()
        .zip(settings)
        .zip(present)
        .filter(|(_, carried)| *carried)
        .map(|((member, value), _)| format!("<{member}>{value}</{member}>"))
        .collect();
    format!(
        "<PublicAccessBlockConfiguration xmlns=\"{XMLNS}\">{members}\
         </PublicAccessBlockConfiguration>"
    )
}

#[tokio::test]
async fn the_account_public_access_block_put_is_framed_as_an_empty_ok() {
    let emulator = Emulator::start();

    // The rows differ in which members the request carries, in their values, and
    // in whether the put creates the configuration or replaces the one the row
    // before it left.
    let rows: [(&str, [bool; 4], [bool; 4]); 5] = [
        ("no member at all", [false; 4], [false; 4]),
        (
            "one member, true",
            [true, false, false, false],
            [true, false, false, false],
        ),
        ("every member, all false", [false; 4], [true; 4]),
        ("every member, all true", [true; 4], [true; 4]),
        (
            "three of the four, mixed",
            [true, false, true, false],
            [true, true, true, false],
        ),
    ];

    for region in [REGION, OTHER_REGION] {
        for (case, settings, present) in rows {
            let case = format!("{case}, signed for {region}");
            let put = call(
                &emulator.router,
                region,
                "PUT",
                ACCOUNT,
                &configuration(settings, present),
            )
            .await;
            assert_eq!(
                put.status, PUT_OK,
                "case {case:?}: expected the put to answer {PUT_OK}, got {} with body {:?}",
                put.status, put.body
            );
            assert!(
                put.body.is_empty(),
                "case {case:?}: expected the put to report no document, got body {:?}",
                put.body
            );
            // An OK with an empty body states its emptiness: the framing header
            // is there and reads zero, which is what a client keying on it sees.
            assert_eq!(
                put.header(CONTENT_LENGTH).as_deref(),
                Some("0"),
                "case {case:?}: expected the empty OK to be framed {CONTENT_LENGTH}: 0, got {:?}",
                put.header(CONTENT_LENGTH)
            );

            // The OK is backed by state, not a shallow success: the reader
            // reports the settings just put, and every member the request left
            // out reads `false` (the model's `@default`).
            let read = call(&emulator.router, region, "GET", ACCOUNT, "").await;
            assert_eq!(
                read.status, WITH_DOCUMENT,
                "case {case:?}: expected the stored configuration to be reported with \
                 {WITH_DOCUMENT}, got {} with body {:?}",
                read.status, read.body
            );
            let reported: Vec<Option<String>> =
                SETTINGS.iter().map(|member| read.text(member)).collect();
            let expected: Vec<Option<String>> = SETTINGS
                .iter()
                .zip(settings)
                .zip(present)
                .map(|((_, value), carried)| Some((carried && value).to_string()))
                .collect();
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the settings just put to be reported, got body {:?}",
                read.body
            );
            // A response that DOES carry a document is framed with its length,
            // so the zero above is the body's length rather than a constant.
            assert_eq!(
                read.header(CONTENT_LENGTH).as_deref(),
                Some(read.body.len().to_string().as_str()),
                "case {case:?}: expected the reported document to be framed with its own \
                 length, got {:?} for a body of {} bytes",
                read.header(CONTENT_LENGTH),
                read.body.len()
            );
        }

        // The contrast that makes the point: the DELETE of the same resource
        // reports nothing either and answers No Content, which carries no
        // framing header at all.
        for case in ["a configuration that is there", "no configuration left"] {
            let delete = call(&emulator.router, region, "DELETE", ACCOUNT, "").await;
            assert_eq!(
                delete.status, NO_CONTENT,
                "case {case:?} ({region}): expected the delete to answer {NO_CONTENT}, got {} \
                 with body {:?}",
                delete.status, delete.body
            );
            assert!(
                delete.body.is_empty(),
                "case {case:?} ({region}): expected no body with {NO_CONTENT}, got {:?}",
                delete.body
            );
            assert_eq!(
                delete.header(CONTENT_LENGTH),
                None,
                "case {case:?} ({region}): expected {NO_CONTENT} to carry no \
                 {CONTENT_LENGTH}, got {:?}",
                delete.header(CONTENT_LENGTH)
            );
        }

        // And the delete really removed it, so the next Region starts from an
        // account holding no configuration.
        let gone = call(&emulator.router, region, "GET", ACCOUNT, "").await;
        assert_eq!(
            (gone.status, gone.text("Code")),
            (404, Some("NoSuchPublicAccessBlockConfiguration".to_string())),
            "region {region}: expected the deleted configuration to be gone, got {} with \
             body {:?}",
            gone.status,
            gone.body
        );
    }
}
