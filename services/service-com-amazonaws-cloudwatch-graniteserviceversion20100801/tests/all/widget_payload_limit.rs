// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the PER-CALL PAYLOAD LIMIT `GetMetricWidgetImage` publishes, and for the
//! refusal of a `MetricWidget` that is not a widget document at all.
//!
//! "Each GetMetricWidgetImage action has the following limits: … Up to 100 KB uncompressed
//! payload." (`GetMetricWidgetImage`) — the limit is on the CALL's payload, so the rows below vary
//! the size of the request BODY around the published bound rather than the size of any one member,
//! and the refusal belongs to the transport layer in front of the service: a different error class
//! from the one a definition this service cannot render is refused with, and a different status.
//!
//! The suite drives both protocols this service's model declares, because the class a client
//! branches on has to be the same on either wire.

use serde_json::{json, Value};

use crate::harness::{build_router, call, error_code, error_message, query_call};

/// "Up to 100 KB uncompressed payload." (`GetMetricWidgetImage`) — the published bound, in bytes.
const PAYLOAD_LIMIT: usize = 102_400;

/// The wire error code the transport layer refuses a payload past that bound with.
const TOO_LARGE_CODE: &str = "RequestEntityTooLarge";

/// The status that class carries.
const TOO_LARGE_STATUS: u16 = 413;

/// The code a definition this service cannot RENDER is refused with, as the `awsJson` body names
/// its class (`query_compatible_error_envelope`).
const REFUSAL_CODE: &str = "ValidationException";

/// The first eight bytes of every PNG (the signature in the PNG specification).
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// The namespace the definitions here graph, which holds no series: an empty graph is still a
/// graph, and what these rows are about is the size of the request.
const NAMESPACE: &str = "MyApp/PayloadLimit";

/// A widget definition over one metric, padded with `padding` characters of a member the renderer
/// accepts and ignores (`title`), so a row varies the payload's SIZE and nothing else.
fn widget_with_padding(padding: usize) -> String {
    json!({
        "metrics": [[NAMESPACE, "Latency", "Service", "api"]],
        "title": "t".repeat(padding),
    })
    .to_string()
}

/// A request body of EXACTLY `size` bytes, as the wire carries it.
///
/// Each padding character adds exactly one byte to the serialized request (it needs no escaping),
/// so one correction lands on the size the row asked for.
fn request_of_size(size: usize) -> Value {
    let mut padding = 0usize;
    for _ in 0..4 {
        let request = json!({"MetricWidget": widget_with_padding(padding)});
        let sent = serde_json::to_vec(&request)
            .expect("the request body serializes")
            .len();
        if sent == size {
            return request;
        }
        assert!(
            sent < size || padding >= sent - size,
            "a request of {size} bytes is smaller than the smallest one this suite builds"
        );
        if sent < size {
            padding += size - sent;
        } else {
            padding -= sent - size;
        }
    }
    panic!("a request body of exactly {size} bytes could not be built");
}

/// The PNG bytes an accepted call answered with.
fn image(case: &str, body: &Value) -> Vec<u8> {
    use base64::Engine as _;

    let encoded = body
        .get("MetricWidgetImage")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: the answer carries no MetricWidgetImage: {body}"));
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: MetricWidgetImage must be base64-encoded, got {error}")
        })
}

