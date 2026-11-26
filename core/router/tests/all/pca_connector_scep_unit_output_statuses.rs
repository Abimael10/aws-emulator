// SPDX-License-Identifier: CC0-1.0

//! What a Connector for SCEP operation modelled with output `smithy.api#Unit` answers.
//!
//! RULE under test: **an operation whose modelled output shape is `smithy.api#Unit` answers
//! the HTTP status its own `@http` trait declares, with an empty body, and the effect it asks
//! for is in state and readable through this service's own reads.** The four bindings come
//! from the model itself (`smithy_query(query="operation",
//! operation="DeleteChallenge,DeleteConnector,TagResource,UntagResource")`):
//! `DELETE /challenges/{ChallengeArn} -> 202`, `DELETE /connectors/{ConnectorArn} -> 202`,
//! `POST /tags/{ResourceArn} -> 204` and `DELETE /tags/{ResourceArn} -> 204`.
//!
//! The service crate derives all four of those output declarations from ONE spelling of that
//! rule (`ops::types::unit_output`, invoked once per operation with that operation's own
//! method, URI and status) instead of from four hand-written copies, so the rows below drive
//! an operation from each former copy over the wire: both modelled statuses (202 and 204),
//! both methods (`POST` and `DELETE`), each of the three URIs, and — for the tagging pair —
//! both resource kinds an ARN can address. Each row asserts the status, the empty body AND
//! the effect through a later read, so a row cannot pass on the status alone.
//!
//! Each case drives the service the way a client does: one HTTP request per call through the
//! same `MultiServiceRouter` the emulator boots, with this service mounted on one shared
//! engine. The suite lives here because this test target picks up a new suite from its file
//! alone, while the service crate's target declares each of its suites by hand.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pcaconnectorscep_pcaconnectorscep as scep;

/// The Region every request here is addressed at and signed for.
const REGION: &str = "us-west-2";

/// A private-CA ARN of exactly the form `CertificateAuthorityArn`'s own `@pattern` requires
/// (`^arn:aws(-[a-z]+)*:acm-pca:<region>:<12 digits>:certificate-authority/<uuid>$`).
const CA_ARN: &str =
    "arn:aws:acm-pca:us-west-2:000000000000:certificate-authority/11111111-2222-3333-4444-555555555555";

/// The modelled response status of the two deletes: `@http` code 202 on `DeleteChallenge`
/// and `DeleteConnector`.
const ACCEPTED: u16 = 202;

/// The modelled response status of the two tagging writes: `@http` code 204 on `TagResource`
/// and `UntagResource`.
const NO_CONTENT: u16 = 204;

/// The `ConnectorStatus` value a connector whose teardown is under way reports, named from
/// the service's own constant so a row cannot drift off it.
const STATUS_DELETING: &str = scep::state::STATUS_DELETING;

/// The tags both creates store, so a tagging row has a key to keep and a key to remove.
const CREATED_TAGS: [(&str, &str); 2] = [("owner", "platform"), ("team", "identity")];

/// The key an `UntagResource` row removes, and the one every row leaves in place.
const REMOVED_KEY: &str = "team";
const KEPT_KEY: &str = "owner";

/// What one call answered.
#[derive(Debug)]
struct Answer {
    status: u16,
    error_type: String,
    raw: Vec<u8>,
    body: Value,
}

/// One emulator: this service mounted on one shared engine, as the router mounts it, with
/// the engine handed back so a row can move the instant a transition is owed.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's own concern, exercised by its own suites.
    router.set_request_authentication(false);
    router.register_service(scep::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// An ARN inside a single `@httpLabel` path segment: the two reserved characters an ARN can
/// carry (`:` and `/`) are escaped, as an AWS SDK escapes them, so neither reads as path
/// structure.
fn label(arn: &str) -> String {
    arn.replace('%', "%25").replace(':', "%3A").replace('/', "%2F")
}

/// One request, addressed at this service's Region endpoint and signed for the same Region.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let payload = match body {
        Some(value) => {
            bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize the request body"))
        }
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://pca-connector-scep.{REGION}.amazonaws.com{path}"
        ))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/pca-connector-scep/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed test request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(['/', ':'])
                .next()
                .unwrap_or(value)
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let raw = response.into_body().to_vec();
    let body = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    Answer {
        status,
        error_type,
        raw,
        body,
    }
}

