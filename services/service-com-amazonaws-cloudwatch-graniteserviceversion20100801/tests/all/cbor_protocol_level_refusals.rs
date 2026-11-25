// SPDX-License-Identifier: CC0-1.0

//! The class a PROTOCOL-level refusal of this service's `rpcv2Cbor` route names.
//!
//! The rule under test: **a refusal made by the protocol in front of the service rather than by
//! its API — a request naming an operation the service does not have, a request addressed to
//! another Smithy service on a route that names the service in its path, a request the route
//! cannot FRAME — is answered with a CBOR map whose `__type` is the FULLY QUALIFIED,
//! coral-namespaced `…Exception` name of that protocol-level class, and with the status the
//! protocol answers that refusal under.** An SDK maps a CBOR error to an exception class by
//! matching that value, so a bare local name resolves to nothing and a client cannot classify
//! the refusal at all.
//!
//! The classes are the framework's, not this API's: they are answered before any operation of
//! this service is named, let alone served, so they are named in the package the framework's
//! request-handling classes are named in — a different package from the one a MODELLED fault of
//! this service carries on the same route (`tests/all/cbor_error_envelope.rs` holds that half).
//!
//! The rows differ along what the rule generalises over: WHICH protocol-level refusal it is
//! (an operation this service does not have, a wrong service in the path, a body that is no
//! CBOR map), how the request misses (an operation name that is no operation, one that is
//! another service's operation, a body of plain text, a body that is a CBOR array, a truncated
//! map), and the status each class answers under. The last two suites are the other half: a
//! well-formed call on the route is served, and a request that names NO protocol of this
//! service keeps the route-less refusal it always had.

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema::Schema;
use aws_smithy_schema_server::CborCodec;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudwatch_graniteserviceversion20100801::ops::types::{
    GetOTelEnrichmentInput, GET_O_TEL_ENRICHMENT_INPUT_SCHEMA,
};
// The entries a client decodes out of an answer on this route: the service's own reader, so
// this suite holds no second definition of it.
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801::service::cbor_error_envelope::error_entries;

use crate::harness::build_router;

/// The Smithy service shape this route addresses, as the path names one.
const SERVICE_SHAPE: &str = "GraniteServiceVersion20100801";

/// The member an error envelope names the class in.
const TYPE_MEMBER: &str = "__type";

/// The two protocol-level classes reachable on this route, as a client resolves them.
const UNKNOWN_OPERATION: &str = "com.amazon.coral.service#UnknownOperationException";
const SERIALIZATION: &str = "com.amazon.coral.service#SerializationException";

/// The status this protocol answers each of them under: its routing rules answer a request it
/// cannot route with a 404, and a request it cannot read is the caller's own 400.
const NOT_ROUTED_STATUS: u16 = 404;
const UNREADABLE_STATUS: u16 = 400;

/// `input` as the `rpcv2Cbor` request body a client sends: written through the protocol's own
/// codec against the operation's input schema, so nothing here encodes CBOR by hand.
fn request_body<I: SerializableStruct>(input_schema: &Schema, input: &I) -> bytes::Bytes {
    let codec = CborCodec::default();
    let mut serializer = codec.create_serializer();
    serializer
        .write_struct(input_schema, input)
        .expect("the input shape serializes");
    bytes::Bytes::from(serializer.finish())
}

/// The body of a request whose operation takes no required member: the empty map this protocol
/// encodes such an input as, which is what a client probing the route sends.
fn empty_input() -> bytes::Bytes {
    request_body(
        GET_O_TEL_ENRICHMENT_INPUT_SCHEMA,
        &GetOTelEnrichmentInput::default(),
    )
}

