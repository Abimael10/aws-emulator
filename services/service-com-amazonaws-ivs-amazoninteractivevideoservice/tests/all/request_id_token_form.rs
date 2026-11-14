// SPDX-License-Identifier: CC0-1.0

//! The FORM of the id every IVS response names the request it answers by, driven
//! end-to-end over the router the production entrypoint serves.
//!
//! The rule under test, stated once and then exercised over a SET of inputs: this
//! endpoint names the request a response answers by a fixed-width base64 token — 16
//! characters, 15 of the standard base64 alphabet followed by the single `=` that padding
//! an 11-byte value needs — in `x-amzn-requestid` on EVERY response, and a refusal repeats
//! that same token under its error document's `RequestID` member. The token's value is
//! per-response (no two responses name the same request) and nothing a client may depend
//! on; its width and alphabet are the wire contract, which is what anything that
//! length-checks, parses, logs or quotes the id reads.
//!
//! The rows differ along what the rule generalises over: the HTTP status class (a 200 with
//! a body, an empty 204, and 400/402/404 refusals), the error class the refusal carries
//! (`ValidationException`, `ResourceNotFoundException`, `ServiceQuotaExceededException`,
//! `ChannelNotBroadcasting`, and the tagging surface's `NotFoundException`, which the
//! operations' own model does not declare), the channel the failure travels (a constraint
//! refused at the request boundary, a modelled failure a handler states, a code outside
//! the operation's model), the operation, and the member the request carries.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ivs_amazoninteractivevideoservice::EmulatedService;

/// The region this suite signs for, and the account every emulator ARN carries.
const REGION: &str = "us-west-2";
const ACCOUNT: &str = "000000000000";

/// The header this protocol names the request a response answers in, and the error
/// document member a refusal repeats it under.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";
const REQUEST_ID_KEY: &str = "RequestID";

/// The error document member a refusal states its code under.
const CODE_KEY: &str = "Code";

/// The form the rule fixes: how wide the token is, how many of its characters are payload,
/// and how many opaque bytes that payload decodes back to.
const TOKEN_CHARACTERS: usize = 16;
const TOKEN_PAYLOAD_CHARACTERS: usize = TOKEN_CHARACTERS - 1;
const TOKEN_BYTES: usize = 11;

/// One response, as a client sees it.
struct Answer {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Value,
}

impl Answer {
    /// A body member as text, or `"<absent>"` so a failing assertion says which member was
    /// missing rather than panicking inside the reader.
    fn member(&self, key: &str) -> &str {
        self.body
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("<absent>")
    }

    /// A response header, or `"<absent>"`.
    fn header(&self, name: &str) -> &str {
        self.headers.get(name).map_or("<absent>", String::as_str)
    }
}

/// One emulator holding only this service, driven through the router that serves
/// production traffic.
struct Ivs {
    router: MultiServiceRouter,
}

impl Ivs {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Pin authentication off so the suite does not depend on the process setting.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// One request over the wire, signed with a credential scope naming `ivs` in
    /// [`REGION`] as an SDK would.
    async fn call(&self, method: &str, path: &str, body: Option<&str>) -> Answer {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/ivs/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=00"
        );
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://ivs.{REGION}.amazonaws.com{path}"))
            .header("authorization", authorization)
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(body.unwrap_or_default().to_owned()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let headers: BTreeMap<String, String> = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    String::from_utf8_lossy(value.as_bytes()).to_string(),
                )
            })
            .collect();
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()))
        };
        Answer {
            status,
            headers,
            body,
        }
    }

    /// One of the operations the model binds to `POST /<Operation>`.
    async fn post(&self, operation: &str, body: &Value) -> Answer {
        self.call("POST", &format!("/{operation}"), Some(&body.to_string()))
            .await
    }
}

/// A well-formed IVS ARN of `resource_type` naming a resource this store never minted.
///
/// Every modelled IVS ARN pattern pins the region to `[a-z0-9-]+`, the account to `[0-9]+`
/// and the id to `[a-zA-Z0-9-]+`, so an ARN built this way reaches the handler rather than
/// the constraint walk.
fn absent_arn(resource_type: &str, id: &str) -> String {
    format!("arn:aws:ivs:{REGION}:{ACCOUNT}:{resource_type}/{id}")
}