/// A call is served up to the published payload size and refused past it, in the transport
/// layer's own class — the bound itself, one byte either side of it, and a payload well past it.
#[tokio::test]
async fn a_payload_past_the_published_size_is_refused_as_an_entity_too_large() {
    let router = build_router();

    // (the case, the size of the body the client sends, whether the call is served)
    let rows: [(&str, usize, bool); 4] = [
        ("one byte below the published payload size", PAYLOAD_LIMIT - 1, true),
        ("exactly the published payload size", PAYLOAD_LIMIT, true),
        ("one byte past the published payload size", PAYLOAD_LIMIT + 1, false),
        ("a payload well past the published size", PAYLOAD_LIMIT * 2, false),
    ];

    for (case, size, served) in rows {
        let (status, answered) = call(&router, "GetMetricWidgetImage", &request_of_size(size)).await;
        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the call to be served, got {status} with {answered}"
            );
            let bytes = image(case, &answered);
            assert!(
                bytes.starts_with(&PNG_MAGIC),
                "case {case:?}: expected PNG bytes, got {:?}",
                bytes.get(..PNG_MAGIC.len())
            );
        } else {
            assert_eq!(
                status, TOO_LARGE_STATUS,
                "case {case:?}: expected HTTP {TOO_LARGE_STATUS}, got {status} with {answered}"
            );
            assert_eq!(
                error_code(&answered),
                TOO_LARGE_CODE,
                "case {case:?}: expected the {TOO_LARGE_CODE} code, got {answered}"
            );
            // The two sizes that decided the refusal: the one the client sent and the one it may
            // send, so a caller splitting its request knows by how much.
            let message = error_message(&answered);
            assert!(
                message.contains(&size.to_string())
                    && message.contains(&PAYLOAD_LIMIT.to_string()),
                "case {case:?}: expected a refusal naming the {size} bytes sent and the \
                 {PAYLOAD_LIMIT} byte limit, got {message:?}"
            );
        }
    }
}

/// The same class on the `awsQuery` wire this service's model also declares: a form body past the
/// limit is refused with the same code and status, and one inside it is served.
#[tokio::test]
async fn the_query_wire_refuses_an_oversized_form_body_with_the_same_class() {
    let router = build_router();
    const FORM: &str = "application/x-www-form-urlencoded";

    // (the case, the padding of the definition, whether the call is served). A form field's value
    // is percent-encoded, so the padding is what makes the BODY cross the bound rather than an
    // exact size: the rows are a small definition and one whose encoded form is far past it.
    let rows: [(&str, usize, bool); 2] = [
        ("a small definition on the query wire", 16, true),
        ("a definition past the payload size on the query wire", PAYLOAD_LIMIT, false),
    ];

    for (case, padding, served) in rows {
        let (status, _content_type, text) = query_call(
            &router,
            FORM,
            &[],
            "GetMetricWidgetImage",
            &[("MetricWidget", &widget_with_padding(padding))],
        )
        .await;
        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the call to be served, got {status} with {text}"
            );
        } else {
            assert_eq!(
                status, TOO_LARGE_STATUS,
                "case {case:?}: expected HTTP {TOO_LARGE_STATUS}, got {status} with {text}"
            );
            assert!(
                text.contains(TOO_LARGE_CODE),
                "case {case:?}: expected the {TOO_LARGE_CODE} code in the envelope, got {text}"
            );
        }
    }
}

/// A `MetricWidget` that is not a widget DOCUMENT is refused with one sentence, whatever it is
/// instead: the refusal names the property the definition is read from, and says nothing about
/// the contents of a document there was none of.
#[tokio::test]
async fn a_metric_widget_that_is_not_a_json_object_is_refused_with_one_sentence() {
    let router = build_router();

    /// What CloudWatch answers such a payload with.
    const BAD_JSON: &str = "MetricWidget property 'metricWidget' has a bad JSON content";

    // (the case, the value the request carries on `MetricWidget`)
    let rows: [(&str, &str); 8] = [
        ("text that is no JSON at all", "not a document"),
        ("a truncated object", "{\"metrics\": ["),
        ("a JSON array", "[]"),
        ("a JSON string", "\"a widget\""),
        ("a JSON number", "42"),
        ("a JSON null", "null"),
        ("a document with a trailing comma", "{\"metrics\": [],}"),
        ("no characters at all", ""),
    ];

    for (case, payload) in rows {
        let (status, answered) = call(
            &router,
            "GetMetricWidgetImage",
            &json!({"MetricWidget": payload}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {answered}"
        );
        assert_eq!(
            error_code(&answered),
            REFUSAL_CODE,
            "case {case:?}: expected the {REFUSAL_CODE} code, got {answered}"
        );
        assert_eq!(
            error_message(&answered),
            BAD_JSON,
            "case {case:?}: expected the refusal {BAD_JSON:?}, got {:?}",
            error_message(&answered)
        );
    }
}