/// One call on this route: `POST /service/{service}/operation/{operation}` with the protocol's
/// own discriminator, answering the HTTP status and the body as answered.
async fn cbor_call(
    router: &MultiServiceRouter,
    service: &str,
    operation: &str,
    body: bytes::Bytes,
) -> (u16, bytes::Bytes) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("/service/{service}/operation/{operation}"))
        .header("content-type", "application/cbor")
        .header("accept", "application/cbor")
        .header("smithy-protocol", "rpc-v2-cbor")
        // The SigV4 credential scope is what routes the request to the `monitoring` signing
        // name, exactly as it does for this service's JSON and query calls.
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/us-east-1/monitoring/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=deadbeef",
        )
        .body(body)
        .expect("a well-formed rpcv2Cbor request");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.into_body())
}

/// The class the answer `body` names, as a client decoding the map reads it.
fn named_class(case: &str, body: &[u8]) -> String {
    entry(case, body, TYPE_MEMBER)
        .unwrap_or_else(|| panic!("case {case:?}: the error map must name its class, got {body:?}"))
}

/// The value the answer's error map holds under `key`, or `None` where it carries no such
/// entry.
fn entry(case: &str, body: &[u8], key: &str) -> Option<String> {
    let entries = error_entries(body).unwrap_or_else(|| {
        panic!("case {case:?}: the error body must decode as a CBOR map, got {body:?}")
    });
    entries
        .iter()
        .find(|(member, _)| member == key)
        .map(|(_, value)| value.clone())
}

/// The member this route's SERIALIZATION class states its sentence under — the capitalised
/// spelling, where the request-validation class states its clause under the lower-case one.
const UPPER_MESSAGE_MEMBER: &str = "Message";

/// The whole sentence that class states for a request that carried no bytes at all.
const UNEXPECTED_EMPTY_VALUE: &str = "Got an unexpected empty value.";

#[tokio::test]
async fn a_request_carrying_no_bytes_states_the_empty_value_sentence() {
    // The one framing condition that service words a sentence for: a body holding no CBOR item
    // supplied no value to read. The rows differ in the operation addressed — the sentence is
    // the condition's, not any operation's — and the member it is stated under is the
    // capitalised one this class words.
    let router = build_router();
    for operation in ["GetOTelEnrichment", "PutMetricData", "DescribeAlarms"] {
        let case = format!("an empty body on {operation}");
        let (status, payload) =
            cbor_call(&router, SERVICE_SHAPE, operation, bytes::Bytes::new()).await;
        assert_eq!(
            status, UNREADABLE_STATUS,
            "case {case:?}: expected HTTP {UNREADABLE_STATUS}, got {status} with body {:?}",
            String::from_utf8_lossy(&payload)
        );
        assert_eq!(
            named_class(&case, &payload),
            SERIALIZATION,
            "case {case:?}: expected the serialization class"
        );
        let stated = entry(&case, &payload, UPPER_MESSAGE_MEMBER);
        assert_eq!(
            stated.as_deref(),
            Some(UNEXPECTED_EMPTY_VALUE),
            "case {case:?}: expected the sentence under {UPPER_MESSAGE_MEMBER:?}, got {stated:?}"
        );
    }
}

