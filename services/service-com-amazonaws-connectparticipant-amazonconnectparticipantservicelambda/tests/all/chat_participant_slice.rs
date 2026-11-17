// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the chat-participant slice: `CreateParticipantConnection`,
//! `SendMessage`, `SendEvent`, `GetTranscript` and `DisconnectParticipant`.
//!
//! Every case goes over the wire, through the same `MultiServiceRouter` the production
//! router dispatches with, so the HTTP status, the restJson1 envelope and the stored
//! state are all exercised together.

use std::sync::Arc;
use std::time::SystemTime;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_connectparticipant_amazonconnectparticipantservicelambda as participant;
use smithy_server_runtime::MultiServiceRouter;

/// The endpoint host the requests address, and the host the websocket URL is reported on.
const HOST: &str = "participant.connect.us-east-1.amazonaws.com";

/// A SigV4 credential scope naming this service's signing name, which is what the
/// router resolves a request's target service from.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/\
                             execute-api/aws4_request, SignedHeaders=host, Signature=00";

/// The chat display name a started chat registers its participant with; the one source of
/// a session's display name is the chat that started it.
const DISPLAY_NAME: &str = "sim-customer";

/// A router with just this service mounted on one shared engine, plus that engine: the
/// store a STARTED chat is registered in.
///
/// A participant token is issued where a chat is started, so a test that connects one
/// registers the chat first ([`Harness::start_chat`]) exactly as `StartChatContact` does.
/// The harness derefs to the router, so a request is dispatched through it directly.
struct Harness {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl std::ops::Deref for Harness {
    type Target = MultiServiceRouter;

    fn deref(&self) -> &MultiServiceRouter {
        &self.router
    }
}

impl Harness {
    /// This service's store in the shared engine.
    fn store(
        &self,
    ) -> ::engine::ServiceState<
        participant::state::ComAmazonawsConnectparticipantAmazonconnectparticipantservicelambdaState,
    > {
        participant::state::service_state(&*self.engine).expect("the connectparticipant store")
    }

    /// Register the chat a `StartChatContact` would have started for `participant_token`,
    /// and answer its contact id.
    ///
    /// Registering one token twice keeps the session the first call registered, because a
    /// participant token names one participant of one contact.
    fn start_chat(&self, participant_token: &str) -> String {
        let store = self.store();
        let mut state = store.lock();
        if let Some(contact_id) = state.participant_tokens.get(participant_token) {
            return contact_id.clone();
        }
        let session = participant::state::ChatSession::started(
            participant::state::new_id(),
            participant::state::new_id(),
            Some(DISPLAY_NAME.to_string()),
            SystemTime::now() + participant::state::CONNECTION_TOKEN_TTL,
        );
        let contact_id = session.contact_id.clone();
        state.register_session(participant_token.to_string(), session);
        contact_id
    }
}

/// A harness with just this service mounted on one shared engine.
fn router() -> Harness {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(participant::EmulatedService::new(engine.clone()));
    Harness { router, engine }
}

/// POST `path` with an optional `X-Amz-Bearer` token, returning the status and the
/// decoded JSON body (`Value::Null` for an empty body).
async fn post(
    router: &MultiServiceRouter,
    path: &str,
    bearer: Option<&str>,
    body: &Value,
) -> (u16, Value) {
    let mut request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{HOST}{path}"))
        .header("host", HOST)
        .header("content-type", "application/json")
        .header("authorization", AUTHORIZATION);
    if let Some(bearer) = bearer {
        request = request.header("x-amz-bearer", bearer);
    }
    let request = request
        .body(Bytes::from(
            serde_json::to_vec(body).expect("serialize the request body"),
        ))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let decoded = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, decoded)
}

/// The `__type` discriminator of a restJson1 error body.
fn error_type(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .to_string()
}

