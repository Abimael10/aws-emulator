// SPDX-License-Identifier: CC0-1.0

//! How the ARN-ADDRESSED tagging surface words a `resourceArn` this emulator holds no
//! resource under.
//!
//! The rule these suites assert: **an operation that addresses its resource generically,
//! by the ARN of any resource class this service owns, refuses an ARN it holds nothing
//! under with 404 `ResourceNotFoundException` carrying the GENERIC fixed sentence
//! `Resource was not found`** — the whole message is fixed text — while the ARN the
//! request named travels in `resourceId` and the class its resource segment names travels
//! in `resourceType`. The three tag operations are exactly that surface: `TagResource`,
//! `UntagResource` and `ListTagsForResource` are all bound to `/tags/{resourceArn}` whose
//! only identifier is an `Arn` (`smithy_query(query="operation",
//! operation="TagResource,UntagResource,ListTagsForResource")`), so none of them is asked
//! for a resource of one named kind. An operation that IS — a cluster's own reader, a
//! snapshot's own reader — keeps its class's own sentence, which the contrast suite below
//! pins so the generic sentence stays scoped to the generic surface.
//!
//! The rule generalises over the operation, over the resource class the ARN names and
//! over the FORM of the identifier, so the table spans all three tag operations and, for
//! each, identifiers differing in which resource segment they spell, their region, their
//! account, the form and width of their resource id, and whether they are an ARN at all.
//! The recorded cases — one `TagResource` and one `UntagResource` against an ARN no
//! resource carries — are two rows of it.
//!
//! The second rule here is about the ENVELOPE rather than the sentence: every failure of
//! this service names its error shape in `x-amzn-errortype` by that shape's NAME, the name
//! the model declares it under, with nothing appended beside it. It holds for a modelled
//! refusal an operation answered and for one the framework raised before any handler ran,
//! and a success names no shape at all.
//!
//! The third rule is the one the two HEADERS of such a refusal carry together, which is
//! what a client classifies the answer by before it parses a byte of the body: **a refusal
//! of the ARN-addressed tagging surface announces its JSON payload as the generic
//! `application/json`, whatever media type the request named for its own payload, and names
//! the modelled error shape in `x-amzn-errortype` with that shape's name as the value's
//! LEADING token** — so a client that dispatches on the media type, and one that resolves
//! the error shape from the header, both read what AWS answers. The rule is about the
//! surface and not about one status or one caller, so the last table spans all three tagging
//! operations, every JSON media type a request can name (including naming none and naming
//! one that is not JSON at all), both resource classes an ARN of this service can name, and
//! a success of the same surface beside the refusals; a route OUTSIDE that surface is the
//! contrast row, which keeps announcing this service's own media type.
//!
//! Each of those two headers has a suite of its own for the WHOLE service —
//! `tests/all/response_media_type.rs` for the media type every operation announces, and
//! `tests/all/error_type_header_form.rs` for the form every failure names its shape in. What
//! the table below adds is the two of them, and the body, asserted on ONE response of this
//! surface: that is the envelope a client of a tag-op refusal reads, and it is the pair that
//! diverged on it.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::collections::BTreeSet;
use std::sync::Arc;

/// The region the suites sign with.
const REGION: &str = "eu-west-2";

/// The canonical placeholder account every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The sentence the ARN-addressed tagging surface answers for an ARN holding no
/// resource: recorded AWS behavior, the entire message with no variable part.
const TAGGED_RESOURCE_SENTENCE: &str = "Resource was not found";

/// The sentences an operation asked for a resource of ONE named class answers instead.
const CLUSTER_SENTENCE: &str = "Unable to find the cluster";
const SNAPSHOT_SENTENCE: &str = "Unable to find the snapshot";

/// The AWS resource type NAMES this service reports in `resourceType`.
///
/// Source: the resource class each operation reads and writes one of, as its own
/// members name it — `GetCluster` answers `cluster` and is addressed by `clusterArn`,
/// `GetClusterSnapshot` answers `snapshot` and is addressed by `snapshotArn`
/// (`smithy_query(query="operation", operation="GetCluster,GetClusterSnapshot")`).
const CLUSTER_TYPE: &str = "Cluster";
const SNAPSHOT_TYPE: &str = "Snapshot";

