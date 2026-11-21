// SPDX-License-Identifier: CC0-1.0

//! The STATUS and the BODY this service answers a SERVED tag mutation with, end to end over
//! HTTP.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is what
//! a signed AWS SDK request observes, headers included. The resource ARN rides in the URI
//! path as the modelled `@httpLabel` puts it, and `UntagResource`'s keys ride in the query
//! string as its `@httpQuery` list puts them.
//!
//! The rule under test: an operation whose modelled output structure declares NO member
//! leaves its caller nothing to parse, so its success answers HTTP 204 No Content with no
//! body at all, and states the length that nothing has ([`NO_CONTENT_LENGTH`]) — and an answer
//! that DOES carry content keeps the status that says a body follows. `TagResourceResponse` and `UntagResourceResponse` are the two members-less output
//! structures of this model
//! (`smithy_query(query="shape",shape="TagResourceResponse,UntagResourceResponse")`), so the
//! two tagging mutations are the operations the rule reaches here; every other operation's
//! output declares members (a `requestId` at least) and keeps its 200.
//!
//! The rows span what the rule generalises over rather than one recorded call: which mutation
//! is called (so both the POST and the DELETE route), which resource kind the ARN names, how
//! many keys one call carries (one, several, the modelled maximum), whether a key is new,
//! overwrites one already there or is not carried at all, whether an untagging call empties
//! the resource's tags, and whether the call is the first or a repeat. The answer is the same
//! for all of them, which is the point: the status is a property of the OUTPUT SHAPE, not of
//! how much the call changed.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::resource_tags::MAX_RESOURCE_TAGS;
use crate::untag_resource::{emulator, encode_label, request, tag_object, Kind, REGION};

/// The status a served tag mutation answers: its output structure declares no member, so the
/// answer carries nothing to parse.
const NO_CONTENT_STATUS: u16 = 204;

/// The length such an answer states for the body it does not carry, so a client that frames a
/// bodyless message by its stated length reads an explicitly empty one. It is the framing the
/// response the emulator hands out carries — the in-process front door, the inter-service
/// callback path and the SDK connector all serve that response unchanged. What a socket client
/// reads is framed by the layer below this crate, so the socket suite leaves the header to it.
const NO_CONTENT_LENGTH: &str = "0";

/// The status an answer that DOES carry members keeps — every read's and every create's.
const BODY_FOLLOWS_STATUS: u16 = 200;

/// Which tagging mutation a row calls.
#[derive(Debug, Clone)]
enum Mutation {
    /// `TagResource`: `POST /tags/{resourceArn}` with the pairs in the body.
    Tag(Vec<(String, String)>),
    /// `UntagResource`: `DELETE /tags/{resourceArn}?tagKeys=…`, one parameter per key.
    Untag(Vec<String>),
}

/// What one call answered: the status, the length it states, and the body bytes themselves.
#[derive(Debug)]
struct Answer {
    status: u16,
    length: Option<String>,
    body: Vec<u8>,
}

