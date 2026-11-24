// SPDX-License-Identifier: CC0-1.0

//! The downstream push provider's verdict on a direct send (`SendMessages`), end to
//! end over HTTP.
//!
//! Every case drives the real restJson1 wire through the `MultiServiceRouter` the
//! production front door uses, so routing, deserialization, the handler,
//! serialization and the HTTP status are all exercised.
//!
//! The rules under test, each asserted over a SET of inputs rather than one request:
//!
//! * delivery over a push channel the API configures with a provider's credentials is
//!   performed by that provider, which accepts only a device token it holds a
//!   registration for — so a destination of an ENABLED push channel whose address no
//!   endpoint of the application registers for that channel is refused by the
//!   provider: `DeliveryStatus` `PERMANENT_FAILURE`, the provider's own downstream
//!   `StatusCode`, no message id, and a `StatusMessage` that is the provider's refusal
//!   document, which a client parses as JSON for its `errorMessage` and `channelType`;
//! * enabling such a channel therefore changes the destination's FAILURE MODE — from
//!   this service's own channel verdict to the provider's — and not only the wording of
//!   it;
//! * a channel whose destination is an address a caller names rather than a token a
//!   provider issues, and a `ChannelType` with no channel resource of its own, are not
//!   gated on a registration at all.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every case in this suite signs for; the suite asserts delivery
/// behaviour, not ARNs.
const REGION: &str = "us-east-1";

/// Every `ChannelType` whose delivery a third-party push provider performs: the push
/// values of the model's `ChannelType` enum that the API also configures as a channel
/// resource of the application, carrying that provider's credentials
/// (`smithy_query(query="shape",shape="ChannelType")` lists the enum; this crate's
/// OPERATIONS.md lists the `/v1/apps/{ApplicationId}/channels/...` operations).
const PROVIDER_PUSH_CHANNELS: &[&str] = &[
    "ADM",
    "APNS",
    "APNS_SANDBOX",
    "APNS_VOIP",
    "APNS_VOIP_SANDBOX",
    "BAIDU",
    "GCM",
];

/// The configurable channels whose destination is an address the caller names — a
/// phone number or a mailbox — rather than a device token a push provider issued: the
/// non-push values of the same enum that have a channel resource.
const ADDRESSED_CHANNELS: &[&str] = &["EMAIL", "SMS", "VOICE"];

/// The `ChannelType` values with no channel resource of their own, so there is no
/// provider configured for them to be refused by: the same enum, minus the ten
/// configurable ones.
const UNGATED_CHANNELS: &[&str] = &["PUSH", "CUSTOM", "IN_APP"];

/// The fixed text the push provider's refusal document carries for a notification
/// addressed at a token it holds no registration for. Written out here independently
/// of the service code under test.
const EXPECTED_PROVIDER_ERROR: &str = "Invalid notification";

/// A mounted emulator holding one shared engine, exactly as the router builds it.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: it asserts service behaviour, not SigV4.
        router.set_request_authentication(false);
        router.register_service(service_com_amazonaws_pinpoint_pinpoint::EmulatedService::new(
            Arc::clone(&engine),
        ));
        Self { router, engine }
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

    async fn send_messages(&self, app: &str, request: Value) -> (u16, Value) {
        self.call("POST", &format!("/v1/apps/{app}/messages"), Some(request))
            .await
    }

    async fn update_baidu_channel(&self, app: &str, request: Value) -> (u16, Value) {
        self.call(
            "PUT",
            &format!("/v1/apps/{app}/channels/baidu"),
            Some(request),
        )
        .await
    }

    /// Register `address` as an endpoint of `app` for `channel`, which is what this
    /// service holds as a registration of a device token.
    async fn register(&self, app: &str, endpoint_id: &str, address: &str, channel: &str) {
        let (status, body) = self
            .call(
                "PUT",
                &format!("/v1/apps/{app}/endpoints/{endpoint_id}"),
                Some(json!({"Address": address, "ChannelType": channel})),
            )
            .await;
        assert_eq!(
            status, 202,
            "UpdateEndpoint for {endpoint_id} must answer 202, got {body:?}"
        );
    }

    /// Enable one channel of `app` in the store.
    ///
    /// Only `UpdateBaiduChannel` is served over the wire so far, so the other nine
    /// channels are configured through the store the handler reads — the same shared
    /// state that operation writes.
    fn enable_channel(&self, app: &str, channel: &str) {
        let store = service_com_amazonaws_pinpoint_pinpoint::state::service_state(&*self.engine)
            .expect("the shared engine holds this service's state");
        let mut guard = store.lock();
        let application = guard
            .apps
            .get_mut(app)
            .expect("the application this case created");
        application.upsert_channel(channel, true, Some(format!("{channel}-credential")));
    }
}