/// The header a restJson1 client resolves an error shape from.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The error shape every refusal below is refused with, as the model names it
/// (`smithy_query(query="shape", shape="ResourceNotFoundException")`).
const NOT_FOUND_SHAPE: &str = "ResourceNotFoundException";

/// The ARN resource segments of those two classes.
const CLUSTER_SEGMENT: &str = "cluster";
const SNAPSHOT_SEGMENT: &str = "cluster-snapshot";

/// A SigV4 `Authorization` header for this service in [`REGION`].
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/docdb-elastic/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One emulator: the shared engine plus a router carrying only this service.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_docdbelastic_chimeradblionfishservicelambda::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// Percent-encode one value the way an AWS SDK encodes a single path segment or one
/// query-parameter value: an ARN carries both `:` and `/`, so nothing outside the
/// unreserved set survives raw.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Send one request naming the generic JSON media type for its own payload — the way an
/// SDK sends a call of this wire — and return its status, the `x-amzn-errortype` header the
/// response names its error shape in, and its parsed JSON body.
///
/// One request builder serves this file ([`announced`]): a second copy of it could drift
/// from the first and have two suites of the same file disagree about what was sent.
async fn answer(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Option<String>, Value) {
    let (status, _, error_type, parsed) =
        announced(router, method, path, body, Some(GENERIC_JSON_MEDIA_TYPE)).await;
    (status, error_type, parsed)
}

/// [`answer`] without the header, for a call whose envelope is asserted on its body alone.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let (status, _, parsed) = answer(router, method, path, body).await;
    (status, parsed)
}

/// The media type every response of the ARN-addressed tagging surface announces its JSON
/// payload with: the generic one of this service's `restJson1` wire
/// (`smithy_query(query="service_info")`: protocols [restJson1]), whatever the request
/// named for its own payload.
const GENERIC_JSON_MEDIA_TYPE: &str = "application/json";

/// The media type a route OUTSIDE that surface announces when the request named none —
/// recorded behavior of this service's own resource operations, and so the contrast that
/// keeps the rule above scoped to the tagging surface.
const SERVICE_JSON_MEDIA_TYPE: &str = "application/x-amz-json-1.1";

/// Send one request, naming `content_type` for its own payload only when it names one at
/// all, and return the status, the media type the answer announced, the error shape it
/// named, and its parsed JSON body.
async fn announced(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
    content_type: Option<&str>,
) -> (u16, Option<String>, Option<String>, Value) {
    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("https://docdb-elastic.{REGION}.amazonaws.com{path}"))
        .header("host", format!("docdb-elastic.{REGION}.amazonaws.com"))
        .header("authorization", authorization());
    if let Some(content_type) = content_type {
        request = request.header("content-type", content_type);
    }
    let response = router
        .dispatch(
            request
                .body(bytes::Bytes::from(payload))
                .expect("a well-formed request"),
        )
        .await;
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    let media_type = header("content-type");
    let error_type = header(ERROR_TYPE_HEADER);
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!(
                "{method} {path}: body is not JSON ({e}): {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, media_type, error_type, parsed)
}

/// One SUCCESS of the tagging surface: the call, and the media type its request names for
/// its own payload (`None` for a request carrying none).
struct Success {
    case: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    named: Option<&'static str>,
}

/// The LEADING token of an `x-amzn-errortype` value: the error shape a client resolves from
/// it, which is the text before any locator a serving stack appends and after any namespace
/// that declares the shape.
///
/// Spelled out here on purpose, rather than taken from
/// `service_common::error_type_locator::header_shape_named`: that reader is what the service
/// under test uses to write the header, so this suite keeps its own INDEPENDENT oracle for
/// reading it back. It is not a third parsing rule of the wire.
fn leading_token(named: &str) -> &str {
    let before_locator = named.split(':').next().unwrap_or(named).trim();
    before_locator
        .rsplit('#')
        .next()
        .unwrap_or(before_locator)
        .trim()
}

/// The wire error code of an error response (`__type`, bare or namespaced).
fn error_code(body: &Value) -> String {
    let raw = body
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>");
    raw.rsplit('#').next().unwrap_or(raw).to_string()
}