impl Answer {
    /// The body as text, for an assertion message.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// Send one request and report exactly what came back — nothing is parsed, so a body the
/// renderer wrote is visible whatever it holds.
async fn send(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    let response = router.dispatch(request(region, method, uri, body)).await;
    let status = response.status().as_u16();
    let length = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    Answer {
        status,
        length,
        body: response.into_body().to_vec(),
    }
}

/// Call `mutation` on `arn`, in [`REGION`].
async fn mutate(router: &MultiServiceRouter, arn: &str, mutation: &Mutation) -> Answer {
    match mutation {
        Mutation::Tag(pairs) => {
            let borrowed: Vec<(&str, &str)> = pairs
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            send(
                router,
                REGION,
                "POST",
                &format!("/tags/{}", encode_label(arn)),
                Some(&json!({ "tags": tag_object(&borrowed) })),
            )
            .await
        }
        Mutation::Untag(keys) => {
            let query = keys
                .iter()
                .map(|key| format!("tagKeys={}", encode_label(key)))
                .collect::<Vec<String>>()
                .join("&");
            send(
                router,
                REGION,
                "DELETE",
                &format!("/tags/{}?{query}", encode_label(arn)),
                None,
            )
            .await
        }
    }
}

/// Assert `answer` is a served mutation's: the no-content status, no body byte at all, and the
/// length such an answer states.
fn assert_no_content(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status,
        NO_CONTENT_STATUS,
        "case {case:?}: expected HTTP {NO_CONTENT_STATUS}, got {} with {:?}",
        answer.status,
        answer.text()
    );
    assert!(
        answer.body.is_empty(),
        "case {case:?}: expected no body at all, got {:?}",
        answer.text()
    );
    assert_eq!(
        answer.length.as_deref(),
        Some(NO_CONTENT_LENGTH),
        "case {case:?}: expected a stated length of {NO_CONTENT_LENGTH}, got {:?}",
        answer.length
    );
}