/// The message member of `DirectMessageConfiguration` that carries the message for
/// `channel`, and a minimal message for it. Taken from the member list of
/// `DirectMessageConfiguration`
/// (`smithy_query(query="shape",shape="DirectMessageConfiguration")`); a channel with
/// no member of its own is sent the default message for all channels.
fn message_configuration_for(channel: &str) -> Value {
    match channel {
        "GCM" => json!({"GCMMessage": {"Body": "gcm body"}}),
        "APNS" | "APNS_SANDBOX" | "APNS_VOIP" | "APNS_VOIP_SANDBOX" => {
            json!({"APNSMessage": {"Body": "apns body"}})
        }
        "ADM" => json!({"ADMMessage": {"Body": "adm body"}}),
        "BAIDU" => json!({"BaiduMessage": {"Body": "baidu body"}}),
        "SMS" => json!({"SMSMessage": {"Body": "sms body", "MessageType": "TRANSACTIONAL"}}),
        "VOICE" => json!({"VoiceMessage": {"Body": "voice body"}}),
        "EMAIL" => {
            json!({"EmailMessage": {"FromAddress": "sender@example.com", "SimpleEmail": {
                "Subject": {"Data": "subject"}, "TextPart": {"Data": "text"}}}})
        }
        _ => json!({"DefaultMessage": {"Body": "default body"}}),
    }
}

/// Assert `outcome` is the push provider's refusal of a token it holds no
/// registration for: `PERMANENT_FAILURE` with the provider's own downstream status,
/// no message id for an undelivered notification, and the provider's refusal document
/// as the `StatusMessage` — parsed here the way a client that reads `errorMessage` and
/// `channelType` out of it does.
pub fn assert_provider_rejection(case: &str, outcome: &Value, channel: &str) {
    assert_eq!(
        outcome["DeliveryStatus"].as_str(),
        Some("PERMANENT_FAILURE"),
        "case {case:?}: expected the provider's permanent failure, got {outcome:?}"
    );
    assert_eq!(
        outcome["StatusCode"].as_i64(),
        Some(400),
        "case {case:?}: expected the provider's downstream status 400, got {outcome:?}"
    );
    assert!(
        outcome["MessageId"].as_str().is_none(),
        "case {case:?}: a notification the provider refused has no message id, got {outcome:?}"
    );
    let reported = outcome["StatusMessage"].as_str().unwrap_or_default();
    let document: Value = serde_json::from_str(reported).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: expected the StatusMessage to be the provider's JSON document, \
             got {reported:?} ({error})"
        )
    });
    assert_eq!(
        (
            document["errorMessage"].as_str(),
            document["channelType"].as_str(),
        ),
        (Some(EXPECTED_PROVIDER_ERROR), Some(channel)),
        "case {case:?}: expected the provider's refusal of a {channel} notification, \
         got {document:?}"
    );
}

/// Assert `outcome` is an accepted delivery: `SUCCESSFUL` with the service's own
/// status and a minted message id.
fn assert_accepted(case: &str, outcome: &Value) {
    assert_eq!(
        (
            outcome["DeliveryStatus"].as_str(),
            outcome["StatusCode"].as_i64(),
        ),
        (Some("SUCCESSFUL"), Some(200)),
        "case {case:?}: expected an accepted delivery, got {outcome:?}"
    );
    let message_id = outcome["MessageId"].as_str().unwrap_or_default();
    assert!(
        message_id.len() == 32 && message_id.bytes().all(|b| b.is_ascii_hexdigit()),
        "case {case:?}: an accepted delivery mints a message id, got {outcome:?}"
    );
}