/// The `Tags` member of a request body, from a list of pairs.
fn tags_member(pairs: &[(&str, &str)]) -> Value {
    Value::Object(
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), Value::String((*value).to_string())))
            .collect(),
    )
}

/// Make the transition the clock owes `arn` due, so the next call materialises it, by
/// rewriting only that transition's due instant — never a status or a stored value.
fn release_pending(engine: &engine::InMemoryEngine, arn: &str) {
    let shared = scep::state::service_state(engine).expect("the service's own state");
    let mut store = shared.lock();
    let pending = store
        .connectors
        .get_mut(arn)
        .and_then(|connector| connector.pending.as_mut())
        .unwrap_or_else(|| panic!("expected a transition the clock owes {arn:?}"));
    pending.due_at = SystemTime::now() - Duration::from_secs(1);
}

/// A connector carrying [`CREATED_TAGS`], with the `ACTIVE` the clock owes it made due so it
/// can mint challenges.
async fn active_connector(router: &MultiServiceRouter, engine: &engine::InMemoryEngine) -> String {
    let created = call(
        router,
        "POST",
        "/connectors",
        Some(json!({
            "CertificateAuthorityArn": CA_ARN,
            "Tags": tags_member(&CREATED_TAGS),
        })),
    )
    .await;
    assert_eq!(
        created.status, ACCEPTED,
        "expected {ACCEPTED} from CreateConnector, got {} / {:?}",
        created.status, created.body
    );
    let arn = created.body["ConnectorArn"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a ConnectorArn in {:?}", created.body))
        .to_string();
    release_pending(engine, &arn);
    arn
}

/// One challenge of `connector`, carrying [`CREATED_TAGS`].
async fn new_challenge(router: &MultiServiceRouter, connector: &str) -> String {
    let created = call(
        router,
        "POST",
        "/challenges",
        Some(json!({ "ConnectorArn": connector, "Tags": tags_member(&CREATED_TAGS) })),
    )
    .await;
    assert_eq!(
        created.status, ACCEPTED,
        "expected {ACCEPTED} from CreateChallenge, got {} / {:?}",
        created.status, created.body
    );
    created.body["Challenge"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a challenge Arn in {:?}", created.body))
        .to_string()
}

/// The tag keys `arn` reports through `ListTagsForResource`, sorted.
async fn listed_tag_keys(router: &MultiServiceRouter, arn: &str, case: &str) -> Vec<String> {
    let listed = call(router, "GET", &format!("/tags/{}", label(arn)), None).await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected 200 from ListTagsForResource, got {} / {:?}",
        listed.status, listed.body
    );
    let mut keys: Vec<String> = listed.body["Tags"]
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: expected a Tags map in {:?}", listed.body))
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// Which resource kind a tagging row addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Connector,
    Challenge,
}

/// Which operation a row drives — one from each of the four former copies of the output
/// declaration, with the tagging pair driven over both resource kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    TagResource(Kind),
    UntagResource(Kind),
    DeleteChallenge,
    DeleteConnector,
}

/// One row: the operation it drives and the status the model declares for it.
#[derive(Debug)]
struct Case {
    name: &'static str,
    op: Op,
    expected: u16,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "TagResource on a connector",
            op: Op::TagResource(Kind::Connector),
            expected: NO_CONTENT,
        },
        Case {
            name: "TagResource on a challenge",
            op: Op::TagResource(Kind::Challenge),
            expected: NO_CONTENT,
        },
        Case {
            name: "UntagResource on a connector",
            op: Op::UntagResource(Kind::Connector),
            expected: NO_CONTENT,
        },
        Case {
            name: "UntagResource on a challenge",
            op: Op::UntagResource(Kind::Challenge),
            expected: NO_CONTENT,
        },
        Case {
            name: "DeleteChallenge",
            op: Op::DeleteChallenge,
            expected: ACCEPTED,
        },
        Case {
            name: "DeleteConnector",
            op: Op::DeleteConnector,
            expected: ACCEPTED,
        },
    ]
}

