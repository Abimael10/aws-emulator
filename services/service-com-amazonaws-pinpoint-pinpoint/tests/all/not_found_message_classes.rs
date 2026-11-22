// SPDX-License-Identifier: CC0-1.0

//! The TEXT a Pinpoint refusal carries, end to end over HTTP.
//!
//! Every case drives the real restJson1 wire through the `MultiServiceRouter` the
//! production front door uses, so routing, deserialization, the handler,
//! serialization and the HTTP status are all exercised.
//!
//! The rules under test, each asserted over a SET of inputs rather than one request:
//!
//! * a `NotFoundException` message is the FIXED text of the class of resource the
//!   request addressed and interpolates no value out of the request — not the
//!   identifier the caller named, nor any other request value — so one class reads the
//!   same whichever operation was asked and whichever identifier it was asked for: an
//!   application the service does not hold, and a resource OF an application that
//!   resolves to nothing, are two such classes;
//! * the one refusal that does name values states them as the service's template: a
//!   direct send over a channel the application has not enabled reports, per
//!   destination, the channel type that destination named and the application the
//!   request path named.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every case in this suite signs for; the suite asserts refusal text, not
/// ARNs.
const REGION: &str = "us-east-1";

/// The service's text for a request addressed at an application it does not hold,
/// written out here independently of the code under test.
const APPLICATION_NOT_FOUND: &str = "Application not found";

/// The service's generic text for a resource it does not hold, written out here
/// independently of the code under test.
const RESOURCE_NOT_FOUND: &str = "Resource not found";

/// The text the service answers with for a channel `channel_type` that `app` has not
/// enabled, written out here independently of the code under test.
fn expected_channel_not_found(channel_type: &str, app: &str) -> String {
    format!(
        "{channel_type} channel not found. Please enable the {channel_type} channel for this application {app}."
    )
}

/// The message member of `DirectMessageConfiguration` that carries the message for
/// `channel`, and a minimal message for it. Taken from the member list of
/// `DirectMessageConfiguration`
/// (`smithy_query(query="shape",shape="DirectMessageConfiguration")`).
fn message_configuration_for(channel: &str) -> Value {
    match channel {
        "BAIDU" => json!({"BaiduMessage": {"Body": "baidu body"}}),
        "GCM" => json!({"GCMMessage": {"Body": "gcm body"}}),
        "APNS" | "APNS_SANDBOX" | "APNS_VOIP" | "APNS_VOIP_SANDBOX" => {
            json!({"APNSMessage": {"Body": "apns body"}})
        }
        "ADM" => json!({"ADMMessage": {"Body": "adm body"}}),
        "SMS" => json!({"SMSMessage": {"Body": "sms body", "MessageType": "TRANSACTIONAL"}}),
        "VOICE" => json!({"VoiceMessage": {"Body": "voice body"}}),
        _ => json!({"DefaultMessage": {"Body": "default body"}}),
    }
}

/// A mounted emulator holding one shared engine, exactly as the router builds it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: it asserts service behaviour, not SigV4.
        router.set_request_authentication(false);
        router.register_service(service_com_amazonaws_pinpoint_pinpoint::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// Dispatch one request and return the status plus the parsed JSON body
    /// (`Value::Null` for an empty body).
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        let payload = body.map_or_else(Bytes::new, |v| {
            Bytes::from(serde_json::to_vec(&v).expect("a test body always serializes"))
        });
        let request = ::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            // The routing key: Pinpoint's sigv4 signing name is `mobiletargeting`.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/mobiletargeting/aws4_request, SignedHeaders=host, Signature=0000"
                ),
            )
            .body(payload)
            .expect("a well-formed test request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Create one application and return its minted id.
    async fn create_app(&self, name: &str) -> String {
        let (status, body) = self
            .call("POST", "/v1/apps", Some(json!({"Name": name})))
            .await;
        assert_eq!(status, 201, "CreateApp must answer 201, got {body:?}");
        body["Id"]
            .as_str()
            .expect("CreateApp answers with the minted Id")
            .to_string()
    }

    async fn get_app(&self, app: &str) -> (u16, Value) {
        self.call("GET", &format!("/v1/apps/{app}"), None).await
    }

    async fn delete_app(&self, app: &str) -> (u16, Value) {
        self.call("DELETE", &format!("/v1/apps/{app}"), None).await
    }

    async fn get_endpoint(&self, app: &str, endpoint: &str) -> (u16, Value) {
        self.call("GET", &format!("/v1/apps/{app}/endpoints/{endpoint}"), None)
            .await
    }

    async fn update_endpoint(&self, app: &str, endpoint: &str, body: Value) -> (u16, Value) {
        self.call(
            "PUT",
            &format!("/v1/apps/{app}/endpoints/{endpoint}"),
            Some(body),
        )
        .await
    }

    async fn get_baidu_channel(&self, app: &str) -> (u16, Value) {
        self.call("GET", &format!("/v1/apps/{app}/channels/baidu"), None)
            .await
    }

    async fn send_messages(&self, app: &str, request: Value) -> (u16, Value) {
        self.call("POST", &format!("/v1/apps/{app}/messages"), Some(request))
            .await
    }
}