#[tokio::test]
async fn a_push_to_a_token_the_provider_holds_no_registration_for_is_refused_by_it() {
    let emulator = Emulator::new();

    // One row per channel whose delivery a push provider performs: the same send,
    // once to a token nothing registers and once to one an endpoint of the
    // application registers for that channel.
    for channel in PROVIDER_PUSH_CHANNELS {
        let app = emulator.create_app(&format!("provider-{channel}")).await;
        emulator.enable_channel(&app, channel);
        let unregistered = format!("{}-unregistered-token", channel.to_ascii_lowercase());
        let registered = format!("{}-registered-token", channel.to_ascii_lowercase());

        let (status, body) = emulator
            .send_messages(
                &app,
                json!({
                    "Addresses": {unregistered.clone(): {"ChannelType": channel}},
                    "MessageConfiguration": message_configuration_for(channel),
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {channel:?}: a send the provider refuses is still a 200 overall, got {body:?}"
        );
        assert_provider_rejection(channel, &body["Result"][&unregistered], channel);

        // Registering the token for that channel is the write that makes the provider
        // hold a registration for it, so the SAME send is delivered.
        emulator
            .register(&app, "registered-endpoint", &registered, channel)
            .await;
        let (_, body) = emulator
            .send_messages(
                &app,
                json!({
                    "Addresses": {registered.clone(): {"ChannelType": channel}},
                    "MessageConfiguration": message_configuration_for(channel),
                }),
            )
            .await;
        assert_accepted(
            &format!("{channel}: a registered token"),
            &body["Result"][&registered],
        );

        // And the same destination named by the id of the endpoint that registers it.
        let (_, body) = emulator
            .send_messages(
                &app,
                json!({
                    "Endpoints": {"registered-endpoint": {}},
                    "MessageConfiguration": message_configuration_for(channel),
                }),
            )
            .await;
        assert_accepted(
            &format!("{channel}: the endpoint that registers the token"),
            &body["EndpointResult"]["registered-endpoint"],
        );
    }
}

#[tokio::test]
async fn a_token_registered_for_one_push_channel_is_not_a_token_of_another() {
    let emulator = Emulator::new();
    let app = emulator.create_app("cross-channel-token").await;
    let address = "one-providers-token";
    emulator.register(&app, "baidu-endpoint", address, "BAIDU").await;

    // Both channels enabled, so neither send is gated on the channel: what separates
    // them is which provider issued the token the endpoint registers.
    for channel in ["BAIDU", "GCM"] {
        emulator.enable_channel(&app, channel);
    }

    let (_, body) = emulator
        .send_messages(
            &app,
            json!({
                "Addresses": {address: {"ChannelType": "GCM"}},
                "MessageConfiguration": message_configuration_for("GCM"),
            }),
        )
        .await;
    assert_provider_rejection(
        "a Baidu token addressed over GCM",
        &body["Result"][address],
        "GCM",
    );

    let (_, body) = emulator
        .send_messages(
            &app,
            json!({
                "Addresses": {address: {"ChannelType": "BAIDU"}},
                "MessageConfiguration": message_configuration_for("BAIDU"),
            }),
        )
        .await;
    assert_accepted(
        "the same token addressed over the channel it is registered for",
        &body["Result"][address],
    );
}

#[tokio::test]
async fn enabling_a_push_channel_moves_the_destination_to_the_providers_verdict() {
    let emulator = Emulator::new();
    let app = emulator.create_app("channel-state-transition").await;
    let address = "baidu-push-token";
    let send = json!({
        "Addresses": {address: {"ChannelType": "BAIDU"}},
        "MessageConfiguration": {"BaiduMessage": {"Body": "baidu body", "Title": "hello"}},
    });

    // Before the channel is enabled the notification never reaches a provider, so the
    // destination carries this service's own channel verdict, in prose.
    let (status, body) = emulator.send_messages(&app, send.clone()).await;
    assert_eq!(status, 200, "SendMessages must answer 200, got {body:?}");
    let before = body["Result"][address].clone();
    let expected = format!(
        "BAIDU channel not found. Please enable the BAIDU channel for this application {app}."
    );
    assert_eq!(
        (
            before["DeliveryStatus"].as_str(),
            before["StatusCode"].as_i64(),
            before["StatusMessage"].as_str(),
        ),
        (
            Some("PERMANENT_FAILURE"),
            Some(404),
            Some(expected.as_str())
        ),
        "case \"before enabling\": expected this service's channel verdict, got {before:?}"
    );

    let (status, channel) = emulator
        .update_baidu_channel(
            &app,
            json!({"ApiKey": "baidu-api-key", "SecretKey": "baidu-secret-key", "Enabled": true}),
        )
        .await;
    assert_eq!(
        (status, channel["Enabled"].as_bool()),
        (200, Some(true)),
        "UpdateBaiduChannel must enable the channel, got {channel:?}"
    );

    // After it, the same send reaches the provider, which refuses the token: a
    // DIFFERENT failure mode, not the same one re-worded and not a delivery.
    let (status, body) = emulator.send_messages(&app, send).await;
    assert_eq!(status, 200, "SendMessages must answer 200, got {body:?}");
    let after = body["Result"][address].clone();
    assert_provider_rejection("after enabling", &after, "BAIDU");
    assert_ne!(
        before["StatusCode"].as_i64(),
        after["StatusCode"].as_i64(),
        "enabling the channel must change the destination's status, got {before:?} then {after:?}"
    );
}

#[tokio::test]
async fn a_destination_no_push_provider_delivers_is_not_gated_on_a_registration() {
    let emulator = Emulator::new();

    // A phone number or a mailbox is an address the caller names, not a token a
    // provider issued, so an enabled channel of one delivers to an address this
    // application holds no endpoint for.
    for channel in ADDRESSED_CHANNELS {
        let app = emulator.create_app(&format!("addressed-{channel}")).await;
        emulator.enable_channel(&app, channel);
        let address = format!("{}-destination", channel.to_ascii_lowercase());
        let (status, body) = emulator
            .send_messages(
                &app,
                json!({
                    "Addresses": {address.clone(): {"ChannelType": channel}},
                    "MessageConfiguration": message_configuration_for(channel),
                }),
            )
            .await;
        assert_eq!(status, 200, "case {channel:?}: expected 200, got {body:?}");
        assert_accepted(channel, &body["Result"][&address]);
    }

    // A `ChannelType` with no channel resource has no provider to configure either,
    // so there is nothing for one to refuse.
    for channel in UNGATED_CHANNELS {
        let app = emulator.create_app(&format!("ungated-{channel}")).await;
        let address = format!("{}-destination", channel.to_ascii_lowercase());
        let (status, body) = emulator
            .send_messages(
                &app,
                json!({
                    "Addresses": {address.clone(): {"ChannelType": channel}},
                    "MessageConfiguration": message_configuration_for(channel),
                }),
            )
            .await;
        assert_eq!(status, 200, "case {channel:?}: expected 200, got {body:?}");
        assert_accepted(channel, &body["Result"][&address]);
    }
}
