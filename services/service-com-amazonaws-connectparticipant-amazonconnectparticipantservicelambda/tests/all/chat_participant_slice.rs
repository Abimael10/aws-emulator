// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the chat-participant slice: `CreateParticipantConnection`,
//! `SendMessage`, `SendEvent`, `GetTranscript`, `DisconnectParticipant` and the
//! participant's two authentication operations, `GetAuthenticationUrl` and
//! `CancelParticipantAuthentication`.
//!
//! Every case goes over the wire, through the same `MultiServiceRouter` the production
//! router dispatches with, so the HTTP status, the restJson1 envelope and the stored
//! state are all exercised together.
//!
//! An authentication session is minted where authentication is INITIATED, which no call
//! this service's API carries does, so the authentication cases write the session into
//! this service's store the way the AuthenticateCustomer flow block does
//! (`state::AuthenticationSession`) and then read it back through the operations.

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
///
/// A page reports `NextToken` whether or not the scan continues past it, so this asserts
/// the member is PRESENT and answers a continuation only for a token that continues the
/// scan: the terminal token (the empty string) is the end of the pages.
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
        .unwrap_or_else(|| panic!("GetTranscript {request} reported no NextToken: {body:?}"));
    let next = (!next.is_empty()).then(|| next.to_string());
    (items, next)
}

/// How many items the transcript of the session `connection_token` authenticates holds.
///
/// Read back through `GetTranscript` with the largest page the model allows, so the count
/// is the whole transcript rather than one page of it.
async fn transcript_len(router: &MultiServiceRouter, connection_token: &str) -> usize {
    let (items, _) = get_transcript(
        router,
        connection_token,
        &json!({"MaxResults": 100, "SortOrder": "ASCENDING", "ScanDirection": "FORWARD"}),
    )
    .await;
    items.len()
}

/// Which record the service answers a repeated request from.
///
/// A repeat carrying a `ClientToken` is answered with what the FIRST call wrote, so a case
/// names where that value is kept: `SendMessage` keeps it on the chat item it appended, a
/// transient `SendEvent` keeps the answer it reported on the session, and a message receipt
/// keeps its timestamp on the message it acknowledges, under the acknowledging
/// participant's own entry (`Receipt$DeliveredTimestamp` / `Receipt$ReadTimestamp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answered {
    /// The chat item `SendMessage` appended.
    Item,
    /// The transient event the session remembers for a `ClientToken`.
    Event,
    /// `Receipt$DeliveredTimestamp` on the acknowledged message.
    DeliveredReceipt,
    /// `Receipt$ReadTimestamp` on the acknowledged message.
    ReadReceipt,
}

/// The `AbsoluteTime` the store holds for the answer `id` names, or `None` when no record
/// of that kind holds it.
///
/// Every assertion about a repeat is made against THIS value rather than against the other
/// answer: the stored record is one reference, the same on every host, so the check has a
/// single outcome however long the two calls are apart and however fast the host runs.
fn stored_absolute_time(
    harness: &Harness,
    participant_token: &str,
    answered: Answered,
    id: &str,
) -> Option<String> {
    let store = harness.store();
    let state = store.lock();
    let contact_id = state.participant_tokens.get(participant_token)?;
    let session = state.sessions.get(contact_id)?;
    match answered {
        Answered::Item => Some(session.item(id)?.absolute_time.clone()),
        Answered::Event => session
            .events
            .values()
            .find(|event| event.id == id)
            .map(|event| event.absolute_time.clone()),
        Answered::DeliveredReceipt | Answered::ReadReceipt => {
            let recipient = session.participant_id.as_str();
            let receipt = session
                .item(id)?
                .receipts
                .iter()
                .find(|receipt| receipt.recipient_participant_id == recipient)?;
            if answered == Answered::ReadReceipt {
                receipt.read_timestamp.clone()
            } else {
                receipt.delivered_timestamp.clone()
            }
        }
    }
}