/// Assert `answer` is the whole answer a `Unit` output can be: the modelled status, no body,
/// and no error code.
fn assert_unit_answer(answer: &Answer, expected: u16, case: &str) {
    assert_eq!(
        answer.status, expected,
        "case {case:?}: expected the modelled {expected}, got {} / {:?}",
        answer.status, answer.body
    );
    assert!(
        answer.error_type.is_empty(),
        "case {case:?}: expected no error code, got {:?} / {:?}",
        answer.error_type,
        answer.body
    );
    assert!(
        answer.raw.is_empty(),
        "case {case:?}: the output shape is `Unit`, so the body must be empty, got {:?}",
        String::from_utf8_lossy(&answer.raw)
    );
}

#[tokio::test]
async fn a_unit_output_operation_answers_its_modelled_status_with_an_empty_body() {
    for case in cases() {
        let name = case.name;
        let (router, engine) = emulator();
        let connector = active_connector(&router, &engine).await;
        let challenge = new_challenge(&router, &connector).await;

        match case.op {
            Op::TagResource(kind) => {
                let arn = match kind {
                    Kind::Connector => connector.as_str(),
                    Kind::Challenge => challenge.as_str(),
                };
                let tagged = call(
                    &router,
                    "POST",
                    &format!("/tags/{}", label(arn)),
                    Some(json!({ "Tags": tags_member(&[("stage", "one")]) })),
                )
                .await;
                assert_unit_answer(&tagged, case.expected, name);

                let mut expected: Vec<String> = CREATED_TAGS
                    .iter()
                    .map(|(key, _)| (*key).to_string())
                    .chain(["stage".to_string()])
                    .collect();
                expected.sort();
                let keys = listed_tag_keys(&router, arn, name).await;
                assert_eq!(
                    keys, expected,
                    "case {name:?}: the merged map must be what the resource reports, got {keys:?}"
                );
            }
            Op::UntagResource(kind) => {
                let arn = match kind {
                    Kind::Connector => connector.as_str(),
                    Kind::Challenge => challenge.as_str(),
                };
                let path = format!("/tags/{}?tagKeys={REMOVED_KEY}", label(arn));
                let untagged = call(&router, "DELETE", &path, None).await;
                assert_unit_answer(&untagged, case.expected, name);
                let keys = listed_tag_keys(&router, arn, name).await;
                assert_eq!(
                    keys,
                    vec![KEPT_KEY.to_string()],
                    "case {name:?}: only the removed key may be gone, got {keys:?}"
                );

                // `@idempotent`: the same request on a resource that no longer carries the
                // key is the same answer.
                let again = call(&router, "DELETE", &path, None).await;
                assert_unit_answer(&again, case.expected, name);
            }
            Op::DeleteChallenge => {
                let path = format!("/challenges/{}", label(&challenge));
                let deleted = call(&router, "DELETE", &path, None).await;
                assert_unit_answer(&deleted, case.expected, name);

                let read = call(
                    &router,
                    "GET",
                    &format!("/challengePasswords/{}", label(&challenge)),
                    None,
                )
                .await;
                assert_eq!(
                    (read.status, read.error_type.as_str()),
                    (404, "ResourceNotFoundException"),
                    "case {name:?}: the deleted challenge must be gone, got {} / {:?}",
                    read.status,
                    read.body
                );

                // `@idempotent`: the end state the call asks for is already reached.
                let again = call(&router, "DELETE", &path, None).await;
                assert_unit_answer(&again, case.expected, name);
            }
            Op::DeleteConnector => {
                let path = format!("/connectors/{}", label(&connector));
                let deleted = call(&router, "DELETE", &path, None).await;
                assert_unit_answer(&deleted, case.expected, name);

                let read = call(&router, "GET", &path, None).await;
                assert_eq!(
                    read.body["Connector"]["Status"].as_str(),
                    Some(STATUS_DELETING),
                    "case {name:?}: expected Status {STATUS_DELETING} while the teardown is \
                     under way, got {} / {:?}",
                    read.status,
                    read.body
                );

                // `@idempotent`, and then the teardown the clock owes is made due: the
                // connector is gone, which is the effect the 202 accepted.
                let again = call(&router, "DELETE", &path, None).await;
                assert_unit_answer(&again, case.expected, name);
                release_pending(&engine, &connector);
                let gone = call(&router, "GET", &path, None).await;
                assert_eq!(
                    (gone.status, gone.error_type.as_str()),
                    (404, "ResourceNotFoundException"),
                    "case {name:?}: once the teardown is due the connector must answer 404, \
                     got {} / {:?}",
                    gone.status,
                    gone.body
                );
            }
        }
    }
}