/// The pairs a row writes, owned so a row can build as many as it likes.
fn pairs(tags: &[(&str, &str)]) -> Vec<(String, String)> {
    tags.iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// The keys a row removes, owned for the same reason.
fn keys(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

/// One row: a resource kind, the tags its create carried, and the mutation called on it.
struct Row {
    case: &'static str,
    kind: Kind,
    name: &'static str,
    created_with: Option<&'static [(&'static str, &'static str)]>,
    mutation: Mutation,
}

/// The table. See the module docs for the dimensions the rows differ along.
fn rows() -> Vec<Row> {
    // The modelled maximum in ONE call ([`MAX_RESOURCE_TAGS`], the `TagMap` bound this suite
    // takes from the tagging suite rather than restating): a write at the published bound, and
    // the removal of every key it wrote.
    let many: Vec<(String, String)> = (0..MAX_RESOURCE_TAGS)
        .map(|n| (format!("key-{n:02}"), format!("value-{n:02}")))
        .collect();
    let many_keys: Vec<String> = many.iter().map(|(key, _)| key.clone()).collect();

    vec![
        Row {
            case: "one new key written onto a resource that carries none",
            kind: Kind::InfrastructureConfiguration,
            name: "Framing Infra",
            created_with: None,
            mutation: Mutation::Tag(pairs(&[("Owner", "platform")])),
        },
        Row {
            case: "several keys written at once, one of them overwriting",
            kind: Kind::Component,
            name: "Framing Component",
            created_with: Some(&[("Stage", "dev")]),
            mutation: Mutation::Tag(pairs(&[
                ("Stage", "prod"),
                ("Owner", "platform"),
                ("Team", "builders"),
            ])),
        },
        Row {
            case: "a key written with the value it already carries",
            kind: Kind::ImageRecipe,
            name: "Framing Recipe",
            created_with: Some(&[("Owner", "platform")]),
            mutation: Mutation::Tag(pairs(&[("Owner", "platform")])),
        },
        Row {
            case: "the modelled maximum of keys in one write",
            kind: Kind::InfrastructureConfiguration,
            name: "Bounded Framing Infra",
            created_with: None,
            mutation: Mutation::Tag(many.clone()),
        },
        Row {
            case: "one carried key removed, leaving others behind",
            kind: Kind::Component,
            name: "Trimmed Component",
            created_with: Some(&[("Owner", "platform"), ("Stage", "dev")]),
            mutation: Mutation::Untag(keys(&["Stage"])),
        },
        Row {
            case: "every carried key removed, emptying the resource's tags",
            kind: Kind::ImageRecipe,
            name: "Emptied Framing Recipe",
            created_with: Some(&[("Owner", "platform"), ("Stage", "dev")]),
            mutation: Mutation::Untag(keys(&["Owner", "Stage"])),
        },
        Row {
            case: "a key the resource does not carry, so nothing is removed",
            kind: Kind::InfrastructureConfiguration,
            name: "Untouched Framing Infra",
            created_with: Some(&[("Owner", "platform")]),
            mutation: Mutation::Untag(keys(&["Absent"])),
        },
        Row {
            case: "a carried key and an absent one named together",
            kind: Kind::Component,
            name: "Mixed Framing Component",
            created_with: Some(&[("Owner", "platform"), ("Stage", "dev")]),
            mutation: Mutation::Untag(keys(&["Stage", "never-set"])),
        },
        Row {
            // `TagKeyList` carries the same maximum as `TagMap`
            // (`smithy_query(query="operation",operation="UntagResource")` reports
            // `tagKeys … len 1..50`), so this is the removal at the published bound.
            case: "the modelled maximum of keys named in one removal",
            kind: Kind::ImageRecipe,
            name: "Bounded Framing Recipe",
            created_with: None,
            mutation: Mutation::Untag(many_keys),
        },
    ]
}

#[tokio::test]
async fn a_served_tag_mutation_answers_no_content_with_no_body() {
    for row in rows() {
        let router = emulator();
        let arn = row
            .kind
            .create(&router, REGION, row.name, row.created_with)
            .await;

        let answer = mutate(&router, &arn, &row.mutation).await;
        assert_no_content(row.case, &answer);

        // The same call again: a mutation that changed nothing the second time answers
        // exactly as the first, because the status follows the output SHAPE and not how much
        // the call changed.
        let repeated = mutate(&router, &arn, &row.mutation).await;
        assert_no_content(&format!("{}, repeated", row.case), &repeated);
    }
}

/// The same no-content answer, delivered over a real socket: the status, no body, and no
/// chunked framing standing in for a stated length.
///
/// The length the answer states is left to the layer that owns the transport, so this case
/// does not pin it: it asserts what belongs to this service — the status and the absent body —
/// on the hop a socket client really takes, while the table above pins the answer the emulator
/// hands out.
#[tokio::test(flavor = "multi_thread")]
async fn the_no_content_answer_reaches_a_socket_client_with_no_body() {
    let router = emulator();
    let arn = Kind::Component
        .create(&router, REGION, "Socket Framing Component", None)
        .await;

    let (endpoint, _served) = emulator_server::serve_on_ephemeral_port(router).await;
    let port: u16 = endpoint
        .rsplit(':')
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("an endpoint with a port, got {endpoint:?}"));

    // Both mutations, each on its own method and binding.
    let cases: Vec<(&str, &str, String, Option<String>)> = vec![
        (
            "TagResource",
            "POST",
            format!("/tags/{}", encode_label(&arn)),
            Some(json!({ "tags": { "Owner": "platform" } }).to_string()),
        ),
        (
            "UntagResource",
            "DELETE",
            format!("/tags/{}?tagKeys=Owner", encode_label(&arn)),
            None,
        ),
    ];

    for (case, method, target, payload) in cases {
        let answered = tokio::task::spawn_blocking(move || {
            raw_exchange(port, method, &target, payload.as_deref())
        })
        .await
        .expect("the socket exchange runs to completion");

        assert!(
            answered.starts_with(&format!("HTTP/1.1 {NO_CONTENT_STATUS}")),
            "case {case:?} on a socket: expected a {NO_CONTENT_STATUS}, got {answered:?}"
        );
        let (head, body) = answered
            .split_once("\r\n\r\n")
            .unwrap_or_else(|| panic!("case {case:?}: a response head and body, got {answered:?}"));
        assert!(
            body.is_empty(),
            "case {case:?} on a socket: expected no body, got {body:?}"
        );
        assert!(
            !head.to_ascii_lowercase().contains("transfer-encoding"),
            "case {case:?} on a socket: expected the answer not to be chunked, got {head:?}"
        );
    }
}

/// One request written straight onto a socket, with everything the server answered read back
/// as text.
///
/// `connection: close` so the server closes the socket after answering and the read below ends
/// on EOF rather than on a parse of the very framing this case is about.
fn raw_exchange(port: u16, method: &str, target: &str, payload: Option<&str>) -> String {
    use std::io::{Read, Write};

    let mut stream =
        std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to the served port");
    let body = payload.unwrap_or_default();
    let request = format!(
        "{method} {target} HTTP/1.1\r\n\
         host: imagebuilder.{REGION}.amazonaws.com\r\n\
         authorization: AWS4-HMAC-SHA256 \
         Credential=AKIDIMAGEBUILDER/20260101/{REGION}/imagebuilder/aws4_request, \
         SignedHeaders=host, Signature=0000\r\n\
         content-type: application/json\r\n\
         content-length: {}\r\n\
         connection: close\r\n\
         \r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .expect("write the request");
    let mut answer = Vec::new();
    stream
        .read_to_end(&mut answer)
        .expect("read the whole answer");
    String::from_utf8_lossy(&answer).to_string()
}

#[tokio::test]
async fn an_answer_that_carries_members_keeps_the_status_that_says_so() {
    let router = emulator();
    // The create itself is the first such answer: its output declares members, so it carries
    // a body at 200.
    let arn = Kind::Component
        .create(&router, REGION, "Reported Component", Some(&[("Owner", "platform")]))
        .await;

    // The tag listing and the resource's own read both declare members, and both answer with
    // a body — the tags the mutations above leave are still readable, so 204 is the mutation's
    // answer and not the service's.
    let reads = [
        (
            "ListTagsForResource",
            format!("/tags/{}", encode_label(&arn)),
        ),
        (
            "GetComponent",
            format!("/GetComponent?componentBuildVersionArn={arn}"),
        ),
    ];
    for (case, uri) in reads {
        let answer = send(&router, REGION, "GET", &uri, None).await;
        assert_eq!(
            answer.status,
            BODY_FOLLOWS_STATUS,
            "case {case:?}: expected HTTP {BODY_FOLLOWS_STATUS}, got {} with {:?}",
            answer.status,
            answer.text()
        );
        assert!(
            !answer.body.is_empty(),
            "case {case:?}: expected a body to parse, got none"
        );
        assert_ne!(
            answer.length.as_deref(),
            Some(NO_CONTENT_LENGTH),
            "case {case:?}: an answer with a body must not be framed as empty"
        );
    }
}

#[tokio::test]
async fn a_refused_tag_mutation_keeps_its_own_status_and_its_error_body() {
    let router = emulator();
    // A resource exists, so the refusals below are about the ARN in the request.
    Kind::Component
        .create(&router, REGION, "Present Framing Component", None)
        .await;

    // An ARN this Region holds no resource under: neither mutation is served, so neither
    // answers the no-content status, and each carries the error shape for a client to read.
    let absent = format!("arn:aws:imagebuilder:{REGION}:000000000000:component/absent/1.0.0/1");
    let refusals = [
        (
            "TagResource on an ARN nothing holds",
            Mutation::Tag(pairs(&[("Owner", "platform")])),
        ),
        (
            "UntagResource on an ARN nothing holds",
            Mutation::Untag(keys(&["Owner"])),
        ),
    ];
    for (case, mutation) in refusals {
        let answer = mutate(&router, &absent, &mutation).await;
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected HTTP 404, got {} with {:?}",
            answer.status,
            answer.text()
        );
        let body: Value = serde_json::from_slice(&answer.body).unwrap_or_else(|_| {
            panic!(
                "case {case:?}: expected a JSON error body, got {:?}",
                answer.text()
            )
        });
        let shape = body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected an error shape in {body}"))
            .rsplit(['#', '/'])
            .next()
            .expect("a non-empty error type")
            .to_string();
        assert_eq!(
            shape, "ResourceNotFoundException",
            "case {case:?}: expected the error shape ResourceNotFoundException, got {shape:?}"
        );
    }
}