#[tokio::test]
async fn a_protocol_level_refusal_names_its_class_in_full() {
    // (case, the service named in the path, the operation named in the path, the body, the
    // status the refusal answers under, the class a client resolves)
    let rows: Vec<(&str, &str, &str, bytes::Bytes, u16, &str)> = vec![
        (
            "an operation this service does not have",
            SERVICE_SHAPE,
            "NoSuchOperationAtAll",
            empty_input(),
            NOT_ROUTED_STATUS,
            UNKNOWN_OPERATION,
        ),
        (
            "an operation of another service altogether",
            SERVICE_SHAPE,
            "ListQueues",
            empty_input(),
            NOT_ROUTED_STATUS,
            UNKNOWN_OPERATION,
        ),
        (
            "an operation this service HAS, under another service's shape",
            "AmazonSQS",
            "GetOTelEnrichment",
            empty_input(),
            NOT_ROUTED_STATUS,
            UNKNOWN_OPERATION,
        ),
        (
            "an operation this service has, under its own name misspelled",
            "GraniteServiceVersion20100802",
            "GetOTelEnrichment",
            empty_input(),
            NOT_ROUTED_STATUS,
            UNKNOWN_OPERATION,
        ),
        (
            "a body of plain text, which is no CBOR map",
            SERVICE_SHAPE,
            "GetOTelEnrichment",
            bytes::Bytes::from_static(b"not-cbor-at-all"),
            UNREADABLE_STATUS,
            SERIALIZATION,
        ),
        (
            // `0x80` opens a CBOR array of no items: well-formed CBOR, and no structure.
            "a body that is a CBOR array rather than a map",
            SERVICE_SHAPE,
            "GetOTelEnrichment",
            bytes::Bytes::from_static(&[0x80]),
            UNREADABLE_STATUS,
            SERIALIZATION,
        ),
        (
            // `0xa1` opens a map of ONE entry and nothing follows it.
            "a map truncated after its header",
            SERVICE_SHAPE,
            "GetOTelEnrichment",
            bytes::Bytes::from_static(&[0xa1]),
            UNREADABLE_STATUS,
            SERIALIZATION,
        ),
        (
            "a body carrying no bytes at all, which holds no CBOR item",
            SERVICE_SHAPE,
            "GetOTelEnrichment",
            bytes::Bytes::new(),
            UNREADABLE_STATUS,
            SERIALIZATION,
        ),
        (
            "an empty body on an operation that takes required members",
            SERVICE_SHAPE,
            "PutMetricData",
            bytes::Bytes::new(),
            UNREADABLE_STATUS,
            SERIALIZATION,
        ),
        (
            "a body the service's own operation cannot be read out of either",
            SERVICE_SHAPE,
            "PutMetricData",
            bytes::Bytes::from_static(b"{\"Namespace\":\"json-not-cbor\"}"),
            UNREADABLE_STATUS,
            SERIALIZATION,
        ),
    ];

    let router = build_router();
    for (case, service, operation, body, status, class) in rows {
        let (answered, payload) = cbor_call(&router, service, operation, body).await;
        assert_eq!(
            answered, status,
            "case {case:?}: expected HTTP {status}, got {answered} with body {:?}",
            String::from_utf8_lossy(&payload)
        );
        let named = named_class(case, &payload);
        assert_eq!(
            named, class,
            "case {case:?}: expected the class {class:?}, got {named:?}"
        );
    }
}

#[tokio::test]
async fn a_well_formed_call_on_that_route_is_served() {
    // The positive control: the refused calls above differ from this one in the path they
    // address or the body they carry, and nothing else. The body is the empty MAP this
    // protocol's own encoder writes for an input whose members are all optional — the one item
    // a caller that means the empty request sends.
    let router = build_router();
    let (status, body) = cbor_call(
        &router,
        SERVICE_SHAPE,
        "GetOTelEnrichment",
        empty_input(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "expected the call to be served, got {status} with body {:?}",
        String::from_utf8_lossy(&body)
    );
}

#[tokio::test]
async fn a_request_naming_no_protocol_keeps_the_route_less_refusal() {
    // The other half of the rule: these classes belong to the CBOR route, so a request that
    // names no protocol of this service is not answered in that route's envelope at all — it
    // keeps the protocol-neutral refusal the framework gives such a request.
    let router = build_router();
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "/service/{SERVICE_SHAPE}/operation/NoSuchOperationAtAll"
        ))
        .header("content-type", "application/cbor")
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/us-east-1/monitoring/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=deadbeef",
        )
        .body(empty_input())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(&response.into_body()).into_owned();
    assert_eq!(
        (status, body.trim()),
        (NOT_ROUTED_STATUS, "<UnknownOperationException/>"),
        "expected the route-less refusal, got {status} with {body:?}"
    );
}