/// The `Message` of a restJson1 error body.
fn error_message(body: &Value) -> String {
    body.get("Message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("<no Message>")
        .to_string()
}

/// Assert `value` is an `ISO8601Datetime` / `Instant`: `yyyy-MM-ddThh:mm:ss.SSSZ`,
/// the form every timestamp in this model documents (e.g. `2019-11-08T02:41:28.172Z`).
///
/// Checked here independently of the service's own formatter.
fn assert_iso8601_millis(case: &str, value: &str) {
    assert_eq!(
        value.len(),
        24,
        "case {case:?}: expected a 24-character yyyy-MM-ddThh:mm:ss.SSSZ timestamp, got {value:?}"
    );
    let chars: Vec<char> = value.chars().collect();
    for (at, expected) in [(4, '-'), (7, '-'), (10, 'T'), (13, ':'), (16, ':'), (19, '.'), (23, 'Z')]
    {
        assert_eq!(
            chars[at], expected,
            "case {case:?}: expected {expected:?} at index {at} of {value:?}, got {:?}",
            chars[at]
        );
    }
    for at in [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18, 20, 21, 22] {
        assert!(
            chars[at].is_ascii_digit(),
            "case {case:?}: expected a digit at index {at} of {value:?}, got {:?}",
            chars[at]
        );
    }
}

/// Start the chat `participant_token` belongs to, open a connection for it and return its
/// `ConnectionToken`.
async fn connect(harness: &Harness, participant_token: &str) -> String {
    harness.start_chat(participant_token);
    let (status, body) = post(
        harness,
        "/participant/connection",
        Some(participant_token),
        &json!({"Type": ["CONNECTION_CREDENTIALS"]}),
    )
    .await;
    assert_eq!(status, 200, "CreateParticipantConnection: got {body:?}");
    body["ConnectionCredentials"]["ConnectionToken"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateParticipantConnection returned no ConnectionToken: {body:?}"))
        .to_string()
}

/// Send `content` as a message and return its `Id`.
async fn send_message(router: &MultiServiceRouter, connection_token: &str, content: &str) -> String {
    let (status, body) = post(
        router,
        "/participant/message",
        Some(connection_token),
        &json!({"ContentType": "text/plain", "Content": content}),
    )
    .await;
    assert_eq!(status, 200, "SendMessage {content:?}: got {body:?}");
    body["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("SendMessage returned no Id: {body:?}"))
        .to_string()
}

/// The `Content` a message receipt carries: `SendEventRequest$Content` documents receipt
/// content as the JSON string `{"messageId":"<chat item id>"}`.
fn receipt_content(message_id: &str) -> String {
    json!({"messageId": message_id}).to_string()
}

/// One `GetTranscript` page: the items and the continuation token.
async fn get_transcript(
    router: &MultiServiceRouter,
    connection_token: &str,
    request: &Value,
) -> (Vec<Value>, Option<String>) {
    let (status, body) = post(router, "/participant/transcript", Some(connection_token), request).await;
    assert_eq!(status, 200, "GetTranscript {request}: got {body:?}");
    let items = body["Transcript"]
        .as_array()
        .unwrap_or_else(|| panic!("GetTranscript returned no Transcript: {body:?}"))
        .clone();
    let next = body
        .get("NextToken")
        .and_then(Value::as_str)
        .map(str::to_string);
    (items, next)
}

#[tokio::test]
async fn a_sent_message_comes_back_from_the_transcript_that_stored_it() {
    let router = router();
    router.start_chat("participant-token-1");
    let (status, body) = post(
        &router,
        "/participant/connection",
        Some("participant-token-1"),
        &json!({"Type": ["CONNECTION_CREDENTIALS", "WEBSOCKET"], "ConnectParticipant": true}),
    )
    .await;
    assert_eq!(status, 200, "CreateParticipantConnection: got {body:?}");

    let connection_token = body["ConnectionCredentials"]["ConnectionToken"]
        .as_str()
        .unwrap_or_else(|| panic!("no ConnectionToken: {body:?}"));
    assert!(
        !connection_token.is_empty(),
        "case credentials: expected a non-empty ConnectionToken, got {connection_token:?}"
    );
    let expiry = body["ConnectionCredentials"]["Expiry"]
        .as_str()
        .unwrap_or_else(|| panic!("no Expiry: {body:?}"));
    assert_iso8601_millis("ConnectionCredentials.Expiry", expiry);
    let now = participant::state::format_instant(SystemTime::now());
    assert!(
        expiry > now.as_str(),
        "case credentials: expected Expiry after {now:?}, got {expiry:?}"
    );

    let url = body["Websocket"]["Url"]
        .as_str()
        .unwrap_or_else(|| panic!("no Websocket.Url: {body:?}"));
    assert!(
        url.starts_with(&format!("wss://{HOST}/")),
        "case websocket: expected a wss:// URL on {HOST}, got {url:?}"
    );
    let connection_expiry = body["Websocket"]["ConnectionExpiry"]
        .as_str()
        .unwrap_or_else(|| panic!("no Websocket.ConnectionExpiry: {body:?}"));
    assert_iso8601_millis("Websocket.ConnectionExpiry", connection_expiry);
    assert!(
        connection_expiry > now.as_str(),
        "case websocket: expected ConnectionExpiry after {now:?}, got {connection_expiry:?}"
    );

    let (status, sent) = post(
        &router,
        "/participant/message",
        Some(connection_token),
        &json!({"ContentType": "text/plain", "Content": "hello", "ClientToken": "ct-1"}),
    )
    .await;
    assert_eq!(status, 200, "SendMessage: got {sent:?}");
    let message_id = sent["Id"].as_str().expect("SendMessage Id").to_string();
    let message_time = sent["AbsoluteTime"]
        .as_str()
        .expect("SendMessage AbsoluteTime")
        .to_string();
    assert_iso8601_millis("SendMessage.AbsoluteTime", &message_time);

    let (status, event) = post(
        &router,
        "/participant/event",
        Some(connection_token),
        &json!({"ContentType": "application/vnd.amazonaws.connect.event.typing"}),
    )
    .await;
    assert_eq!(status, 200, "SendEvent: got {event:?}");
    let event_id = event["Id"].as_str().expect("SendEvent Id").to_string();
    assert_iso8601_millis(
        "SendEvent.AbsoluteTime",
        event["AbsoluteTime"].as_str().expect("SendEvent AbsoluteTime"),
    );

    // Read it back: the transcript is what was sent, not a canned body. A typing event is
    // transient, so the message is the only item in it.
    let (status, transcript) = post(
        &router,
        "/participant/transcript",
        Some(connection_token),
        &json!({"SortOrder": "ASCENDING", "ScanDirection": "FORWARD"}),
    )
    .await;
    assert_eq!(status, 200, "GetTranscript: got {transcript:?}");
    let items = transcript["Transcript"].as_array().expect("Transcript");
    assert_eq!(
        items.len(),
        1,
        "case round trip: expected the one sent message, got {items:?}"
    );
    assert!(
        items
            .iter()
            .all(|item| item["Id"].as_str() != Some(event_id.as_str())),
        "case round trip: the typing event must not be a transcript item, got {items:?}"
    );
    assert_eq!(
        items[0]["Id"].as_str(),
        Some(message_id.as_str()),
        "case round trip: expected the message, got {items:?}"
    );
    assert_eq!(
        items[0]["Content"].as_str(),
        Some("hello"),
        "case round trip: expected the stored Content, got {items:?}"
    );
    assert_eq!(
        items[0]["ContentType"].as_str(),
        Some("text/plain"),
        "case round trip: expected the stored ContentType, got {items:?}"
    );
    assert_eq!(
        items[0]["AbsoluteTime"].as_str(),
        Some(message_time.as_str()),
        "case round trip: expected the AbsoluteTime SendMessage reported, got {items:?}"
    );
    assert_eq!(
        items[0]["Type"].as_str(),
        Some("MESSAGE"),
        "case round trip: expected a MESSAGE item, got {items:?}"
    );
    assert_eq!(
        items[0]["ParticipantRole"].as_str(),
        Some("CUSTOMER"),
        "case round trip: expected the session's ParticipantRole, got {items:?}"
    );
    let participant_id = items[0]["ParticipantId"].as_str().unwrap_or_default();
    assert!(
        !participant_id.is_empty(),
        "case round trip: expected the item attributed to a participant, got {items:?}"
    );
    assert!(
        items[0].get("MessageMetadata").is_none(),
        "case round trip: a message nobody acknowledged carries no MessageMetadata, got {items:?}"
    );
    let initial_contact_id = transcript["InitialContactId"].as_str().unwrap_or_default();
    assert!(
        !initial_contact_id.is_empty(),
        "case round trip: expected InitialContactId, got {transcript:?}"
    );
    assert_eq!(
        items[0]["ContactId"].as_str(),
        Some(initial_contact_id),
        "case round trip: expected the item's ContactId to be the session's, got {items:?}"
    );
}

#[tokio::test]
async fn only_the_requested_connection_types_are_returned() {
    let router = router();
    // `Type` is a list of `ConnectionType`; its value set is CONNECTION_CREDENTIALS,
    // WEBSOCKET and WEBRTC_CONNECTION (smithy_query shape=ConnectionType).
    let cases: &[(&[&str], bool, bool)] = &[
        (&["CONNECTION_CREDENTIALS"], true, false),
        (&["WEBSOCKET"], false, true),
        (&["CONNECTION_CREDENTIALS", "WEBSOCKET"], true, true),
        (&["WEBSOCKET", "CONNECTION_CREDENTIALS"], true, true),
    ];
    for (index, (types, credentials, websocket)) in cases.iter().enumerate() {
        let case = format!("{types:?}");
        let participant_token = format!("participant-token-types-{index}");
        router.start_chat(&participant_token);
        let (status, body) = post(
            &router,
            "/participant/connection",
            Some(&participant_token),
            &json!({"Type": types}),
        )
        .await;
        assert_eq!(status, 200, "case {case}: got {body:?}");
        assert_eq!(
            body.get("ConnectionCredentials").is_some(),
            *credentials,
            "case {case}: ConnectionCredentials presence wrong, got {body:?}"
        );
        assert_eq!(
            body.get("Websocket").is_some(),
            *websocket,
            "case {case}: Websocket presence wrong, got {body:?}"
        );
        assert!(
            body.get("WebRTCConnection").is_none(),
            "case {case}: expected no WebRTCConnection, got {body:?}"
        );
    }

    // An absent `Type` asks for no connection information at all, so neither member is
    // returned. (An EMPTY list asks for connection information of no kind, which the
    // service refuses with its own sentence — see below.)
    router.start_chat("participant-token-no-type");
    let (status, body) = post(
        &router,
        "/participant/connection",
        Some("participant-token-no-type"),
        &json!({"ConnectParticipant": true}),
    )
    .await;
    assert_eq!(status, 200, "case absent Type: got {body:?}");
    assert!(
        body.get("ConnectionCredentials").is_none() && body.get("Websocket").is_none(),
        "case absent Type: expected no connection information, got {body:?}"
    );

    // WEBRTC_CONNECTION's response is Chime SDK media-session data this emulator cannot
    // produce, so it is answered with the scaffold's 501 rather than invented values.
    router.start_chat("participant-token-webrtc");
    let (status, body) = post(
        &router,
        "/participant/connection",
        Some("participant-token-webrtc"),
        &json!({"Type": ["WEBRTC_CONNECTION"]}),
    )
    .await;
    assert_eq!(status, 501, "case WEBRTC_CONNECTION: got {body:?}");
    assert_eq!(
        error_type(&body),
        "NotImplemented",
        "case WEBRTC_CONNECTION: got {body:?}"
    );

    // A value outside the ConnectionType set is a client error, not a silent no-op.
    router.start_chat("participant-token-bogus-type");
    let (status, body) = post(
        &router,
        "/participant/connection",
        Some("participant-token-bogus-type"),
        &json!({"Type": ["TELEPATHY"]}),
    )
    .await;
    assert_eq!(status, 400, "case TELEPATHY: got {body:?}");
    assert_eq!(
        error_type(&body),
        "ValidationException",
        "case TELEPATHY: got {body:?}"
    );
}

/// A request that asks for connection information of no kind at all is refused with the
/// service's own sentence about the missing connection type, whatever else the request
/// carries — fixed text, with nothing of the request in it.
#[tokio::test]
async fn an_empty_connection_type_list_is_refused_with_the_services_own_sentence() {
    let router = router();
    router.start_chat("participant-token-empty-type");

    // Rows differ in what the request carries BESIDE the empty list: the member is the
    // same, so the refusal is.
    let cases: &[(&str, Value)] = &[
        ("an empty Type alone", json!({"Type": []})),
        (
            "an empty Type with ConnectParticipant",
            json!({"Type": [], "ConnectParticipant": true}),
        ),
        (
            "an empty Type with ConnectParticipant false",
            json!({"Type": [], "ConnectParticipant": false}),
        ),
    ];
    for (case, body) in cases {
        let (status, response) = post(
            &router,
            "/participant/connection",
            Some("participant-token-empty-type"),
            body,
        )
        .await;
        assert_eq!(status, 400, "case {case:?}: got {response:?}");
        assert_eq!(
            (
                error_type(&response).as_str(),
                error_message(&response).as_str()
            ),
            (
                "ValidationException",
                "Failed to create participant connection due to missing connection type"
            ),
            "case {case:?}: got {response:?}"
        );
    }

    // The constraint report the framework would otherwise render — the member path, the
    // submitted size and the bound — is what this refusal replaces.
    let (_, response) = post(
        &router,
        "/participant/connection",
        Some("participant-token-empty-type"),
        &json!({"Type": []}),
    )
    .await;
    let message = error_message(&response);
    for fragment in ["'type'", "0 items", "validation error"] {
        assert!(
            !message.contains(fragment),
            "case {fragment:?}: the refusal must not carry it, got {message:?}"
        );
    }

    // Another member's own constraint keeps the framework's report: only the missing
    // connection type is worded by this service.
    let (status, response) = post(
        &router,
        "/participant/transcript",
        Some("participant-token-empty-type"),
        &json!({"MaxResults": 101}),
    )
    .await;
    assert_eq!(
        (status, error_type(&response).as_str()),
        (400, "ValidationException"),
        "case \"another member's constraint\": got {response:?}"
    );
    assert_ne!(
        error_message(&response).as_str(),
        "Failed to create participant connection due to missing connection type",
        "case \"another member's constraint\": got {response:?}"
    );
}

/// A participant token no started chat registered authenticates nobody: it is refused
/// rather than served a chat session of its own.
#[tokio::test]
async fn a_participant_token_no_started_chat_registered_is_refused() {
    let router = router();

    // Rows differ in the shape of the token and in the connection information asked for;
    // none of them was ever issued.
    let cases: &[(&str, &str, Value)] = &[
        (
            "an unissued token",
            "bogus-participant-token",
            json!({"Type": ["CONNECTION_CREDENTIALS"]}),
        ),
        (
            "a token shaped like one the service mints",
            "00000000-0000-4000-8000-000000000000",
            json!({"Type": ["WEBSOCKET"]}),
        ),
        ("a single character", "x", json!({"ConnectParticipant": true})),
        (
            "a token at the modelled maximum length",
            &"t".repeat(1000),
            json!({"Type": ["CONNECTION_CREDENTIALS", "WEBSOCKET"]}),
        ),
    ];
    for (case, participant_token, body) in cases {
        let (status, response) =
            post(&router, "/participant/connection", Some(participant_token), body).await;
        assert_eq!(
            (status, error_type(&response).as_str(), error_message(&response).as_str()),
            (
                403,
                "AccessDeniedException",
                "User is not authorized to access this resource with an explicit deny in an \
                 identity-based policy"
            ),
            "case {case:?}: got {response:?}"
        );
        // Nothing was minted for it: no session, no connection token, no participant token.
        let store = router.store();
        let state = store.lock();
        assert!(
            state.sessions.is_empty()
                && state.connections.is_empty()
                && state.participant_tokens.is_empty(),
            "case {case:?}: the refused call must mint nothing, store holds {} session(s), \
             {} connection(s), {} participant token(s)",
            state.sessions.len(),
            state.connections.len(),
            state.participant_tokens.len()
        );
    }

    // And the token a started chat DID register is served.
    let connection_token = connect(&router, "participant-token-issued").await;
    assert!(
        !connection_token.is_empty(),
        "case \"an issued token\": expected a ConnectionToken"
    );
}

#[tokio::test]
async fn re_using_a_participant_token_resolves_to_the_same_session() {
    let router = router();
    let first = connect(&router, "participant-token-reuse").await;
    let message_id = send_message(&router, &first, "first connection").await;

    let second = connect(&router, "participant-token-reuse").await;
    assert_ne!(
        first, second,
        "case reuse: each call must mint a fresh connection token, got {second:?} twice"
    );

    // Both tokens authenticate the SAME session, so the transcript already holds the
    // message the first connection sent.
    for (case, token) in [("first token", &first), ("second token", &second)] {
        let (items, _) = get_transcript(
            &router,
            token,
            &json!({"SortOrder": "ASCENDING", "ScanDirection": "FORWARD"}),
        )
        .await;
        assert_eq!(
            items.len(),
            1,
            "case {case}: expected one stored item, got {items:?}"
        );
        assert_eq!(
            items[0]["Id"].as_str(),
            Some(message_id.as_str()),
            "case {case}: expected the message the other connection sent, got {items:?}"
        );
    }
}

#[tokio::test]
async fn repeating_a_client_token_returns_the_stored_item_and_appends_nothing() {
    let router = router();
    let token = connect(&router, "participant-token-idempotent").await;
    let acknowledged = send_message(&router, &token, "a message to acknowledge").await;

    // Rows differ in the operation, the client token, the content type and the content.
    let cases: Vec<(&str, &str, &str, String)> = vec![
        ("/participant/message", "ct-1", "text/plain", "hello".to_string()),
        (
            "/participant/message",
            "ct-2",
            "text/plain",
            "a second message".to_string(),
        ),
        (
            "/participant/message",
            "a-much-longer-client-token-0123456789",
            "text/markdown",
            "# heading".to_string(),
        ),
        (
            "/participant/event",
            "ct-1",
            "application/vnd.amazonaws.connect.event.typing",
            String::new(),
        ),
        (
            "/participant/event",
            "ct-3",
            "application/vnd.amazonaws.connect.event.message.delivered",
            receipt_content(&acknowledged),
        ),
    ];

    // Only a message is an item of the conversation: a typing event is transient and a
    // receipt is recorded on the message it names, so neither grows the transcript.
    let mut expected_items = 1usize;
    for (path, client_token, content_type, content) in &cases {
        let case = format!("{path} {client_token}");
        let mut body = json!({"ContentType": content_type, "ClientToken": client_token});
        if !content.is_empty() {
            body["Content"] = json!(content);
        }
        let (status, first) = post(&router, path, Some(&token), &body).await;
        assert_eq!(status, 200, "case {case}: got {first:?}");
        if *path == "/participant/message" {
            expected_items += 1;
        }

        let (status, again) = post(&router, path, Some(&token), &body).await;
        assert_eq!(status, 200, "case {case} repeated: got {again:?}");
        assert_eq!(
            again["Id"], first["Id"],
            "case {case}: expected the same Id on the repeat, got {again:?}"
        );
        assert_eq!(
            again["AbsoluteTime"], first["AbsoluteTime"],
            "case {case}: expected the same AbsoluteTime on the repeat, got {again:?}"
        );

        let (items, _) = get_transcript(
            &router,
            &token,
            &json!({"MaxResults": 100, "SortOrder": "ASCENDING", "ScanDirection": "FORWARD"}),
        )
        .await;
        assert_eq!(
            items.len(),
            expected_items,
            "case {case}: the repeat must not append a second item, got {items:?}"
        );
    }

    // The same client token on SendMessage and on SendEvent is two distinct requests.
    let message_case = cases
        .iter()
        .find(|(path, client_token, _, _)| *path == "/participant/message" && *client_token == "ct-1")
        .expect("the SendMessage ct-1 row");
    let (_, message) = post(
        &router,
        message_case.0,
        Some(&token),
        &json!({"ContentType": message_case.2, "Content": message_case.3, "ClientToken": "ct-1"}),
    )
    .await;
    let (_, event) = post(
        &router,
        "/participant/event",
        Some(&token),
        &json!({"ContentType": "application/vnd.amazonaws.connect.event.typing", "ClientToken": "ct-1"}),
    )
    .await;
    assert_ne!(
        message["Id"], event["Id"],
        "case cross-operation ct-1: a client token scopes one operation, got {message:?} and {event:?}"
    );
}

#[tokio::test]
async fn paging_a_transcript_visits_every_item_exactly_once() {
    let router = router();
    let token = connect(&router, "participant-token-pages").await;
    let mut sent = Vec::new();
    for index in 0..7 {
        sent.push(send_message(&router, &token, &format!("message {index}")).await);
    }

    // Rows differ in page size, sort order and scan direction: whatever the scan, the
    // pages together must cover the transcript with no repeat and no drop.
    let cases: &[(Value, usize)] = &[
        (json!({"MaxResults": 1, "SortOrder": "ASCENDING", "ScanDirection": "FORWARD"}), 1),
        (json!({"MaxResults": 2, "SortOrder": "ASCENDING", "ScanDirection": "FORWARD"}), 2),
        (json!({"MaxResults": 3, "SortOrder": "DESCENDING", "ScanDirection": "BACKWARD"}), 3),
        (json!({"MaxResults": 7, "SortOrder": "DESCENDING", "ScanDirection": "BACKWARD"}), 7),
        (json!({"MaxResults": 100, "SortOrder": "ASCENDING", "ScanDirection": "BACKWARD"}), 7),
        // An absent MaxResults takes the documented default of 10.
        (json!({"SortOrder": "ASCENDING", "ScanDirection": "FORWARD"}), 7),
        (json!({"SortOrder": "DESCENDING"}), 7),
    ];

    for (request, first_page_len) in cases {
        let case = request.to_string();
        let mut seen: Vec<String> = Vec::new();
        let mut page_request = request.clone();
        let mut first = true;
        loop {
            let (items, next) = get_transcript(&router, &token, &page_request).await;
            if first {
                assert_eq!(
                    items.len(),
                    *first_page_len,
                    "case {case}: wrong first page size, got {items:?}"
                );
                first = false;
            }
            for item in &items {
                let id = item["Id"].as_str().unwrap_or_default().to_string();
                assert!(
                    !seen.contains(&id),
                    "case {case}: item {id} appeared on two pages, seen {seen:?}"
                );
                seen.push(id);
            }
            match next {
                Some(next_token) => {
                    page_request = json!({"NextToken": next_token});
                }
                None => break,
            }
        }
        let mut sorted_seen = seen.clone();
        sorted_seen.sort();
        let mut sorted_sent = sent.clone();
        sorted_sent.sort();
        assert_eq!(
            sorted_seen, sorted_sent,
            "case {case}: the pages must cover exactly the sent items, got {seen:?}"
        );
    }

    // `MaxResults` is modelled with range 0..100, so an explicit 0 is a valid page size
    // and is honoured as one: an empty page. No `NextToken` comes back with it, because a
    // page that consumes nothing cannot make progress and a continuation would hand back
    // the same empty page for ever.
    let (status, body) = post(
        &router,
        "/participant/transcript",
        Some(&token),
        &json!({"MaxResults": 0}),
    )
    .await;
    assert_eq!(status, 200, "case MaxResults 0: got {body:?}");
    assert_eq!(
        body["Transcript"].as_array().map(Vec::len),
        Some(0),
        "case MaxResults 0: expected an empty page, got {body:?}"
    );
    assert!(
        body.get("NextToken").is_none(),
        "case MaxResults 0: expected no NextToken for a page that cannot progress, got {body:?}"
    );
}

#[tokio::test]
async fn sort_order_orders_the_returned_page() {
    let router = router();
    let token = connect(&router, "participant-token-sort").await;
    let mut sent = Vec::new();
    for index in 0..4 {
        sent.push(send_message(&router, &token, &format!("message {index}")).await);
    }
    let mut reversed = sent.clone();
    reversed.reverse();

    // `SortOrder`'s value set is ASCENDING and DESCENDING (smithy_query shape=SortKey);
    // an absent SortOrder takes the documented default, DESCENDING.
    let cases: &[(Value, &Vec<String>)] = &[
        (json!({"MaxResults": 100, "SortOrder": "ASCENDING", "ScanDirection": "FORWARD"}), &sent),
        (json!({"MaxResults": 100, "SortOrder": "DESCENDING", "ScanDirection": "FORWARD"}), &reversed),
        (json!({"MaxResults": 100, "SortOrder": "ASCENDING", "ScanDirection": "BACKWARD"}), &sent),
        (json!({"MaxResults": 100, "SortOrder": "DESCENDING", "ScanDirection": "BACKWARD"}), &reversed),
        (json!({"MaxResults": 100}), &reversed),
    ];
    for (request, expected) in cases {
        let case = request.to_string();
        let (items, _) = get_transcript(&router, &token, request).await;
        let ids: Vec<String> = items
            .iter()
            .map(|item| item["Id"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            &ids, *expected,
            "case {case}: wrong order, got {ids:?}"
        );
    }

    // An unmodelled SortOrder is refused rather than silently defaulted.
    let (status, body) = post(
        &router,
        "/participant/transcript",
        Some(&token),
        &json!({"SortOrder": "SIDEWAYS"}),
    )
    .await;
    assert_eq!(status, 400, "case SIDEWAYS: got {body:?}");
    assert_eq!(
        error_type(&body),
        "ValidationException",
        "case SIDEWAYS: got {body:?}"
    );
}

#[tokio::test]
async fn start_position_decides_where_the_scan_begins() {
    let router = router();
    let token = connect(&router, "participant-token-start").await;
    let mut sent = Vec::new();
    for index in 0..5 {
        sent.push(send_message(&router, &token, &format!("message {index}")).await);
    }
    let (all, _) = get_transcript(
        &router,
        &token,
        &json!({"MaxResults": 100, "SortOrder": "ASCENDING", "ScanDirection": "FORWARD"}),
    )
    .await;
    let third_time = all[2]["AbsoluteTime"].as_str().expect("AbsoluteTime").to_string();

    // `StartPosition` selects a position by Id, by AbsoluteTime, or by counting back
    // from the newest item (smithy_query shape=StartPosition).
    let cases: Vec<(Value, Vec<String>)> = vec![
        (
            json!({"StartPosition": {"Id": sent[2]}, "ScanDirection": "FORWARD",
                   "SortOrder": "ASCENDING", "MaxResults": 100}),
            sent[2..].to_vec(),
        ),
        (
            json!({"StartPosition": {"Id": sent[2]}, "ScanDirection": "BACKWARD",
                   "SortOrder": "ASCENDING", "MaxResults": 100}),
            sent[..=2].to_vec(),
        ),
        (
            json!({"StartPosition": {"MostRecent": 2}, "ScanDirection": "FORWARD",
                   "SortOrder": "ASCENDING", "MaxResults": 100}),
            sent[3..].to_vec(),
        ),
        (
            json!({"StartPosition": {"MostRecent": 100}, "ScanDirection": "FORWARD",
                   "SortOrder": "ASCENDING", "MaxResults": 100}),
            sent.clone(),
        ),
        (
            // An absent StartPosition with an absent ScanDirection scans BACKWARD from
            // the newest item, which reaches the whole transcript.
            json!({"SortOrder": "ASCENDING", "MaxResults": 100}),
            sent.clone(),
        ),
    ];
    for (request, expected) in &cases {
        let case = request.to_string();
        let (items, _) = get_transcript(&router, &token, request).await;
        let ids: Vec<String> = items
            .iter()
            .map(|item| item["Id"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(&ids, expected, "case {case}: wrong window, got {ids:?}");
    }

    // `StartPosition.AbsoluteTime` selects by time, not by index: a FORWARD scan from it
    // reaches every item at or after that instant, and a BACKWARD scan every item at or
    // before it. Several items can share one millisecond, so the expectation is derived
    // from the stored timestamps rather than from a position in the send order.
    for (case, direction, forward) in [
        ("AbsoluteTime FORWARD", "FORWARD", true),
        ("AbsoluteTime BACKWARD", "BACKWARD", false),
    ] {
        let expected: Vec<String> = all
            .iter()
            .filter(|item| {
                let at = item["AbsoluteTime"].as_str().unwrap_or_default();
                if forward {
                    at >= third_time.as_str()
                } else {
                    at <= third_time.as_str()
                }
            })
            .map(|item| item["Id"].as_str().unwrap_or_default().to_string())
            .collect();
        let (items, _) = get_transcript(
            &router,
            &token,
            &json!({"StartPosition": {"AbsoluteTime": third_time}, "ScanDirection": direction,
                    "SortOrder": "ASCENDING", "MaxResults": 100}),
        )
        .await;
        let ids: Vec<String> = items
            .iter()
            .map(|item| item["Id"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(ids, expected, "case {case}: wrong window, got {ids:?}");
    }

    // A StartPosition that names no item of this transcript is a client error.
    let (status, body) = post(
        &router,
        "/participant/transcript",
        Some(&token),
        &json!({"StartPosition": {"Id": "00000000-0000-4000-8000-000000000000"}}),
    )
    .await;
    assert_eq!(status, 400, "case unknown StartPosition.Id: got {body:?}");
    assert_eq!(
        error_type(&body),
        "ValidationException",
        "case unknown StartPosition.Id: got {body:?}"
    );

    let (status, body) = post(
        &router,
        "/participant/transcript",
        Some(&token),
        &json!({"StartPosition": {"AbsoluteTime": "yesterday"}}),
    )
    .await;
    assert_eq!(status, 400, "case malformed AbsoluteTime: got {body:?}");
    assert_eq!(
        error_type(&body),
        "ValidationException",
        "case malformed AbsoluteTime: got {body:?}"
    );

    let (status, body) = post(
        &router,
        "/participant/transcript",
        Some(&token),
        &json!({"NextToken": "not-a-token-we-issued"}),
    )
    .await;
    assert_eq!(status, 400, "case foreign NextToken: got {body:?}");
    assert_eq!(
        error_type(&body),
        "ValidationException",
        "case foreign NextToken: got {body:?}"
    );

    // An oversized NextToken (still within the modelled 1..1000 length) is refused too.
    let (status, body) = post(
        &router,
        "/participant/transcript",
        Some(&token),
        &json!({"NextToken": "ab".repeat(400)}),
    )
    .await;
    assert_eq!(status, 400, "case oversized NextToken: got {body:?}");
    assert_eq!(
        error_type(&body),
        "ValidationException",
        "case oversized NextToken: got {body:?}"
    );
}

#[tokio::test]
async fn a_connection_token_that_names_no_connected_participant_is_access_denied() {
    let router = router();
    let live = connect(&router, "participant-token-denied").await;
    send_message(&router, &live, "before the disconnect").await;

    let (status, body) = post(
        &router,
        "/participant/disconnect",
        Some(&live),
        &json!({"ClientToken": "dc-1"}),
    )
    .await;
    assert_eq!(status, 200, "DisconnectParticipant: got {body:?}");
    assert!(
        body.is_null() || body == json!({}),
        "DisconnectParticipant must answer an empty body, got {body:?}"
    );

    // Rows differ in how the token fails to name a connected participant.
    let tokens: &[(&str, String)] = &[
        ("unknown token", "bogus-token".to_string()),
        (
            "a token shaped like one we mint",
            "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        ("a single character", "x".to_string()),
        ("the disconnected session's token", live.clone()),
    ];
    let calls: &[(&str, Value)] = &[
        (
            "/participant/message",
            json!({"ContentType": "text/plain", "Content": "hello"}),
        ),
        (
            "/participant/event",
            json!({"ContentType": "application/vnd.amazonaws.connect.event.typing"}),
        ),
        ("/participant/transcript", json!({})),
        ("/participant/disconnect", json!({})),
    ];
    for (label, token) in tokens {
        for (path, body) in calls {
            let case = format!("{path} with {label}");
            let (status, response) = post(&router, path, Some(token), body).await;
            assert_eq!(status, 403, "case {case}: got {response:?}");
            assert_eq!(
                error_type(&response),
                "AccessDeniedException",
                "case {case}: got {response:?}"
            );
        }
    }

    // The participant token is no longer part of a contact either.
    let (status, response) = post(
        &router,
        "/participant/connection",
        Some("participant-token-denied"),
        &json!({"Type": ["CONNECTION_CREDENTIALS"]}),
    )
    .await;
    assert_eq!(
        status, 403,
        "case reconnecting a disconnected participant: got {response:?}"
    );
    assert_eq!(
        error_type(&response),
        "AccessDeniedException",
        "case reconnecting a disconnected participant: got {response:?}"
    );
}

#[tokio::test]
async fn modelled_constraints_are_refused_at_the_bounds_the_model_declares() {
    let router = router();
    let token = connect(&router, "participant-token-bounds").await;

    // The bounds come from the model: ContentType len 1..100, Content len 1..16384,
    // MaxResults range 0..100 (smithy_query operation=SendMessage,GetTranscript).
    let accepted: &[(&str, Value)] = &[
        (
            "Content at its minimum length",
            json!({"ContentType": "text/plain", "Content": "x"}),
        ),
        (
            "Content at its maximum length",
            json!({"ContentType": "text/plain", "Content": "x".repeat(16_384)}),
        ),
        (
            "ContentType at its maximum length",
            json!({"ContentType": "x".repeat(100), "Content": "hello"}),
        ),
    ];
    for (case, body) in accepted {
        let (status, response) = post(&router, "/participant/message", Some(&token), body).await;
        assert_eq!(status, 200, "case {case}: got {response:?}");
    }

    let refused: &[(&str, &str, Value)] = &[
        (
            "empty Content",
            "/participant/message",
            json!({"ContentType": "text/plain", "Content": ""}),
        ),
        (
            "Content one past its maximum",
            "/participant/message",
            json!({"ContentType": "text/plain", "Content": "x".repeat(16_385)}),
        ),
        (
            "ContentType one past its maximum",
            "/participant/message",
            json!({"ContentType": "x".repeat(101), "Content": "hello"}),
        ),
        (
            "empty ContentType",
            "/participant/message",
            json!({"ContentType": "", "Content": "hello"}),
        ),
        (
            "MaxResults one past its maximum",
            "/participant/transcript",
            json!({"MaxResults": 101}),
        ),
        (
            "MaxResults far past its maximum",
            "/participant/transcript",
            json!({"MaxResults": 500}),
        ),
        (
            "MaxResults below its minimum",
            "/participant/transcript",
            json!({"MaxResults": -1}),
        ),
    ];
    for (case, path, body) in refused {
        let (status, response) = post(&router, path, Some(&token), body).await;
        assert_eq!(status, 400, "case {case}: got {response:?}");
        assert_eq!(
            error_type(&response),
            "ValidationException",
            "case {case}: got {response:?}"
        );
    }

    // MaxResults at both published bounds is accepted.
    for max_results in [0, 100] {
        let (status, response) = post(
            &router,
            "/participant/transcript",
            Some(&token),
            &json!({"MaxResults": max_results}),
        )
        .await;
        assert_eq!(status, 200, "case MaxResults {max_results}: got {response:?}");
    }
}

#[tokio::test]
async fn a_contact_id_from_another_connection_is_refused() {
    let router = router();
    let mine = connect(&router, "participant-token-mine").await;
    let theirs = connect(&router, "participant-token-theirs").await;
    send_message(&router, &theirs, "not for you").await;

    let (_, their_transcript) =
        post(&router, "/participant/transcript", Some(&theirs), &json!({})).await;
    let their_contact_id = their_transcript["InitialContactId"]
        .as_str()
        .expect("InitialContactId");

    let (status, body) = post(
        &router,
        "/participant/transcript",
        Some(&mine),
        &json!({"ContactId": their_contact_id}),
    )
    .await;
    assert_eq!(status, 400, "case another contact's id: got {body:?}");
    assert_eq!(
        error_type(&body),
        "ValidationException",
        "case another contact's id: got {body:?}"
    );

    // Our own contact id is accepted and returns our own (empty) transcript.
    let (_, my_transcript) =
        post(&router, "/participant/transcript", Some(&mine), &json!({})).await;
    let my_contact_id = my_transcript["InitialContactId"]
        .as_str()
        .expect("InitialContactId")
        .to_string();
    let (items, _) = get_transcript(&router, &mine, &json!({"ContactId": my_contact_id})).await;
    assert!(
        items.is_empty(),
        "case own contact id: expected an empty transcript, got {items:?}"
    );
}

/// An event this service has no conversation item for is acknowledged with an `Id` and an
/// `AbsoluteTime` and recorded nowhere: the transcript holds only what the chat retains.
///
/// `GetTranscript` documents the event content types a transcript contains
/// (`…participant.invited`, `…participant.joined`, `…participant.left`, `…chat.ended`,
/// `…transfer.succeeded`, `…transfer.failed`), and no content type `SendEvent` sends is
/// one of them.
#[tokio::test]
async fn a_transient_event_is_acknowledged_and_never_becomes_a_transcript_item() {
    let router = router();
    let token = connect(&router, "participant-token-transient").await;

    // Rows are the content types `SendEvent` documents that name no message of their own
    // (smithy_query operation=SendEvent, member ContentType), with and without content.
    let cases: Vec<(&str, &str, Option<&str>)> = vec![
        (
            "typing",
            "application/vnd.amazonaws.connect.event.typing",
            None,
        ),
        (
            "typing with content",
            "application/vnd.amazonaws.connect.event.typing",
            Some("{}"),
        ),
        (
            "connection acknowledged",
            "application/vnd.amazonaws.connect.event.connection.acknowledged",
            None,
        ),
    ];

    let mut acknowledged: Vec<(String, String)> = Vec::new();
    for (case, content_type, content) in &cases {
        let mut body = json!({"ContentType": content_type});
        if let Some(content) = content {
            body["Content"] = json!(content);
        }
        let (status, response) = post(&router, "/participant/event", Some(&token), &body).await;
        assert_eq!(status, 200, "case {case:?}: got {response:?}");
        let id = response["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an Id, got {response:?}"))
            .to_string();
        assert_iso8601_millis(
            case,
            response["AbsoluteTime"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected an AbsoluteTime")),
        );
        assert!(
            acknowledged.iter().all(|(_, other)| other != &id),
            "case {case:?}: expected an Id of its own, got {id:?} again"
        );
        acknowledged.push(((*content_type).to_string(), id));
    }

    // A message sent after them fences the transcript: whatever the scan, the events are
    // not in it.
    let fence = send_message(&router, &token, "after the events").await;
    let (items, _) = get_transcript(
        &router,
        &token,
        &json!({"MaxResults": 100, "SortOrder": "ASCENDING", "ScanDirection": "FORWARD"}),
    )
    .await;
    assert_eq!(
        items.len(),
        1,
        "case \"the fenced transcript\": expected only the message, got {items:?}"
    );
    assert_eq!(
        items[0]["Id"].as_str(),
        Some(fence.as_str()),
        "case \"the fenced transcript\": expected the message, got {items:?}"
    );
    for (content_type, id) in &acknowledged {
        assert!(
            items.iter().all(|item| {
                item["Id"].as_str() != Some(id.as_str())
                    && item["ContentType"].as_str() != Some(content_type.as_str())
                    && item["Type"].as_str() != Some("TYPING")
            }),
            "case {content_type:?}: no transcript item may name it, got {items:?}"
        );
    }
}

/// One row of the message-receipt table: which receipts one message is sent, and the
/// timestamps its one receipt entry must carry afterwards.
struct ReceiptCase {
    /// What the assertions name this row.
    case: &'static str,
    /// Each receipt as its `ContentType` and the `ClientToken` it is sent under; an empty
    /// token means the call carries none.
    receipts: &'static [(&'static str, &'static str)],
    /// Whether the entry must carry a `DeliveredTimestamp` at the end.
    delivered: bool,
    /// Whether the entry must carry a `ReadTimestamp` at the end.
    read: bool,
}

/// A message receipt answers the `Id` of the message its `Content` names, and that
/// message's own transcript item reports it as `MessageMetadata`.
#[tokio::test]
async fn a_message_receipt_answers_its_message_and_shows_up_on_that_items_metadata() {
    let router = router();
    let token = connect(&router, "participant-token-receipts").await;

    const DELIVERED: &str = "application/vnd.amazonaws.connect.event.message.delivered";
    const READ: &str = "application/vnd.amazonaws.connect.event.message.read";

    // Rows differ in which receipts one message gets, in which order, and under which
    // `ClientToken` — which is what decides the timestamps its one receipt entry carries.
    // A receipt's answer follows from the message it names, so repeating a token cannot
    // swallow the acknowledgement.
    let cases: &[ReceiptCase] = &[
        ReceiptCase {
            case: "delivered only",
            receipts: &[(DELIVERED, "")],
            delivered: true,
            read: false,
        },
        ReceiptCase {
            case: "delivered then read",
            receipts: &[(DELIVERED, ""), (READ, "")],
            delivered: true,
            read: true,
        },
        ReceiptCase {
            case: "read only",
            receipts: &[(READ, "")],
            delivered: false,
            read: true,
        },
        ReceiptCase {
            case: "delivered twice",
            receipts: &[(DELIVERED, ""), (DELIVERED, "")],
            delivered: true,
            read: false,
        },
        ReceiptCase {
            case: "delivered then read under one ClientToken",
            receipts: &[(DELIVERED, "one-token"), (READ, "one-token")],
            delivered: true,
            read: true,
        },
    ];
    for ReceiptCase {
        case,
        receipts,
        delivered: expect_delivered,
        read: expect_read,
    } in cases
    {
        let message_id = send_message(&router, &token, &format!("message for {case}")).await;
        for (content_type, client_token) in *receipts {
            let mut body = json!({"ContentType": content_type,
                                  "Content": receipt_content(&message_id)});
            if !client_token.is_empty() {
                body["ClientToken"] = json!(client_token);
            }
            let (status, answered) = post(&router, "/participant/event", Some(&token), &body).await;
            assert_eq!(status, 200, "case {case:?}: got {answered:?}");
            assert_eq!(
                answered["Id"].as_str(),
                Some(message_id.as_str()),
                "case {case:?}: a receipt must answer the Id of the message it names, \
                 got {answered:?}"
            );
            assert_iso8601_millis(
                case,
                answered["AbsoluteTime"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: expected an AbsoluteTime")),
            );
        }

        let (items, _) = get_transcript(
            &router,
            &token,
            &json!({"MaxResults": 100, "SortOrder": "ASCENDING", "ScanDirection": "FORWARD"}),
        )
        .await;
        let item = items
            .iter()
            .find(|item| item["Id"].as_str() == Some(message_id.as_str()))
            .unwrap_or_else(|| panic!("case {case:?}: the message is not in {items:?}"));
        let metadata = item
            .get("MessageMetadata")
            .unwrap_or_else(|| panic!("case {case:?}: expected MessageMetadata on {item:?}"));
        assert_eq!(
            metadata["MessageId"].as_str(),
            Some(message_id.as_str()),
            "case {case:?}: MessageMetadata must name its own message, got {metadata:?}"
        );
        let entries = metadata["Receipts"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected Receipts on {metadata:?}"));
        assert_eq!(
            entries.len(),
            1,
            "case {case:?}: one recipient acknowledged it, so it carries one receipt, \
             got {entries:?}"
        );
        let receipt = &entries[0];
        assert_eq!(
            receipt["RecipientParticipantId"].as_str(),
            item["ParticipantId"].as_str(),
            "case {case:?}: the receipt names the participant that acknowledged it, \
             got {receipt:?}"
        );
        assert_eq!(
            receipt.get("DeliveredTimestamp").is_some(),
            *expect_delivered,
            "case {case:?}: DeliveredTimestamp presence wrong, got {receipt:?}"
        );
        assert_eq!(
            receipt.get("ReadTimestamp").is_some(),
            *expect_read,
            "case {case:?}: ReadTimestamp presence wrong, got {receipt:?}"
        );
        if let (Some(delivered), Some(read)) = (
            receipt["DeliveredTimestamp"].as_str(),
            receipt["ReadTimestamp"].as_str(),
        ) {
            assert_iso8601_millis(case, delivered);
            assert_iso8601_millis(case, read);
            assert!(
                delivered <= read,
                "case {case:?}: expected DeliveredTimestamp not after ReadTimestamp, \
                 got {receipt:?}"
            );
        }
    }

    // A receipt adds no item of its own: the transcript holds exactly the messages sent.
    let (items, _) = get_transcript(
        &router,
        &token,
        &json!({"MaxResults": 100, "SortOrder": "ASCENDING", "ScanDirection": "FORWARD"}),
    )
    .await;
    assert_eq!(
        items.len(),
        cases.len(),
        "case \"the whole transcript\": expected one item per message sent, got {items:?}"
    );

    // Content that names no message of this chat is a client error, not a new item.
    let foreign = connect(&router, "participant-token-receipts-other").await;
    let theirs = send_message(&router, &foreign, "another chat's message").await;
    let refused: Vec<(&str, Value)> = vec![
        (
            "no Content at all",
            json!({"ContentType": DELIVERED}),
        ),
        (
            "Content that is not the receipt form",
            json!({"ContentType": DELIVERED, "Content": "just text"}),
        ),
        (
            "Content with no messageId member",
            json!({"ContentType": READ, "Content": "{\"message\":\"x\"}"}),
        ),
        (
            "a messageId of no item",
            json!({"ContentType": DELIVERED,
                   "Content": receipt_content("00000000-0000-4000-8000-000000000000")}),
        ),
        (
            "a messageId from another chat",
            json!({"ContentType": DELIVERED, "Content": receipt_content(&theirs)}),
        ),
    ];
    for (case, body) in &refused {
        let (status, response) = post(&router, "/participant/event", Some(&token), body).await;
        assert_eq!(
            (status, error_type(&response).as_str()),
            (400, "ValidationException"),
            "case {case:?}: got {response:?}"
        );
    }
    let (items_again, _) = get_transcript(
        &router,
        &token,
        &json!({"MaxResults": 100, "SortOrder": "ASCENDING", "ScanDirection": "FORWARD"}),
    )
    .await;
    assert_eq!(
        items_again.len(),
        items.len(),
        "case \"after the refusals\": a refused receipt must store nothing, got {items_again:?}"
    );
}

/// A `ClientToken` makes one REQUEST idempotent: the replay key is the token together with
/// the `Content` the request carries, and never its `ContentType`. A receipt is not
/// replayed from a token at all — its answer follows from the message it names.
#[tokio::test]
async fn a_reused_client_token_is_judged_on_the_content_it_carries() {
    let router = router();
    let token = connect(&router, "participant-token-content-key").await;
    let first_message = send_message(&router, &token, "the first acknowledged message").await;
    let second_message = send_message(&router, &token, "the second acknowledged message").await;

    const DELIVERED: &str = "application/vnd.amazonaws.connect.event.message.delivered";
    const READ: &str = "application/vnd.amazonaws.connect.event.message.read";
    const TYPING: &str = "application/vnd.amazonaws.connect.event.typing";

    // Rows are (path, the first call, the second call, whether the two answer one Id).
    // They differ in the operation and in what the second call changes: the content type
    // alone (one request repeated), or the content (a request of its own).
    let cases: Vec<(&str, &str, Value, Value, bool)> = vec![
        (
            "a receipt under one token with another content type",
            "/participant/event",
            json!({"ContentType": DELIVERED, "Content": receipt_content(&first_message),
                   "ClientToken": "ck-1"}),
            json!({"ContentType": READ, "Content": receipt_content(&first_message),
                   "ClientToken": "ck-1"}),
            true,
        ),
        (
            "a receipt under one token for another message",
            "/participant/event",
            json!({"ContentType": DELIVERED, "Content": receipt_content(&first_message),
                   "ClientToken": "ck-2"}),
            json!({"ContentType": DELIVERED, "Content": receipt_content(&second_message),
                   "ClientToken": "ck-2"}),
            false,
        ),
        (
            "a typing event under one token with no content either time",
            "/participant/event",
            json!({"ContentType": TYPING, "ClientToken": "ck-3"}),
            json!({"ContentType": TYPING, "ClientToken": "ck-3"}),
            true,
        ),
        (
            "a typing event under one token that gains content",
            "/participant/event",
            json!({"ContentType": TYPING, "ClientToken": "ck-4"}),
            json!({"ContentType": TYPING, "Content": "{}", "ClientToken": "ck-4"}),
            false,
        ),
        (
            "a message under one token with another content type",
            "/participant/message",
            json!({"ContentType": "text/plain", "Content": "# heading", "ClientToken": "ck-5"}),
            json!({"ContentType": "text/markdown", "Content": "# heading", "ClientToken": "ck-5"}),
            true,
        ),
        (
            "a message under one token with other content",
            "/participant/message",
            json!({"ContentType": "text/plain", "Content": "one", "ClientToken": "ck-6"}),
            json!({"ContentType": "text/plain", "Content": "two", "ClientToken": "ck-6"}),
            false,
        ),
    ];
    for (case, path, first_body, second_body, same) in &cases {
        let (status, first) = post(&router, path, Some(&token), first_body).await;
        assert_eq!(status, 200, "case {case:?}: got {first:?}");
        let (status, second) = post(&router, path, Some(&token), second_body).await;
        assert_eq!(status, 200, "case {case:?} second call: got {second:?}");
        let (first_id, second_id) = (first["Id"].as_str(), second["Id"].as_str());
        if *same {
            assert_eq!(
                first_id, second_id,
                "case {case:?}: expected the first answer replayed, got {second:?}"
            );
        } else {
            assert_ne!(
                first_id, second_id,
                "case {case:?}: expected an answer of its own, got {second:?}"
            );
        }
    }
}