/// The refusal's `message`, or the empty string.
fn message(body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A `CreateCluster` body with every required member and nothing else.
fn minimal_create(name: &str) -> Value {
    json!({
        "clusterName": name,
        "authType": "PLAIN_TEXT",
        "adminUserName": "admin",
        "adminUserPassword": "Secret123!",
        "shardCapacity": 2,
        "shardCount": 1,
    })
}

/// One tagging operation, as it addresses an ARN on the wire.
///
/// `tags` travels in the BODY of `TagResource` and `tagKeys` as repeated `@httpQuery`
/// parameters of `UntagResource`, so each row carries how many of them the call names:
/// the rule holds whatever the call would have written or removed.
struct TagCall {
    operation: &'static str,
    method: &'static str,
    /// The request path for an ARN, query string included.
    path: fn(&str) -> String,
    /// The request body for an ARN, when the operation carries one.
    body: fn() -> Option<Value>,
}

/// All three operations bound to `/tags/{resourceArn}`, each carrying a different number
/// of tags or tag keys.
fn tag_calls() -> Vec<TagCall> {
    vec![
        TagCall {
            operation: "TagResource",
            method: "POST",
            path: |arn| format!("/tags/{}", encode(arn)),
            body: || Some(json!({"tags": {"Project": "lionfish"}})),
        },
        TagCall {
            operation: "TagResource with several tags",
            method: "POST",
            path: |arn| format!("/tags/{}", encode(arn)),
            body: || {
                Some(json!({"tags": {"Project": "lionfish", "Tier": "gold", "Owner": ""}}))
            },
        },
        TagCall {
            operation: "UntagResource",
            method: "DELETE",
            path: |arn| format!("/tags/{}?tagKeys=Project", encode(arn)),
            body: || None,
        },
        TagCall {
            operation: "UntagResource naming several keys",
            method: "DELETE",
            path: |arn| format!("/tags/{}?tagKeys=Project&tagKeys=Tier", encode(arn)),
            body: || None,
        },
        TagCall {
            operation: "ListTagsForResource",
            method: "GET",
            path: |arn| format!("/tags/{}", encode(arn)),
            body: || None,
        },
    ]
}

/// One reader asked for a resource of ONE named class: the route the class's own
/// identifier travels in, and the sentence that class is missed with.
struct TypedReader {
    operation: &'static str,
    path: fn(&str) -> String,
    sentence: &'static str,
}

/// One identifier no resource carries, labelled by what makes it differ from the others,
/// with the resource type name its ARN segment names.
struct Identifier {
    case: &'static str,
    value: String,
    resource_type: &'static str,
}

/// Identifiers differing in resource segment, region, account, resource-id form and
/// width, and whether they are an ARN at all.
///
/// An identifier spelling no segment this service owns addresses its primary taggable
/// resource, the elastic cluster, so its reported type is [`CLUSTER_TYPE`].
fn identifiers() -> Vec<Identifier> {
    vec![
        Identifier {
            case: "a cluster ARN in the signing region",
            value: format!(
                "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:{CLUSTER_SEGMENT}/11111111-2222-3333-4444-555555555555"
            ),
            resource_type: CLUSTER_TYPE,
        },
        Identifier {
            case: "a cluster ARN in another region",
            value: format!(
                "arn:aws:docdb-elastic:us-west-2:{ACCOUNT}:{CLUSTER_SEGMENT}/00000000-0000-0000-0000-000000000000"
            ),
            resource_type: CLUSTER_TYPE,
        },
        Identifier {
            case: "a cluster ARN in another account",
            value: format!(
                "arn:aws:docdb-elastic:{REGION}:210987654321:{CLUSTER_SEGMENT}/ABCDEF01-2345-6789-abcd-ef0123456789"
            ),
            resource_type: CLUSTER_TYPE,
        },
        Identifier {
            case: "a snapshot ARN in the signing region",
            value: format!(
                "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:{SNAPSHOT_SEGMENT}/99999999-8888-7777-6666-555555555555"
            ),
            resource_type: SNAPSHOT_TYPE,
        },
        Identifier {
            case: "a snapshot ARN whose resource id is a long word",
            value: format!(
                "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:{SNAPSHOT_SEGMENT}/{}",
                "z".repeat(120)
            ),
            resource_type: SNAPSHOT_TYPE,
        },
        Identifier {
            case: "an identifier that is not an ARN at all",
            value: "not-an-arn-at-all".to_string(),
            resource_type: CLUSTER_TYPE,
        },
    ]
}

#[tokio::test]
async fn a_tagging_call_on_an_unheld_arn_states_the_generic_resource_sentence() {
    let router = emulator();
    // A tagged cluster DOES exist, so every row below refuses the ARN the request named
    // rather than an empty store.
    let (status, created) = call(
        &router,
        "POST",
        "/cluster",
        Some(minimal_create("present-cluster")),
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture cluster must be created, got {status} with {created}"
    );

    for tag_call in tag_calls() {
        for identifier in identifiers() {
            let case = format!("{} / {}", tag_call.operation, identifier.case);
            let (status, body) = call(
                &router,
                tag_call.method,
                &(tag_call.path)(&identifier.value),
                (tag_call.body)(),
            )
            .await;

            assert_eq!(
                status, 404,
                "case {case:?}: an ARN holding no resource is HTTP 404, got {status} with \
                 {body}"
            );
            assert_eq!(
                error_code(&body),
                "ResourceNotFoundException",
                "case {case:?}: expected a ResourceNotFoundException, got {body}"
            );
            assert_eq!(
                message(&body),
                TAGGED_RESOURCE_SENTENCE,
                "case {case:?}: expected the generic sentence {TAGGED_RESOURCE_SENTENCE:?}, \
                 got {:?}",
                message(&body)
            );
            // Fixed text: no part of the refused request comes back in it, and it names
            // no resource class.
            for fragment in [
                identifier.value.as_str(),
                REGION,
                ACCOUNT,
                "arn:aws:docdb-elastic",
                "cluster",
                "snapshot",
            ] {
                assert!(
                    !message(&body).contains(fragment),
                    "case {case:?}: the message must carry no part of the request and no \
                     resource class, found {fragment:?} in {:?}",
                    message(&body)
                );
            }
            assert_eq!(
                body["resourceId"].as_str(),
                Some(identifier.value.as_str()),
                "case {case:?}: the ARN belongs in resourceId, got {body}"
            );
            assert_eq!(
                body["resourceType"].as_str(),
                Some(identifier.resource_type),
                "case {case:?}: resourceType must be {:?}, got {body}",
                identifier.resource_type
            );
        }
    }
}

#[tokio::test]
async fn one_generic_sentence_serves_every_tagging_call_and_identifier() {
    // The defect this pins: a sentence chosen per resource class (or built from the
    // request) varies across these calls, where AWS's is one constant. So the sentence
    // is collected over every operation and every identifier and must come back as ONE
    // distinct value.
    let router = emulator();
    let mut sentences = BTreeSet::new();

    for tag_call in tag_calls() {
        for identifier in identifiers() {
            let case = format!("{} / {}", tag_call.operation, identifier.case);
            let (status, body) = call(
                &router,
                tag_call.method,
                &(tag_call.path)(&identifier.value),
                (tag_call.body)(),
            )
            .await;
            assert_eq!(
                status, 404,
                "case {case:?}: expected HTTP 404, got {status} with {body}"
            );
            sentences.insert(message(&body));
        }
    }

    assert_eq!(
        sentences,
        [TAGGED_RESOURCE_SENTENCE.to_string()].into_iter().collect(),
        "case the whole tagging surface: one fixed sentence must serve every call, got \
         {sentences:?}"
    );
}

#[tokio::test]
async fn an_operation_asked_for_one_named_class_keeps_its_own_sentence() {
    // The scope of the rule: the generic sentence belongs to the surface addressed by a
    // bare ARN. A reader asked for a resource of one named class was asked which class,
    // so it still answers that class's own sentence for the very same identifiers.
    let router = emulator();

    let readers = vec![
        TypedReader {
            operation: "GetCluster",
            path: |arn| format!("/cluster/{}", encode(arn)),
            sentence: CLUSTER_SENTENCE,
        },
        TypedReader {
            operation: "GetClusterSnapshot",
            path: |arn| format!("/cluster-snapshot/{}", encode(arn)),
            sentence: SNAPSHOT_SENTENCE,
        },
    ];

    for reader in readers {
        let expected = reader.sentence;
        for identifier in identifiers() {
            let case = format!("{} / {}", reader.operation, identifier.case);
            let (status, body) = call(&router, "GET", &(reader.path)(&identifier.value), None).await;
            assert_eq!(
                status, 404,
                "case {case:?}: an absent resource is HTTP 404, got {status} with {body}"
            );
            assert_eq!(
                message(&body),
                expected,
                "case {case:?}: expected the class sentence {expected:?}, got {:?}",
                message(&body)
            );
            assert_ne!(
                message(&body),
                TAGGED_RESOURCE_SENTENCE,
                "case {case:?}: a typed lookup must not take the generic sentence"
            );
        }
    }
}

#[tokio::test]
async fn every_failure_names_its_error_shape_by_its_name() {
    // The envelope rule, over everything that can answer a failure: the modelled refusal
    // each tag operation answers for an ARN it holds nothing under, a refusal the
    // framework raises before any handler runs, and a typed reader's own refusal. A
    // success names no error shape at all.
    let router = emulator();
    let unheld = format!(
        "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:{CLUSTER_SEGMENT}/00000000-0000-0000-0000-000000000000"
    );

    let (status, error_type, created) = answer(
        &router,
        "POST",
        "/cluster",
        Some(minimal_create("qualified-cluster")),
    )
    .await;
    assert_eq!(
        status, 200,
        "the cluster must be created, got {status} with {created}"
    );
    assert_eq!(
        error_type, None,
        "case \"a success\": a success names no error shape, got {error_type:?}"
    );

    for tag_call in tag_calls() {
        let case = format!("{} / an ARN no resource carries", tag_call.operation);
        let (status, error_type, body) = answer(
            &router,
            tag_call.method,
            &(tag_call.path)(&unheld),
            (tag_call.body)(),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} with {body}"
        );
        let expected = NOT_FOUND_SHAPE.to_string();
        assert_eq!(
            error_type.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected {expected:?} in {ERROR_TYPE_HEADER}, got {error_type:?}"
        );
        // The shape the header names is the shape the body names in `__type`, and the
        // header carries nothing beside that name.
        assert_eq!(
            error_type.clone().unwrap_or_default(),
            error_code(&body),
            "case {case:?}: the header and the body must name one shape, got {error_type:?} \
             and {body}"
        );
    }

    // A refusal the FRAMEWORK raises: `tags` is @required on TagResource
    // (`smithy_query(query="operation", operation="TagResource")`), so a body omitting it
    // is refused before the handler runs — and that refusal names its shape the same way.
    let (status, error_type, body) = answer(
        &router,
        "POST",
        &format!("/tags/{}", encode(&unheld)),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"an omitted required member\": expected HTTP 400, got {status} with {body}"
    );
    let named = error_type.unwrap_or_default();
    assert_eq!(
        named,
        error_code(&body),
        "case \"an omitted required member\": the header must name the shape the body names, \
         got {named:?} and {body}"
    );
    assert!(
        !named.is_empty() && named.chars().all(|c| c.is_ascii_alphanumeric()),
        "case \"an omitted required member\": expected the shape name alone in \
         {ERROR_TYPE_HEADER}, got {named:?}"
    );

    // A typed reader's refusal: the envelope rule is the whole service's, not the tagging
    // surface's.
    let (status, error_type, body) = answer(
        &router,
        "GET",
        &format!("/cluster/{}", encode(&unheld)),
        None,
    )
    .await;
    assert_eq!(
        status, 404,
        "case \"GetCluster on an absent cluster\": expected HTTP 404, got {status} with {body}"
    );
    let expected = NOT_FOUND_SHAPE.to_string();
    assert_eq!(
        error_type.as_deref(),
        Some(expected.as_str()),
        "case \"GetCluster on an absent cluster\": expected {expected:?}, got {error_type:?}"
    );
}