/// Percent-encode one `@httpLabel` value the way an SDK encodes a non-greedy label, so a
/// whole ARN is ONE path segment.
fn encode_path_label(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Whether `id` takes the token form the rule fixes, read without restating the
/// generator: [`TOKEN_CHARACTERS`] characters, the last of them the one `=` pad, the rest
/// of the standard base64 alphabet, and a payload that decodes back to exactly
/// [`TOKEN_BYTES`] bytes. The reason it does not, for an assertion that says what was
/// wrong; `None` when it does.
///
/// This is the ONE reading of that form in this crate's suites: `error_envelope`'s
/// `is_request_id_shaped` — which `not_found_envelope` reads a refusal's document by —
/// answers from it, so the two cannot drift apart.
pub(crate) fn token_form_violation(id: &str) -> Option<String> {
    if id.chars().count() != TOKEN_CHARACTERS {
        return Some(format!("expected {TOKEN_CHARACTERS} characters"));
    }
    let Some(payload) = id.strip_suffix('=') else {
        return Some("expected one trailing '=' pad".to_string());
    };
    if payload.len() != TOKEN_PAYLOAD_CHARACTERS {
        return Some(format!(
            "expected {TOKEN_PAYLOAD_CHARACTERS} payload characters"
        ));
    }
    if !payload
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/')
    {
        return Some("expected standard base64-alphabet characters".to_string());
    }
    match aws_smithy_types::base64::decode(id) {
        Ok(bytes) if bytes.len() == TOKEN_BYTES => None,
        Ok(bytes) => Some(format!(
            "expected {TOKEN_BYTES} opaque bytes, decoded {}",
            bytes.len()
        )),
        Err(error) => Some(format!("expected a decodable token, got {error}")),
    }
}

/// The rule over ONE response: the header names the request by a token of that form, and a
/// refusal's document repeats the very same token. Returns the token, so a caller can
/// compare the ids of a whole run.
fn assert_names_its_request(case: &str, answer: &Answer) -> String {
    let header = answer.header(REQUEST_ID_HEADER).to_string();
    assert!(
        token_form_violation(&header).is_none(),
        "case {case:?}: {REQUEST_ID_HEADER} must take the {TOKEN_CHARACTERS}-character \
         base64 token form ({}), got {header:?} in {:?}",
        token_form_violation(&header).unwrap_or_default(),
        answer.headers
    );
    if answer.body.get(REQUEST_ID_KEY).is_some() {
        let stated = answer.member(REQUEST_ID_KEY);
        assert!(
            token_form_violation(stated).is_none(),
            "case {case:?}: the document's {REQUEST_ID_KEY} must take the \
             {TOKEN_CHARACTERS}-character base64 token form ({}), got {stated:?} in {}",
            token_form_violation(stated).unwrap_or_default(),
            answer.body
        );
        assert_eq!(
            stated, header,
            "case {case:?}: the document and {REQUEST_ID_HEADER} must name the same \
             request, got {stated:?} and {header:?}"
        );
    }
    header
}

/// One row of the table: the call to make, and the status and error code the service
/// answers it with.
type Row = (
    &'static str,
    &'static str,
    Value,
    u16,
    Option<&'static str>,
);

/// The rule over every class of response this service answers, including each of the
/// refusals the recorded error-path traces exercise.
#[tokio::test]
async fn every_response_names_its_request_by_a_base64_token() {
    let ivs = Ivs::new();
    let absent_ad = absent_arn("ad-configuration", "neverCreated");
    let absent_recording = absent_arn("recording-configuration", "neverCreated");
    let absent_channel = absent_arn("channel", "neverCreated");
    // A channel that exists and is idle; creating it also mints its one stream key, so a
    // second `CreateStreamKey` meets the per-channel quota.
    let created = ivs
        .post("CreateChannel", &json!({ "name": "requestIds" }))
        .await;
    assert_eq!(
        created.status, 200,
        "the fixture channel must be created, got {} {}",
        created.status, created.body
    );
    let idle = created.body["channel"]["arn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let rows: Vec<Row> = vec![
        (
            "a success with a body",
            "GetChannel",
            json!({ "arn": idle }),
            200,
            None,
        ),
        (
            "a list, which answers a body of its own",
            "ListChannels",
            json!({}),
            200,
            None,
        ),
        (
            "an ad configuration refused at the request boundary",
            "CreateAdConfiguration",
            json!({ "name": "ads",
                    "mediaTailorPlaybackConfigurations": [
                        { "playbackConfigurationArn": "not-an-arn" }] }),
            400,
            Some("ValidationException"),
        ),
        (
            "an ad configuration this store does not hold, read",
            "GetAdConfiguration",
            json!({ "arn": absent_ad }),
            404,
            Some("ResourceNotFoundException"),
        ),
        (
            "an ad configuration this store does not hold, deleted",
            "DeleteAdConfiguration",
            json!({ "arn": absent_ad }),
            404,
            Some("ResourceNotFoundException"),
        ),
        (
            "a recording configuration this store does not hold, read",
            "GetRecordingConfiguration",
            json!({ "arn": absent_recording }),
            404,
            Some("ResourceNotFoundException"),
        ),
        (
            "a recording configuration this store does not hold, deleted",
            "DeleteRecordingConfiguration",
            json!({ "arn": absent_recording }),
            404,
            Some("ResourceNotFoundException"),
        ),
        (
            "a channel this store does not hold, read",
            "GetChannel",
            json!({ "arn": absent_channel }),
            404,
            Some("ResourceNotFoundException"),
        ),
        (
            "a quota a handler states",
            "CreateStreamKey",
            json!({ "channelArn": idle }),
            402,
            Some("ServiceQuotaExceededException"),
        ),
        (
            "a state a handler states",
            "GetStream",
            json!({ "channelArn": idle }),
            404,
            Some("ChannelNotBroadcasting"),
        ),
    ];

    let mut named: Vec<(String, String)> = Vec::new();
    named.push((
        "the fixture create".to_string(),
        assert_names_its_request("the fixture create", &created),
    ));
    for (case, operation, request, status, code) in rows {
        let answer = ivs.post(operation, &request).await;
        assert_eq!(
            answer.status, status,
            "case {case:?}: expected HTTP {status}, got {} {}",
            answer.status, answer.body
        );
        if let Some(code) = code {
            assert_eq!(
                answer.member(CODE_KEY),
                code,
                "case {case:?}: expected the document's {CODE_KEY} to be {code:?}, got {}",
                answer.body
            );
        }
        named.push((case.to_string(), assert_names_its_request(case, &answer)));
    }

    // A code the operations' own model does not declare, answered by the tagging surface,
    // and an empty 204 success: two responses the rows above cannot send.
    let tagging = ivs
        .call(
            "GET",
            &format!("/tags/{}", encode_path_label(&absent_channel)),
            None,
        )
        .await;
    assert_eq!(
        tagging.status, 404,
        "the tagging read of an absent ARN must answer 404, got {} {}",
        tagging.status, tagging.body
    );
    named.push((
        "a code outside the operation's model".to_string(),
        assert_names_its_request("a code outside the operation's model", &tagging),
    ));
    let deleted = ivs.post("DeleteChannel", &json!({ "arn": idle })).await;
    assert_eq!(
        deleted.status, 204,
        "the delete must answer an empty 204, got {} {}",
        deleted.status, deleted.body
    );
    named.push((
        "an empty 204 success".to_string(),
        assert_names_its_request("an empty 204 success", &deleted),
    ));

    for (left, (case, id)) in named.iter().enumerate() {
        for (other_case, other_id) in named.iter().skip(left + 1) {
            assert_ne!(
                id, other_id,
                "case {case:?} and case {other_case:?}: two responses must not name the \
                 same request, both named {id:?}"
            );
        }
    }
}

/// The same refusal, repeated: the FORM holds on every call while the VALUE never does, so
/// nothing a client reads can depend on a particular id.
#[tokio::test]
async fn repeating_one_refusal_names_a_fresh_request_each_time() {
    let ivs = Ivs::new();
    let absent = absent_arn("recording-configuration", "repeated");
    let mut named: Vec<String> = Vec::new();
    for round in 0..8 {
        let answer = ivs
            .post("GetRecordingConfiguration", &json!({ "arn": absent }))
            .await;
        assert_eq!(
            answer.status, 404,
            "round {round}: expected HTTP 404, got {} {}",
            answer.status, answer.body
        );
        named.push(assert_names_its_request(&format!("round {round}"), &answer));
    }
    let distinct: std::collections::BTreeSet<&String> = named.iter().collect();
    assert_eq!(
        distinct.len(),
        named.len(),
        "expected {} refusals to name {} distinct requests, got {} (ids {named:?})",
        named.len(),
        named.len(),
        distinct.len()
    );
}