/// The modelled error code carried by a restJson1 error body (`__type`).
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The `Message` member of a refusal body, or the empty string when it carries none.
fn message_of(body: &Value) -> String {
    body["Message"].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn an_application_level_refusal_is_fixed_text_that_echoes_no_request_value() {
    let emulator = Emulator::new();

    // An id the service itself minted and a delete removed: that id was real, so no row
    // can be answered off the SHAPE of the identifier.
    let minted_then_deleted = emulator.create_app("deleted").await;
    let (status, body) = emulator.delete_app(&minted_then_deleted).await;
    assert_eq!(
        status, 200,
        "setup: DeleteApp must succeed, got {status} with {body:?}"
    );

    // Rows along the form of the identifier the caller names.
    let identifiers: Vec<(&str, String)> = vec![
        (
            "a well-formed id that was never minted",
            "00000000000000000000000000000000".to_string(),
        ),
        ("an id minted and then deleted", minted_then_deleted),
        ("an id shorter than a minted one", "ab".to_string()),
        (
            "an id outside the minted alphabet",
            "not-a-real-application".to_string(),
        ),
        ("an id longer than a minted one", "c".repeat(80)),
    ];

    for (case, id) in identifiers {
        // And rows along the operation asked: a read, a delete, a read of a resource of
        // the application, and a send to it.
        let answers: Vec<(&str, (u16, Value))> = vec![
            ("GetApp", emulator.get_app(&id).await),
            ("DeleteApp", emulator.delete_app(&id).await),
            ("GetEndpoint", emulator.get_endpoint(&id, "any").await),
            (
                "SendMessages",
                emulator
                    .send_messages(
                        &id,
                        json!({
                            "Addresses": {"some-token": {"ChannelType": "BAIDU"}},
                            "MessageConfiguration": message_configuration_for("BAIDU"),
                        }),
                    )
                    .await,
            ),
        ];

        for (operation, (status, body)) in answers {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (404, "NotFoundException"),
                "case {case:?}/{operation}: expected a 404 NotFoundException, got {body:?}"
            );
            let message = message_of(&body);
            assert_eq!(
                message, APPLICATION_NOT_FOUND,
                "case {case:?}/{operation}: expected Message {APPLICATION_NOT_FOUND:?}, got {message:?}"
            );
            assert!(
                !message.contains(&id),
                "case {case:?}/{operation}: the refusal carries no identifier out of the request, got {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_resource_a_live_application_does_not_hold_is_the_generic_refusal() {
    let emulator = Emulator::new();

    let app = emulator.create_app("live").await;
    let other = emulator.create_app("other").await;
    for (owner, endpoint) in [(&app, "known"), (&other, "held-elsewhere")] {
        let (status, body) = emulator
            .update_endpoint(
                owner,
                endpoint,
                json!({"Address": format!("{endpoint}-token"), "ChannelType": "BAIDU"}),
            )
            .await;
        assert_eq!(
            status, 202,
            "setup: the endpoint write must be accepted, got {status} with {body:?}"
        );
    }

    // Rows along the form of the endpoint id named on an application that IS stored,
    // including one that is a real endpoint id — of another application.
    let endpoints: Vec<(&str, String)> = vec![
        (
            "an id that was never written",
            "no-such-endpoint".to_string(),
        ),
        ("an id outside the stored alphabet", "~endpoint~".to_string()),
        ("an id far longer than any stored", "e".repeat(80)),
        (
            "an id another application holds",
            "held-elsewhere".to_string(),
        ),
    ];

    for (case, endpoint_id) in endpoints {
        let (status, body) = emulator.get_endpoint(&app, &endpoint_id).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "NotFoundException"),
            "case {case:?}: expected a 404 NotFoundException, got {body:?}"
        );
        let message = message_of(&body);
        assert_eq!(
            message, RESOURCE_NOT_FOUND,
            "case {case:?}: expected Message {RESOURCE_NOT_FOUND:?}, got {message:?}"
        );
        assert!(
            !message.contains(&endpoint_id) && !message.contains(app.as_str()),
            "case {case:?}: the refusal carries neither identifier out of the request, got {message:?}"
        );
    }

    // A channel of an application that is not stored is the same class: the caller
    // addressed a resource of an application, and no resource resolved.
    let (status, body) = emulator.delete_app(&other).await;
    assert_eq!(
        status, 200,
        "setup: DeleteApp must succeed, got {status} with {body:?}"
    );
    let (status, body) = emulator.get_baidu_channel(&other).await;
    assert_eq!(
        (status, error_code(&body).as_str(), message_of(&body).as_str()),
        (404, "NotFoundException", RESOURCE_NOT_FOUND),
        "case \"a channel of an application that is not stored\": expected {RESOURCE_NOT_FOUND:?}, got {body:?}"
    );

    // None of the refusals above disturbed what the application DOES hold.
    let (status, body) = emulator.get_endpoint(&app, "known").await;
    assert_eq!(
        status, 200,
        "the stored endpoint still reads back after every refusal, got {status} with {body:?}"
    );
}

#[tokio::test]
async fn the_refusal_classes_of_one_application_are_distinct_fixed_texts() {
    let emulator = Emulator::new();
    let app = emulator.create_app("classes").await;

    // The application is stored, so the refusals below differ only in the class of
    // resource each request addressed.
    let (status, body) = emulator.get_endpoint(&app, "absent").await;
    assert_eq!(
        status, 404,
        "GetEndpoint on an absent endpoint is a 404, got {body:?}"
    );
    let resource_level = message_of(&body);

    let (status, body) = emulator.get_baidu_channel(&app).await;
    assert_eq!(
        status, 404,
        "GetBaiduChannel on an absent channel is a 404, got {body:?}"
    );
    let channel_level = message_of(&body);

    let (status, body) = emulator.delete_app(&app).await;
    assert_eq!(status, 200, "DeleteApp must succeed, got {body:?}");
    let (status, body) = emulator.get_app(&app).await;
    assert_eq!(
        status, 404,
        "GetApp on a deleted application is a 404, got {body:?}"
    );
    let application_level = message_of(&body);

    assert_eq!(
        (
            application_level.as_str(),
            resource_level.as_str(),
            channel_level.as_str(),
        ),
        (
            APPLICATION_NOT_FOUND,
            RESOURCE_NOT_FOUND,
            expected_channel_not_found("BAIDU", &app).as_str(),
        ),
        "each class answers with its own text, got {application_level:?} / {resource_level:?} / {channel_level:?}"
    );
    assert!(
        application_level != resource_level
            && resource_level != channel_level
            && application_level != channel_level,
        "the three classes must not share one text, got {application_level:?} / {resource_level:?} / {channel_level:?}"
    );
}

#[tokio::test]
async fn a_send_over_an_unenabled_channel_names_that_channel_and_that_application() {
    let emulator = Emulator::new();

    // Two applications, so the identifier in the message is this request's path and not
    // a constant; and several channels, so the channel type is this destination's.
    let first = emulator.create_app("send-first").await;
    let second = emulator.create_app("send-second").await;

    for (case, app, other) in [
        ("the first application", &first, &second),
        ("the second application", &second, &first),
    ] {
        for channel in ["BAIDU", "GCM", "APNS", "SMS"] {
            let endpoint_id = format!("{}-endpoint", channel.to_ascii_lowercase());
            let endpoint_address = format!("{}-endpoint-address", channel.to_ascii_lowercase());
            let address = format!("{}-address", channel.to_ascii_lowercase());
            let (status, body) = emulator
                .update_endpoint(
                    app,
                    &endpoint_id,
                    json!({"Address": endpoint_address, "ChannelType": channel}),
                )
                .await;
            assert_eq!(
                status, 202,
                "case {case:?}/{channel}: the endpoint write must be accepted, got {body:?}"
            );

            // The same refused destination both ways round: named by its address, and
            // named by the id of an endpoint that carries one.
            let (status, body) = emulator
                .send_messages(
                    app,
                    json!({
                        "Addresses": {address.clone(): {"ChannelType": channel}},
                        "Endpoints": {endpoint_id.clone(): {}},
                        "MessageConfiguration": message_configuration_for(channel),
                    }),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}/{channel}: SendMessages answers 200 overall, got {body:?}"
            );

            let expected = expected_channel_not_found(channel, app);
            for (destination, outcome) in [
                ("Result", body["Result"][&address].clone()),
                (
                    "EndpointResult",
                    body["EndpointResult"][&endpoint_id].clone(),
                ),
            ] {
                assert_eq!(
                    (
                        outcome["DeliveryStatus"].as_str(),
                        outcome["StatusCode"].as_i64(),
                        outcome["StatusMessage"].as_str(),
                    ),
                    (
                        Some("PERMANENT_FAILURE"),
                        Some(404),
                        Some(expected.as_str())
                    ),
                    "case {case:?}/{channel}/{destination}: expected StatusMessage {expected:?}, got {outcome:?}"
                );
                let reported = outcome["StatusMessage"].as_str().unwrap_or_default();
                assert!(
                    !reported.contains(other.as_str()),
                    "case {case:?}/{channel}/{destination}: the message names the application of THIS request, got {reported:?}"
                );
            }
        }
    }
}