#[tokio::test]
async fn an_arn_this_service_does_hold_is_still_tagged_and_read_back() {
    // The positive control: the refusal above is about the ARN, not about the surface.
    // A resource that exists takes a tag write and reports it back through the reader.
    let router = emulator();
    let (status, created) = call(
        &router,
        "POST",
        "/cluster",
        Some(minimal_create("tagged-cluster")),
    )
    .await;
    assert_eq!(
        status, 200,
        "the cluster must be created, got {status} with {created}"
    );
    let arn = created["cluster"]["clusterArn"]
        .as_str()
        .expect("the created cluster carries its ARN")
        .to_string();

    let (status, body) = call(
        &router,
        "POST",
        &format!("/tags/{}", encode(&arn)),
        Some(json!({"tags": {"Project": "lionfish"}})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"tagging a held ARN\": expected HTTP 200, got {status} with {body}"
    );

    let (status, body) = call(&router, "GET", &format!("/tags/{}", encode(&arn)), None).await;
    assert_eq!(
        status, 200,
        "case \"reading a held ARN\": expected HTTP 200, got {status} with {body}"
    );
    assert_eq!(
        body["tags"]["Project"].as_str(),
        Some("lionfish"),
        "case \"reading a held ARN\": expected the written tag back, got {body}"
    );

    let (status, body) = call(
        &router,
        "DELETE",
        &format!("/tags/{}?tagKeys=Project", encode(&arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"untagging a held ARN\": expected HTTP 200, got {status} with {body}"
    );
    let (status, body) = call(&router, "GET", &format!("/tags/{}", encode(&arn)), None).await;
    assert_eq!(
        status, 200,
        "case \"reading after untag\": expected HTTP 200, got {status} with {body}"
    );
    assert!(
        body["tags"].get("Project").is_none(),
        "case \"reading after untag\": the removed key must be gone, got {body}"
    );
}

#[tokio::test]
async fn a_tagging_refusal_announces_the_generic_media_type_and_leads_with_its_error_code() {
    // The two headers a client classifies this refusal by, asserted on ONE response: the
    // media type it announces its JSON payload with, and the error shape it names. The rows
    // span all three tagging operations, every JSON media type a request can name for its
    // own payload (naming none and naming one that is not JSON included), and both resource
    // classes an ARN of this service can name — the recorded cases, a tag read and a tag
    // delete of an ARN no resource carries, are rows of it.
    let router = emulator();

    // The media types a request can name for its own payload. A request naming none at all
    // is the `None` row: that is how a read and a delete arrive.
    let named_media_types = [
        ("no media type", None),
        ("the generic JSON media type", Some(GENERIC_JSON_MEDIA_TYPE)),
        ("this service's own", Some(SERVICE_JSON_MEDIA_TYPE)),
        ("another AWS JSON version", Some("application/x-amz-json-1.0")),
        ("a parameterised media type", Some("application/json; charset=utf-8")),
        ("a media type that is not JSON", Some("application/xml")),
    ];

    for tag_call in tag_calls() {
        for identifier in identifiers() {
            for (named_case, named) in named_media_types {
                let case = format!(
                    "{} / {} / {named_case}",
                    tag_call.operation, identifier.case
                );
                let (status, media_type, error_type, body) = announced(
                    &router,
                    tag_call.method,
                    &(tag_call.path)(&identifier.value),
                    (tag_call.body)(),
                    named,
                )
                .await;

                assert_eq!(
                    status, 404,
                    "case {case:?}: an ARN holding no resource is HTTP 404, got {status} \
                     with {body}"
                );
                assert_eq!(
                    media_type.as_deref(),
                    Some(GENERIC_JSON_MEDIA_TYPE),
                    "case {case:?}: expected the payload announced as \
                     {GENERIC_JSON_MEDIA_TYPE:?}, got {media_type:?}"
                );
                let named_shape = error_type.clone().unwrap_or_else(|| {
                    panic!("case {case:?}: a refusal must name its error shape in {ERROR_TYPE_HEADER}")
                });
                assert_eq!(
                    leading_token(&named_shape),
                    NOT_FOUND_SHAPE,
                    "case {case:?}: expected {NOT_FOUND_SHAPE:?} to lead \
                     {ERROR_TYPE_HEADER}, got {named_shape:?}"
                );
                // No Amazon-internal locator rides along: a client reading the whole value
                // sees nothing a real answer does not carry.
                assert!(
                    !named_shape.contains("internal.amazon.com"),
                    "case {case:?}: {ERROR_TYPE_HEADER} must carry no internal locator, got \
                     {named_shape:?}"
                );
                // The envelope the two headers announce is the one the body carries.
                assert_eq!(
                    error_code(&body),
                    NOT_FOUND_SHAPE,
                    "case {case:?}: the body must name the same shape, got {body}"
                );
                assert_eq!(
                    message(&body),
                    TAGGED_RESOURCE_SENTENCE,
                    "case {case:?}: expected {TAGGED_RESOURCE_SENTENCE:?}, got {:?}",
                    message(&body)
                );
            }
        }
    }
}

#[tokio::test]
async fn the_tagging_surface_announces_one_media_type_whatever_the_status() {
    // The rule belongs to the SURFACE, not to a status: a success of the same three
    // operations is announced with the same media type and names no error shape. The
    // contrast row is a route outside that surface, which keeps announcing this service's
    // own media type for a request that named none — so the rule does not leak into the
    // rest of the service.
    let router = emulator();
    let (status, created) = call(
        &router,
        "POST",
        "/cluster",
        Some(minimal_create("announced-cluster")),
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture cluster must be created, got {status} with {created}"
    );
    let arn = created["cluster"]["clusterArn"]
        .as_str()
        .expect("the created cluster carries its ARN")
        .to_string();

    let successes = vec![
        Success {
            case: "TagResource on a held ARN",
            method: "POST",
            path: format!("/tags/{}", encode(&arn)),
            body: Some(json!({"tags": {"Project": "lionfish"}})),
            named: Some(GENERIC_JSON_MEDIA_TYPE),
        },
        Success {
            case: "ListTagsForResource on a held ARN, naming no media type",
            method: "GET",
            path: format!("/tags/{}", encode(&arn)),
            body: None,
            named: None,
        },
        Success {
            case: "ListTagsForResource on a held ARN, naming this service's own",
            method: "GET",
            path: format!("/tags/{}", encode(&arn)),
            body: None,
            named: Some(SERVICE_JSON_MEDIA_TYPE),
        },
        Success {
            case: "UntagResource on a held ARN",
            method: "DELETE",
            path: format!("/tags/{}?tagKeys=Project", encode(&arn)),
            body: None,
            named: Some(SERVICE_JSON_MEDIA_TYPE),
        },
    ];

    for Success {
        case,
        method,
        path,
        body,
        named,
    } in successes
    {
        let (status, media_type, error_type, answered) =
            announced(&router, method, &path, body, named).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {answered}"
        );
        assert_eq!(
            media_type.as_deref(),
            Some(GENERIC_JSON_MEDIA_TYPE),
            "case {case:?}: expected {GENERIC_JSON_MEDIA_TYPE:?}, got {media_type:?}"
        );
        assert_eq!(
            error_type, None,
            "case {case:?}: a success names no error shape, got {error_type:?}"
        );
    }

    // The contrast: a cluster's own reader is not the ARN-addressed surface. Each row names
    // the status its own ARN must answer, so a row that stops exercising its route fails.
    let unheld = format!(
        "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:{CLUSTER_SEGMENT}/00000000-0000-0000-0000-000000000000"
    );
    for (case, path, expected) in [
        (
            "GetCluster of a held ARN",
            format!("/cluster/{}", encode(&arn)),
            200,
        ),
        (
            "GetCluster of an ARN no cluster carries",
            format!("/cluster/{}", encode(&unheld)),
            404,
        ),
    ] {
        let (status, media_type, _, body) = announced(&router, "GET", &path, None, None).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected HTTP {expected}, got {status} with {body}"
        );
        assert_eq!(
            media_type.as_deref(),
            Some(SERVICE_JSON_MEDIA_TYPE),
            "case {case:?}: a route outside the tagging surface announces \
             {SERVICE_JSON_MEDIA_TYPE:?} for a request naming none, got {media_type:?}"
        );
    }
}