/// Overwrite the `AbsoluteTime` the store holds for the answer `id` names with `at`.
///
/// Pins a stored answer to an instant no clock read in this run can produce, so a repeat
/// that reports it can only have read the record the first call wrote.
fn pin_stored_absolute_time(
    harness: &Harness,
    participant_token: &str,
    answered: Answered,
    id: &str,
    at: &str,
) {
    let store = harness.store();
    let mut state = store.lock();
    let contact_id = state
        .participant_tokens
        .get(participant_token)
        .cloned()
        .expect("the chat the participant token names");
    let session = state
        .sessions
        .get_mut(&contact_id)
        .expect("the session the started chat registered");
    let recipient = session.participant_id.clone();
    if answered == Answered::Event {
        let event = session
            .events
            .values_mut()
            .find(|event| event.id == id)
            .expect("the remembered event the answer names");
        event.absolute_time = at.to_string();
        return;
    }
    let item = session
        .transcript
        .iter_mut()
        .find(|item| item.id == id)
        .expect("the chat item the answer names");
    match answered {
        Answered::Item => item.absolute_time = at.to_string(),
        _ => {
            let receipt = item
                .receipts
                .iter_mut()
                .find(|receipt| receipt.recipient_participant_id == recipient)
                .expect("this participant's receipt entry on the acknowledged message");
            if answered == Answered::ReadReceipt {
                receipt.read_timestamp = Some(at.to_string());
            } else {
                receipt.delivered_timestamp = Some(at.to_string());
            }
        }
    }
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
    let metadata = items[0]
        .get("MessageMetadata")
        .unwrap_or_else(|| panic!("case round trip: expected MessageMetadata, got {items:?}"));
    assert_eq!(
        (
            metadata["MessageId"].as_str(),
            metadata["Receipts"].as_array().map(Vec::len)
        ),
        (Some(message_id.as_str()), Some(0)),
        "case round trip: a message nobody acknowledged names itself and carries no \
         receipts, got {metadata:?}"
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
    /// The chat these cases connect to. Named, because the stored record each answer is
    /// compared with is read back through it.
    const PARTICIPANT: &str = "participant-token-idempotent";

    let router = router();
    let token = connect(&router, PARTICIPANT).await;
    let acknowledged = send_message(&router, &token, "a message to acknowledge").await;

    // Rows differ in the operation, the client token, the content type, the content and
    // which record the answer is kept on. The two receipt content types and
    // `…event.typing` are `SendEvent`'s own documented values.
    let cases: Vec<(&str, &str, &str, String, Answered)> = vec![
        (
            "/participant/message",
            "ct-1",
            "text/plain",
            "hello".to_string(),
            Answered::Item,
        ),
        (
            "/participant/message",
            "ct-2",
            "text/plain",
            "a second message".to_string(),
            Answered::Item,
        ),
        (
            "/participant/message",
            "a-much-longer-client-token-0123456789",
            "text/markdown",
            "# heading".to_string(),
            Answered::Item,
        ),
        (
            "/participant/event",
            "ct-1",
            "application/vnd.amazonaws.connect.event.typing",
            String::new(),
            Answered::Event,
        ),
        (
            "/participant/event",
            "ct-3",
            "application/vnd.amazonaws.connect.event.message.delivered",
            receipt_content(&acknowledged),
            Answered::DeliveredReceipt,
        ),
        (
            "/participant/event",
            "ct-4",
            "application/vnd.amazonaws.connect.event.message.read",
            receipt_content(&acknowledged),
            Answered::ReadReceipt,
        ),
    ];

    // Only a message is an item of the conversation: a typing event is transient and a
    // receipt is recorded on the message it names, so neither grows the transcript.
    let mut expected_items = 1usize;
    for (path, client_token, content_type, content, answered) in &cases {
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

        // What the call STORED for this answer. Every assertion below is made against it,
        // so none of them turns on how much of the clock passed between the two calls.
        let id = first["Id"].as_str().unwrap_or_default().to_string();
        let stored = stored_absolute_time(&router, PARTICIPANT, *answered, &id).unwrap_or_else(
            || panic!("case {case}: the call stored no {answered:?} record for {first:?}"),
        );
        assert_eq!(
            first["AbsoluteTime"].as_str(),
            Some(stored.as_str()),
            "case {case}: expected the AbsoluteTime it stored ({stored}), got {first:?}"
        );

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
        assert_eq!(
            again["AbsoluteTime"].as_str(),
            Some(stored.as_str()),
            "case {case}: expected the repeat answered from the stored {answered:?} record \
             ({stored}), got {again:?}"
        );

        let items = transcript_len(&router, &token).await;
        assert_eq!(
            items, expected_items,
            "case {case}: the repeat must not append a second item, got {items} item(s)"
        );
    }

    // The same client token on SendMessage and on SendEvent is two distinct requests.
    let message_case = cases
        .iter()
        .find(|(path, client_token, ..)| {
            *path == "/participant/message" && *client_token == "ct-1"
        })
        .expect("the SendMessage ct-1 row");
    let (status, message) = post(
        &router,
        message_case.0,
        Some(&token),
        &json!({"ContentType": message_case.2, "Content": message_case.3, "ClientToken": "ct-1"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case cross-operation ct-1 message: got {message:?}"
    );
    let (status, event) = post(
        &router,
        "/participant/event",
        Some(&token),
        &json!({"ContentType": "application/vnd.amazonaws.connect.event.typing", "ClientToken": "ct-1"}),
    )
    .await;
    assert_eq!(status, 200, "case cross-operation ct-1 event: got {event:?}");
    assert_ne!(
        message["Id"], event["Id"],
        "case cross-operation ct-1: a client token scopes one operation, got {message:?} and {event:?}"
    );
}

#[tokio::test]
async fn a_repeated_request_is_answered_from_the_stored_record_not_from_the_clock() {
    /// The chat these cases connect to.
    const PARTICIPANT: &str = "participant-token-replay-from-state";
    /// An `Instant` in the form the model documents for a chat item's `AbsoluteTime`
    /// (`yyyy-MM-ddThh:mm:ss.SSSZ`), written into the store so that no clock read in this
    /// run can have produced it: an answer carrying it was read from the record.
    const PINNED: &str = "2019-11-08T02:41:28.172Z";
    const TYPING: &str = "application/vnd.amazonaws.connect.event.typing";
    const DELIVERED: &str = "application/vnd.amazonaws.connect.event.message.delivered";
    const READ: &str = "application/vnd.amazonaws.connect.event.message.read";

    let router = router();
    let token = connect(&router, PARTICIPANT).await;
    let acknowledged = send_message(&router, &token, "a message to acknowledge").await;

    // Rows differ in the operation, the content type, the content and which record holds
    // the answer — every kind of record a repeat can be answered from.
    let cases: Vec<(&str, &str, Value, Answered)> = vec![
        (
            "a message",
            "/participant/message",
            json!({"ContentType": "text/plain", "Content": "hello", "ClientToken": "pin-1"}),
            Answered::Item,
        ),
        (
            "a message carrying markdown",
            "/participant/message",
            json!({"ContentType": "text/markdown", "Content": "# heading", "ClientToken": "pin-2"}),
            Answered::Item,
        ),
        (
            "a transient event",
            "/participant/event",
            json!({"ContentType": TYPING, "ClientToken": "pin-3"}),
            Answered::Event,
        ),
        (
            "a delivered receipt",
            "/participant/event",
            json!({"ContentType": DELIVERED, "Content": receipt_content(&acknowledged),
                   "ClientToken": "pin-4"}),
            Answered::DeliveredReceipt,
        ),
        (
            "a read receipt",
            "/participant/event",
            json!({"ContentType": READ, "Content": receipt_content(&acknowledged),
                   "ClientToken": "pin-5"}),
            Answered::ReadReceipt,
        ),
    ];

    for (case, path, body, answered) in &cases {
        let (status, first) = post(&router, path, Some(&token), body).await;
        assert_eq!(status, 200, "case {case:?}: got {first:?}");
        let id = first["Id"].as_str().unwrap_or_default().to_string();
        assert!(
            !id.is_empty(),
            "case {case:?}: expected an Id to be answered, got {first:?}"
        );
        let before = transcript_len(&router, &token).await;

        // Pin the stored answer to an instant the clock cannot read, then repeat the
        // request: an answer carrying the pinned instant came from the record, so a
        // repeat's answer never depends on when the repeat arrives.
        pin_stored_absolute_time(&router, PARTICIPANT, *answered, &id, PINNED);

        let (status, again) = post(&router, path, Some(&token), body).await;
        assert_eq!(status, 200, "case {case:?} repeated: got {again:?}");
        assert_eq!(
            again["Id"].as_str(),
            Some(id.as_str()),
            "case {case:?}: expected the stored record's Id {id}, got {again:?}"
        );
        assert_eq!(
            again["AbsoluteTime"].as_str(),
            Some(PINNED),
            "case {case:?}: expected the stored AbsoluteTime {PINNED}, got {again:?}"
        );

        let after = transcript_len(&router, &token).await;
        assert_eq!(
            after, before,
            "case {case:?}: the repeat must append nothing, transcript went from {before} \
             to {after} item(s)"
        );
    }
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
    // and is honoured as one: an empty page. It ends the scan, because a page that consumes
    // nothing cannot make progress and a continuation would hand back the same empty page
    // for ever — so it carries the terminal `NextToken` rather than one of ours.
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
    assert_eq!(
        body.get("NextToken").and_then(Value::as_str),
        Some(""),
        "case MaxResults 0: expected the terminal NextToken for a page that cannot \
         progress, got {body:?}"
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

// ---------------------------------------------------------------------------------------
// The participant's authentication operations: `GetAuthenticationUrl` and
// `CancelParticipantAuthentication`.
// ---------------------------------------------------------------------------------------

/// Write the record the AuthenticateCustomer flow block writes when it raises the
/// `authenticationInitiated` event for a contact's participant.
fn initiate_authentication(
    harness: &Harness,
    session_id: &str,
    session: participant::state::AuthenticationSession,
) {
    harness
        .store()
        .lock()
        .authentication_sessions
        .insert(session_id.to_string(), session);
}

/// One authentication session as the flow block stored it, for `contact_id`, raised
/// `initiated_secs_ago` seconds ago.
fn authentication_session(
    contact_id: &str,
    authorize_endpoint: &str,
    client_id: &str,
    scopes: &[&str],
    initiated_secs_ago: u64,
    cancelled: bool,
) -> participant::state::AuthenticationSession {
    participant::state::AuthenticationSession {
        contact_id: contact_id.to_string(),
        initiated_at: SystemTime::now() - std::time::Duration::from_secs(initiated_secs_ago),
        authorize_endpoint: authorize_endpoint.to_string(),
        client_id: client_id.to_string(),
        scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
        cancelled,
    }
}

/// `GetAuthenticationUrl`: `POST /participant/authentication-url`, the connection token in
/// the `X-Amz-Bearer` header.
async fn get_authentication_url(
    router: &MultiServiceRouter,
    connection_token: &str,
    session_id: &str,
    redirect_uri: &str,
) -> (u16, Value) {
    post(
        router,
        "/participant/authentication-url",
        Some(connection_token),
        &json!({"SessionId": session_id, "RedirectUri": redirect_uri}),
    )
    .await
}

/// `CancelParticipantAuthentication`: `POST /participant/cancel-authentication`.
async fn cancel_participant_authentication(
    router: &MultiServiceRouter,
    connection_token: &str,
    session_id: &str,
) -> (u16, Value) {
    post(
        router,
        "/participant/cancel-authentication",
        Some(connection_token),
        &json!({"SessionId": session_id}),
    )
    .await
}

/// The query parameters of `url`, decoded — read back with a URL parser, so what the
/// service encoded is checked by decoding it rather than by matching its text.
fn query_pairs(case: &str, url: &str) -> Vec<(String, String)> {
    let parsed = ::url::Url::parse(url)
        .unwrap_or_else(|error| panic!("case {case:?}: {url:?} is not a URL: {error}"));
    parsed
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect()
}

/// The single value `name` carries in `pairs`, or `None` when it carries none.
fn query_value<'pairs>(pairs: &'pairs [(String, String)], name: &str) -> Option<&'pairs str> {
    pairs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// The `ValidationException$Message` both authentication operations refuse a request whose
/// parameters they cannot act on with: AWS's fixed parameter-validation sentence, spelled
/// out here rather than read from the service so the assertion checks the wording instead
/// of restating the service's own constant.
const INVALID_PARAMETERS_MESSAGE: &str = "Specified request parameter(s) are invalid.";

/// Assert `body` is the parameter refusal both authentication operations answer with: HTTP
/// 400, `ValidationException`, AWS's fixed sentence, and no part of the request
/// (`session_id`, and whatever else `request_values` names) anywhere in the message.
fn assert_invalid_parameters(case: &str, status: u16, body: &Value, request_values: &[&str]) {
    assert_eq!(status, 400, "case {case:?}: got {body:?}");
    assert_eq!(
        error_type(body),
        "ValidationException",
        "case {case:?}: got {body:?}"
    );
    let message = error_message(body);
    assert_eq!(
        message, INVALID_PARAMETERS_MESSAGE,
        "case {case:?}: expected {INVALID_PARAMETERS_MESSAGE:?}, got {message:?}"
    );
    for value in request_values {
        assert!(
            !message.contains(value),
            "case {case:?}: the refusal must carry nothing of the request, \
             but {message:?} contains {value:?}"
        );
    }
}

/// One initiated authentication session, and the redirect a caller asks for it with.
struct AuthenticationCase {
    /// What the assertions name this row.
    name: &'static str,
    /// `SessionId`, which the model constrains to exactly 36 characters.
    session_id: &'static str,
    /// The Cognito user pool's authorize endpoint the flow block is configured with.
    authorize_endpoint: &'static str,
    /// The user-pool app client the customer is authenticated through.
    client_id: &'static str,
    /// The scopes that app client asks for.
    scopes: &'static [&'static str],
    /// `RedirectUri`, "The URL where the customer will be redirected after Amazon Cognito
    /// authorizes the user".
    redirect_uri: &'static str,
    /// How long ago the `authenticationInitiated` event was raised — inside the one minute
    /// `GetAuthenticationUrl` documents for every row here.
    initiated_secs_ago: u64,
}

/// The rows differ along what the sign-in URL is composed of: whether the authorize
/// endpoint already carries a query, how many scopes the app client asks for, and what the
/// redirect itself contains — plus how long ago the session was initiated.
fn authentication_cases() -> Vec<AuthenticationCase> {
    vec![
        AuthenticationCase {
            name: "a plain authorize endpoint and one scope",
            session_id: "11111111-2222-4333-8444-555555555555",
            authorize_endpoint:
                "https://chat-auth.auth.us-east-1.amazoncognito.com/oauth2/authorize",
            client_id: "1example23456789abcdefghij",
            scopes: &["openid"],
            redirect_uri: "https://example.com/connect/callback",
            initiated_secs_ago: 0,
        },
        AuthenticationCase {
            name: "an authorize endpoint that already carries a query parameter",
            session_id: "22222222-3333-4444-8555-666666666666",
            authorize_endpoint:
                "https://chat-auth.auth.eu-west-1.amazoncognito.com/oauth2/authorize?lang=fr",
            client_id: "2example23456789abcdefghij",
            scopes: &["openid", "email"],
            redirect_uri: "https://example.com/retour",
            initiated_secs_ago: 1,
        },
        AuthenticationCase {
            name: "an app client that asks for no scope at all",
            session_id: "33333333-4444-4555-8666-777777777777",
            authorize_endpoint: "https://pool-3.auth.us-west-2.amazoncognito.com/oauth2/authorize",
            client_id: "3example23456789abcdefghij",
            scopes: &[],
            redirect_uri: "https://example.com/callback?from=chat",
            initiated_secs_ago: 2,
        },
        AuthenticationCase {
            name: "several scopes, in the order the app client asks for them",
            session_id: "44444444-5555-4666-8777-888888888888",
            authorize_endpoint: "https://pool-4.auth.ap-south-1.amazoncognito.com/oauth2/authorize",
            client_id: "4example23456789abcdefghij",
            scopes: &["openid", "profile", "aws.cognito.signin.user.admin"],
            redirect_uri: "https://example.com/done",
            initiated_secs_ago: 3,
        },
        AuthenticationCase {
            name: "a redirect carrying a query, a space and non-ASCII text",
            session_id: "55555555-6666-4777-8888-999999999999",
            authorize_endpoint: "https://pool-5.auth.sa-east-1.amazoncognito.com/oauth2/authorize",
            client_id: "5example23456789abcdefghij",
            scopes: &["openid"],
            redirect_uri: "https://example.com/retour?état=prêt&next=my page#top",
            initiated_secs_ago: 4,
        },
        AuthenticationCase {
            name: "a session raised 30 seconds ago, inside the documented minute",
            session_id: "66666666-7777-4888-8999-aaaaaaaaaaaa",
            authorize_endpoint: "https://pool-6.auth.us-east-2.amazoncognito.com/oauth2/authorize",
            client_id: "6example23456789abcdefghij",
            scopes: &["openid"],
            redirect_uri: "https://example.com/late",
            initiated_secs_ago: 30,
        },
    ]
}

#[tokio::test]
async fn an_initiated_authentication_session_reports_its_cognito_sign_in_url() {
    let harness = router();
    let participant_token = "participant-token-authentication";
    let connection_token = connect(&harness, participant_token).await;
    let contact_id = harness.start_chat(participant_token);

    for case in authentication_cases() {
        // `SessionId` is modelled `length 36..36`, so every row names one of that length.
        assert_eq!(
            case.session_id.len(),
            36,
            "case {:?}: a SessionId is 36 characters, got {:?}",
            case.name,
            case.session_id
        );
        initiate_authentication(
            &harness,
            case.session_id,
            authentication_session(
                &contact_id,
                case.authorize_endpoint,
                case.client_id,
                case.scopes,
                case.initiated_secs_ago,
                false,
            ),
        );
    }

    for case in authentication_cases() {
        let label = case.name;
        let (status, body) =
            get_authentication_url(&harness, &connection_token, case.session_id, case.redirect_uri)
                .await;
        assert_eq!(status, 200, "case {label:?}: got {body:?}");
        let url = body["AuthenticationUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case {label:?}: no AuthenticationUrl in {body:?}"));
        // `AuthenticationUrl` is modelled `length 1..2083`.
        assert!(
            !url.is_empty() && url.chars().count() <= 2083,
            "case {label:?}: expected a 1..2083-character AuthenticationUrl, got {} characters",
            url.chars().count()
        );

        let endpoint = ::url::Url::parse(case.authorize_endpoint).expect("the authorize endpoint");
        let composed = ::url::Url::parse(url)
            .unwrap_or_else(|error| panic!("case {label:?}: {url:?} is not a URL: {error}"));
        assert_eq!(
            (composed.scheme(), composed.host_str(), composed.path()),
            (endpoint.scheme(), endpoint.host_str(), endpoint.path()),
            "case {label:?}: expected the stored authorize endpoint, got {url:?}"
        );

        let pairs = query_pairs(label, url);
        assert_eq!(
            query_value(&pairs, "response_type"),
            Some("code"),
            "case {label:?}: got {pairs:?}"
        );
        assert_eq!(
            query_value(&pairs, "client_id"),
            Some(case.client_id),
            "case {label:?}: got {pairs:?}"
        );
        assert_eq!(
            query_value(&pairs, "redirect_uri"),
            Some(case.redirect_uri),
            "case {label:?}: expected the RedirectUri the request named, got {pairs:?}"
        );
        assert_eq!(
            query_value(&pairs, "state"),
            Some(case.session_id),
            "case {label:?}: expected the SessionId as the OAuth state, got {pairs:?}"
        );
        let expected_scope = (!case.scopes.is_empty()).then(|| case.scopes.join(" "));
        assert_eq!(
            query_value(&pairs, "scope").map(str::to_string),
            expected_scope,
            "case {label:?}: got {pairs:?}"
        );
        // A query the endpoint already carried is still there: the parameters are added
        // to it rather than replacing it.
        for (name, value) in endpoint.query_pairs() {
            assert_eq!(
                query_value(&pairs, &name),
                Some(value.as_ref()),
                "case {label:?}: expected the endpoint's own {name}={value}, got {pairs:?}"
            );
        }
    }
}

#[tokio::test]
async fn reporting_an_authentication_url_leaves_what_a_later_caller_sees_unchanged() {
    let harness = router();
    let participant_token = "participant-token-reread-authentication";
    let connection_token = connect(&harness, participant_token).await;
    let contact_id = harness.start_chat(participant_token);
    let case = authentication_cases()
        .into_iter()
        .next()
        .expect("at least one authentication case");
    initiate_authentication(
        &harness,
        case.session_id,
        authentication_session(
            &contact_id,
            case.authorize_endpoint,
            case.client_id,
            case.scopes,
            case.initiated_secs_ago,
            false,
        ),
    );

    let (first_status, first) =
        get_authentication_url(&harness, &connection_token, case.session_id, case.redirect_uri)
            .await;
    assert_eq!(first_status, 200, "case first read: got {first:?}");
    let (second_status, second) =
        get_authentication_url(&harness, &connection_token, case.session_id, case.redirect_uri)
            .await;
    assert_eq!(second_status, 200, "case second read: got {second:?}");
    assert_eq!(
        first, second,
        "case repeated read: expected {first:?}, got {second:?}"
    );

    // The read consumed nothing: the session is still there, still not cancelled.
    {
        let store = harness.store();
        let state = store.lock();
        let session = state
            .authentication_sessions
            .get(case.session_id)
            .unwrap_or_else(|| {
                panic!(
                    "case repeated read: the session must still be stored, got {:?}",
                    state.authentication_sessions.keys().collect::<Vec<_>>()
                )
            });
        assert!(
            !session.cancelled,
            "case repeated read: reporting a URL must not cancel the session"
        );
    }

    // Cancelling it is what closes it: the opted-out branch is taken, and no URL is
    // reported for that session again — including to a second cancel.
    let (status, body) =
        cancel_participant_authentication(&harness, &connection_token, case.session_id).await;
    assert_eq!(status, 200, "case cancel: got {body:?}");
    assert!(
        harness
            .store()
            .lock()
            .authentication_sessions
            .get(case.session_id)
            .is_some_and(|session| session.cancelled),
        "case cancel: expected the stored session to be cancelled"
    );

    for (label, (status, body)) in [
        (
            "a URL for a cancelled session",
            get_authentication_url(&harness, &connection_token, case.session_id, case.redirect_uri)
                .await,
        ),
        (
            "a second cancel of the same session",
            cancel_participant_authentication(&harness, &connection_token, case.session_id).await,
        ),
    ] {
        assert_invalid_parameters(label, status, &body, &[case.session_id, case.redirect_uri]);
    }
}

#[tokio::test]
async fn a_session_id_that_names_no_current_authentication_session_is_refused() {
    let harness = router();
    let my_token = "participant-token-authentication-mine";
    let their_token = "participant-token-authentication-theirs";
    let mine = connect(&harness, my_token).await;
    let theirs = connect(&harness, their_token).await;
    let my_contact = harness.start_chat(my_token);
    let their_contact = harness.start_chat(their_token);

    let endpoint = "https://pool-x.auth.us-east-1.amazoncognito.com/oauth2/authorize";
    // My own current session, which must keep working after every refusal below.
    let current = "aaaaaaaa-1111-4111-8111-111111111111";
    initiate_authentication(
        &harness,
        current,
        authentication_session(&my_contact, endpoint, "client-current", &["openid"], 0, false),
    );
    // One of my own sessions already cancelled, one past the documented minute, and one
    // belonging to the other chat.
    let cancelled = "bbbbbbbb-2222-4222-8222-222222222222";
    initiate_authentication(
        &harness,
        cancelled,
        authentication_session(&my_contact, endpoint, "client-cancelled", &["openid"], 0, true),
    );
    let past_window = "cccccccc-3333-4333-8333-333333333333";
    initiate_authentication(
        &harness,
        past_window,
        authentication_session(&my_contact, endpoint, "client-late", &["openid"], 61, false),
    );
    let other_chats = "dddddddd-4444-4444-8444-444444444444";
    initiate_authentication(
        &harness,
        other_chats,
        authentication_session(&their_contact, endpoint, "client-other", &["openid"], 0, false),
    );

    // The rows differ in WHY the id names no current authentication session of this chat.
    let rows: &[(&str, &str)] = &[
        ("an id no session was initiated for", "00000000-0000-4000-8000-000000000000"),
        ("an id shaped like one of ours but unknown", "ffffffff-ffff-4fff-8fff-ffffffffffff"),
        ("a session already cancelled", cancelled),
        ("a session past the one-minute window", past_window),
        ("another chat's authentication session", other_chats),
    ];
    for (label, session_id) in rows {
        for (operation, (status, body)) in [
            (
                "GetAuthenticationUrl",
                get_authentication_url(&harness, &mine, session_id, "https://example.com/cb")
                    .await,
            ),
            (
                "CancelParticipantAuthentication",
                cancel_participant_authentication(&harness, &mine, session_id).await,
            ),
        ] {
            let case = format!("{operation} with {label}");
            assert_invalid_parameters(&case, status, &body, &[session_id]);
        }
    }

    // None of those refusals touched the session that IS current, or the other chat's.
    let (status, body) =
        get_authentication_url(&harness, &mine, current, "https://example.com/cb").await;
    assert_eq!(status, 200, "case my own current session: got {body:?}");
    let (status, body) =
        get_authentication_url(&harness, &theirs, other_chats, "https://example.com/cb").await;
    assert_eq!(status, 200, "case the other chat's own session: got {body:?}");
}

/// Every parameter refusal of the two authentication operations carries ONE fixed sentence,
/// whatever the request named and whatever the reason: a caller holding no session cannot
/// tell an unknown id from a cancelled one, from one past its window, or from another
/// chat's, and the sentence carries nothing of what it submitted.
#[tokio::test]
async fn authentication_parameter_refusals_all_carry_one_fixed_sentence() {
    let harness = router();
    let my_token = "participant-token-authentication-fixed-message";
    let their_token = "participant-token-authentication-fixed-message-other";
    let mine = connect(&harness, my_token).await;
    connect(&harness, their_token).await;
    let my_contact = harness.start_chat(my_token);
    let their_contact = harness.start_chat(their_token);
    let endpoint = "https://pool-f.auth.us-east-1.amazoncognito.com/oauth2/authorize";

    // Sessions that exist but are not ones this caller may act on, so the rows below cover
    // reasons that differ in STATE beside rows whose id names no session at all.
    let cancelled = "11111111-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    initiate_authentication(
        &harness,
        cancelled,
        authentication_session(
            &my_contact,
            endpoint,
            "client-fixed-cancelled",
            &["openid"],
            0,
            true,
        ),
    );
    let past_window = "22222222-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    initiate_authentication(
        &harness,
        past_window,
        authentication_session(&my_contact, endpoint, "client-fixed-late", &["openid"], 61, false),
    );
    let other_chats = "33333333-cccc-4ccc-8ccc-cccccccccccc";
    initiate_authentication(
        &harness,
        other_chats,
        authentication_session(
            &their_contact,
            endpoint,
            "client-fixed-other",
            &["openid"],
            0,
            false,
        ),
    );

    // The rows differ in the FORM of the 36-character `SessionId` (modelled @length min 36
    // / max 36, so every row is a value the model itself permits), in the redirect the
    // request asks for, and in why the id is not a session this caller may act on.
    let rows: &[(&str, &str, &str)] = &[
        (
            "a dashed uuid no session was initiated for",
            "00000000-0000-4000-8000-000000000000",
            "https://example.com/cb",
        ),
        (
            "an upper-case hexadecimal id",
            "ABCDEF0123456789ABCDEF0123456789ABCD",
            "https://example.com/callback?next=%2Fhome",
        ),
        (
            "an all-digit id",
            "012345678901234567890123456789012345",
            "https://sign-in.example.test/after",
        ),
        (
            "a dash-separated label rather than a uuid",
            "chatauth-session-aaaaaaaaaaaaaaaaaaa",
            "http://localhost:3000/cb",
        ),
        (
            "an id of punctuation only",
            "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!",
            "https://example.com/cb#fragment",
        ),
        (
            "my own session, already cancelled",
            cancelled,
            "https://example.com/cb",
        ),
        (
            "my own session, past the documented minute",
            past_window,
            "https://example.com/cb",
        ),
        (
            "another chat's current session",
            other_chats,
            "https://example.com/cb",
        ),
    ];

    let mut refusals: Vec<(String, String)> = Vec::new();
    for (label, session_id, redirect_uri) in rows {
        assert_eq!(
            session_id.chars().count(),
            36,
            "case {label:?}: a SessionId is 36 characters, got {}",
            session_id.chars().count()
        );
        for (operation, (status, body)) in [
            (
                "GetAuthenticationUrl",
                get_authentication_url(&harness, &mine, session_id, redirect_uri).await,
            ),
            (
                "CancelParticipantAuthentication",
                cancel_participant_authentication(&harness, &mine, session_id).await,
            ),
        ] {
            let case = format!("{operation} with {label}");
            assert_invalid_parameters(&case, status, &body, &[session_id, redirect_uri]);
            refusals.push((case, error_message(&body)));
        }
    }

    // Every row answered the SAME sentence, so no reason is distinguishable from another.
    let (ref first_case, ref first_message) = refusals[0];
    for (case, message) in &refusals {
        assert_eq!(
            message, first_message,
            "case {case:?}: expected the sentence {first_case:?} answered ({first_message:?}), \
             got {message:?}"
        );
    }
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

#[tokio::test]
async fn a_redirect_that_cannot_be_carried_in_the_authentication_url_is_refused() {
    let harness = router();
    let participant_token = "participant-token-long-redirect";
    let connection_token = connect(&harness, participant_token).await;
    let contact_id = harness.start_chat(participant_token);
    let session_id = "77777777-8888-4999-8aaa-bbbbbbbbbbbb";
    initiate_authentication(
        &harness,
        session_id,
        authentication_session(
            &contact_id,
            "https://pool-w.auth.us-east-1.amazoncognito.com/oauth2/authorize",
            "client-long-redirect",
            &["openid"],
            0,
            false,
        ),
    );

    // `RedirectUri` is modelled len 1..1024 and `AuthenticationUrl` len 1..2083, so a
    // redirect inside its own bound can still be one the answer cannot carry once it is
    // encoded. The rows differ in how long the ENCODED form of a legal-length redirect is.
    let rows: &[(&str, String, bool)] = &[
        (
            "a redirect at its modelled maximum length",
            format!("https://example.com/{}", "a".repeat(1004)),
            true,
        ),
        (
            "a legal-length redirect whose encoded form does not fit the AuthenticationUrl",
            format!("https://example.com/?q={}", "\"".repeat(1000)),
            false,
        ),
    ];
    for (case, redirect_uri, expected_served) in rows {
        // Every row is a RedirectUri the model itself permits.
        assert!(
            (1..=1024).contains(&redirect_uri.chars().count()),
            "case {case:?}: a RedirectUri is 1..1024 characters, got {}",
            redirect_uri.chars().count()
        );
        let (status, body) =
            get_authentication_url(&harness, &connection_token, session_id, redirect_uri).await;
        if *expected_served {
            assert_eq!(status, 200, "case {case:?}: got {body:?}");
            let url = body["AuthenticationUrl"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no AuthenticationUrl in {body:?}"));
            let pairs = query_pairs(case, url);
            assert_eq!(
                query_value(&pairs, "redirect_uri"),
                Some(redirect_uri.as_str()),
                "case {case:?}: expected the RedirectUri back, got {pairs:?}"
            );
            continue;
        }
        // A redirect the answer cannot carry is a parameter this operation refuses, so it
        // carries the same fixed sentence as every other parameter refusal — and neither
        // the redirect it was handed nor the length it composed appears in it.
        assert_invalid_parameters(case, status, &body, &[redirect_uri.as_str(), session_id]);
    }
}

#[tokio::test]
async fn an_authentication_call_with_a_token_that_authenticates_nobody_is_access_denied() {
    let harness = router();
    let live_token = "participant-token-authentication-denied";
    let live = connect(&harness, live_token).await;
    let contact_id = harness.start_chat(live_token);
    let session_id = "eeeeeeee-5555-4555-8555-555555555555";
    initiate_authentication(
        &harness,
        session_id,
        authentication_session(
            &contact_id,
            "https://pool-y.auth.us-east-1.amazoncognito.com/oauth2/authorize",
            "client-denied",
            &["openid"],
            0,
            false,
        ),
    );

    // A token of a participant that has left: valid once, and never again.
    let gone = connect(&harness, "participant-token-authentication-left").await;
    let (status, body) = post(&harness, "/participant/disconnect", Some(&gone), &json!({})).await;
    assert_eq!(status, 200, "DisconnectParticipant: got {body:?}");

    // The rows differ in WHICH token fails to authenticate a connected participant.
    let rows: &[(&str, &str)] = &[
        ("an unknown connection token", "bogus-connection-token"),
        (
            "a token shaped like one we mint",
            "00000000-0000-4000-8000-000000000000",
        ),
        ("a disconnected participant's token", &gone),
    ];
    for (label, token) in rows {
        for (operation, (status, body)) in [
            (
                "GetAuthenticationUrl",
                get_authentication_url(&harness, token, session_id, "https://example.com/cb")
                    .await,
            ),
            (
                "CancelParticipantAuthentication",
                cancel_participant_authentication(&harness, token, session_id).await,
            ),
        ] {
            let case = format!("{operation} with {label}");
            assert_eq!(status, 403, "case {case:?}: got {body:?}");
            assert_eq!(
                error_type(&body),
                "AccessDeniedException",
                "case {case:?}: got {body:?}"
            );
            assert_eq!(
                body["Message"].as_str(),
                Some("Access denied"),
                "case {case:?}: got {body:?}"
            );
        }
    }

    // The refusals took nothing away: the session the live token reaches still reports.
    let (status, body) =
        get_authentication_url(&harness, &live, session_id, "https://example.com/cb").await;
    assert_eq!(status, 200, "case the live token: got {body:?}");
}

#[tokio::test]
async fn an_authentication_session_is_current_for_exactly_the_documented_minute() {
    // `GetAuthenticationUrl`: "This API can only be called within one minute of receiving
    // the authenticationInitiated event." Driven off a fixed instant rather than the wall
    // clock, so the bound itself is what is checked.
    let initiated_at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let session = participant::state::AuthenticationSession {
        contact_id: "contact-window".to_string(),
        initiated_at,
        authorize_endpoint: "https://pool-z.auth.us-east-1.amazoncognito.com/oauth2/authorize"
            .to_string(),
        client_id: "client-window".to_string(),
        scopes: vec!["openid".to_string()],
        cancelled: false,
    };
    // The rows are the instant the session is judged at, around the published bound.
    let rows: &[(&str, std::time::Duration, bool)] = &[
        ("the instant it was initiated", std::time::Duration::ZERO, true),
        ("half a minute in", std::time::Duration::from_secs(30), true),
        (
            "a millisecond before the minute",
            std::time::Duration::from_millis(59_999),
            true,
        ),
        ("the minute itself", std::time::Duration::from_secs(60), false),
        (
            "a millisecond past the minute",
            std::time::Duration::from_millis(60_001),
            false,
        ),
    ];
    for (case, elapsed, expected) in rows {
        assert_eq!(
            session.is_current(initiated_at + *elapsed),
            *expected,
            "case {case:?}: expected is_current == {expected}, got {}",
            session.is_current(initiated_at + *elapsed)
        );
    }

    // A cancelled session is not current even inside the window.
    let cancelled = participant::state::AuthenticationSession {
        cancelled: true,
        ..session
    };
    assert!(
        !cancelled.is_current(initiated_at),
        "case a cancelled session: expected is_current == false, got true"
    );
}

/// One row of [`every_transcript_page_reports_its_pagination_token`]: how much the chat
/// holds and how large a page it is read at, which together decide how many pages the scan
/// has.
#[derive(Debug, Clone, Copy)]
struct PageCase {
    /// What this row is.
    case: &'static str,
    /// How many messages the chat holds before the scan.
    messages: usize,
    /// The `MaxResults` the scan is requested with; `None` leaves the member out, which the
    /// model documents as a page of 10.
    max_results: Option<i64>,
    /// How many pages the whole scan takes at that page size.
    pages: usize,
}

/// `GetTranscript` reports `NextToken` on EVERY page: a continuation while the scan has
/// more to serve, and the terminal token (the empty string) on the page that ends it.
#[tokio::test]
async fn every_transcript_page_reports_its_pagination_token() {
    let router = router();

    // The rows differ in how many items the scan covers and in the page size it is sliced
    // at, so they span a scan that fits one page, one that takes several, the documented
    // default page size, and a scan with nothing in it at all.
    let cases: &[PageCase] = &[
        PageCase { case: "one message, default page size", messages: 1, max_results: None, pages: 1 },
        PageCase { case: "three messages, one per page", messages: 3, max_results: Some(1), pages: 3 },
        PageCase { case: "three messages, two per page", messages: 3, max_results: Some(2), pages: 2 },
        PageCase { case: "three messages in one page", messages: 3, max_results: Some(3), pages: 1 },
        PageCase { case: "twelve messages, default page size", messages: 12, max_results: None, pages: 2 },
        PageCase { case: "three messages, the largest page", messages: 3, max_results: Some(100), pages: 1 },
        PageCase { case: "an empty transcript", messages: 0, max_results: Some(5), pages: 1 },
    ];

    for (
        index,
        PageCase {
            case,
            messages,
            max_results,
            pages: expected_pages,
        },
    ) in cases.iter().copied().enumerate()
    {
        let token = connect(&router, &format!("participant-token-pages-{index}")).await;
        for index in 0..messages {
            send_message(&router, &token, &format!("{case} message {index}")).await;
        }

        let mut request = json!({"SortOrder": "ASCENDING", "ScanDirection": "FORWARD"});
        if let Some(max_results) = max_results {
            request["MaxResults"] = json!(max_results);
        }
        let mut pages = 0usize;
        let mut served = 0usize;
        loop {
            let (status, body) =
                post(&router, "/participant/transcript", Some(&token), &request).await;
            assert_eq!(status, 200, "case {case:?}: got {body:?}");
            pages += 1;
            served += body["Transcript"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: no Transcript in {body:?}"))
                .len();
            let next = body
                .get("NextToken")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: page {pages} reported no NextToken: {body:?}")
                });
            if next.is_empty() {
                break;
            }
            assert!(
                pages < expected_pages,
                "case {case:?}: page {pages} of {expected_pages} reported a continuation \
                 {next:?} after serving {served} of {messages} items"
            );
            // A continuation re-states `MaxResults`, which is taken from the continuation
            // request, so every page of one row is sliced at that row's page size.
            request = json!({"NextToken": next});
            if let Some(max_results) = max_results {
                request["MaxResults"] = json!(max_results);
            }
        }
        assert_eq!(
            (pages, served),
            (expected_pages, messages),
            "case {case:?}: expected {expected_pages} page(s) covering {messages} item(s), \
             got {pages} page(s) covering {served}"
        );

        // The terminal token continues nothing: `NextToken` is modelled with a minimum
        // length of 1, so handing it back is refused rather than restarting the scan.
        let (status, body) = post(
            &router,
            "/participant/transcript",
            Some(&token),
            &json!({"NextToken": ""}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the terminal NextToken to be refused as input, \
             got {body:?}"
        );
    }
}

/// One row of [`message_metadata_is_reported_for_the_items_a_receipt_can_name`]: a kind of
/// transcript item, and whether `Item$MessageMetadata` belongs to it.
#[derive(Debug, Clone, Copy)]
struct MetadataCase {
    /// What this row is.
    case: &'static str,
    /// `Item$Type`, from `CHAT_ITEM_TYPE_VALUES`.
    item_type: &'static str,
    /// Whether an item of that type reports `MessageMetadata` while nobody has
    /// acknowledged it.
    metadata: bool,
}

/// Every transcript item a message receipt can name reports its own `MessageMetadata` —
/// naming itself, and carrying an EMPTY receipt list until a recipient acknowledges it.
#[tokio::test]
async fn message_metadata_is_reported_for_the_items_a_receipt_can_name() {
    let harness = router();
    let participant_token = "participant-token-metadata";
    let token = connect(&harness, participant_token).await;

    // The rows are the item types a transcript holds: the MESSAGE `SendMessage` appends,
    // the ATTACHMENT a confirmed upload appends, and a chat EVENT, which is not a message
    // and which no receipt names. (`CHAT_ITEM_TYPE_VALUES` in the service's shared types.)
    let cases: &[MetadataCase] = &[
        MetadataCase { case: "a sent message", item_type: "MESSAGE", metadata: true },
        MetadataCase { case: "a confirmed attachment", item_type: "ATTACHMENT", metadata: true },
        MetadataCase { case: "a chat event", item_type: "EVENT", metadata: false },
    ];

    let mut expected: Vec<(MetadataCase, String)> = Vec::new();
    for case in cases.iter().copied() {
        let id = match case.item_type {
            "MESSAGE" => send_message(&harness, &token, "metadata message").await,
            // The upload flow has its own suite (`tests/all/attachments.rs`); what this
            // suite needs is an item of each TYPE in the transcript, so the other two are
            // appended through the very state calls `CompleteAttachmentUpload` and a chat
            // event append with.
            other => append_stored_item(&harness, participant_token, other),
        };
        expected.push((case, id));
    }

    let (items, _) = get_transcript(
        &harness,
        &token,
        &json!({"MaxResults": 100, "SortOrder": "ASCENDING", "ScanDirection": "FORWARD"}),
    )
    .await;
    for (MetadataCase { case, item_type, metadata }, id) in &expected {
        let item = items
            .iter()
            .find(|item| item["Id"].as_str() == Some(id.as_str()))
            .unwrap_or_else(|| panic!("case {case:?}: no {item_type} item in {items:?}"));
        let reported = item.get("MessageMetadata");
        assert_eq!(
            reported.is_some(),
            *metadata,
            "case {case:?}: expected MessageMetadata presence {metadata}, got {item:?}"
        );
        if let Some(reported) = reported {
            assert_eq!(
                (
                    reported["MessageId"].as_str(),
                    reported["Receipts"].as_array().map(Vec::len)
                ),
                (Some(id.as_str()), Some(0)),
                "case {case:?}: expected the item's own Id and an empty receipt list, \
                 got {reported:?}"
            );
        }
    }

    // An acknowledgement is observable through that same member: the list that was empty
    // now carries the recipient's receipt.
    let message_id = expected
        .iter()
        .find(|(case, _)| case.item_type == "MESSAGE")
        .map(|(_, id)| id.clone())
        .expect("the message row");
    let (status, body) = post(
        &harness,
        "/participant/event",
        Some(&token),
        &json!({"ContentType": "application/vnd.amazonaws.connect.event.message.delivered",
                "Content": receipt_content(&message_id)}),
    )
    .await;
    assert_eq!(status, 200, "case a delivered receipt: got {body:?}");
    let (items, _) = get_transcript(
        &harness,
        &token,
        &json!({"MaxResults": 100, "SortOrder": "ASCENDING", "ScanDirection": "FORWARD"}),
    )
    .await;
    let item = items
        .iter()
        .find(|item| item["Id"].as_str() == Some(message_id.as_str()))
        .unwrap_or_else(|| panic!("case a delivered receipt: the message is not in {items:?}"));
    let receipts = item["MessageMetadata"]["Receipts"]
        .as_array()
        .unwrap_or_else(|| panic!("case a delivered receipt: expected Receipts on {item:?}"));
    assert_eq!(
        receipts.len(),
        1,
        "case a delivered receipt: one recipient acknowledged it, got {receipts:?}"
    );
    assert_eq!(
        receipts[0]["RecipientParticipantId"].as_str(),
        item["ParticipantId"].as_str(),
        "case a delivered receipt: expected the acknowledging participant, got {receipts:?}"
    );
}

/// Append an item of `item_type` to the transcript of the chat `participant_token` names,
/// through the state calls the service's own appenders use, and answer its `Id`.
fn append_stored_item(harness: &Harness, participant_token: &str, item_type: &str) -> String {
    let store = harness.store();
    let mut state = store.lock();
    let contact_id = state
        .participant_tokens
        .get(participant_token)
        .cloned()
        .expect("the chat the participant token names");
    let session = state
        .sessions
        .get_mut(&contact_id)
        .expect("the session the started chat registered");
    let now = SystemTime::now();
    match item_type {
        "ATTACHMENT" => session.append_attachment(
            participant::state::new_id(),
            "image/png".to_string(),
            now,
        ),
        other => {
            session
                .append(
                    other,
                    Some("{}".to_string()),
                    "application/vnd.amazonaws.connect.event.typing".to_string(),
                    None,
                    now,
                )
                .0
        }
    }
}
