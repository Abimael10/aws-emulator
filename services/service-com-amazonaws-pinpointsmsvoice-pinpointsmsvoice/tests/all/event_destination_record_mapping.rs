// SPDX-License-Identifier: CC0-1.0

//! How the three nested destination shapes of an event destination
//! (`CloudWatchLogsDestination`, `KinesisFirehoseDestination`, `SnsDestination`) are
//! stored, read back through `GetConfigurationSetEventDestinations`.
//!
//! RULE: a stored destination record is the member-for-member image of the wire shape it
//! was built from, whichever writer built it — so the same definition reaches the same
//! stored destination through `CreateConfigurationSetEventDestination` and through
//! `UpdateConfigurationSetEventDestination`, and reads back identically. The members of
//! the three shapes come from
//! `smithy_query(query="shape",shape="CloudWatchLogsDestination,KinesisFirehoseDestination,SnsDestination",model_path="services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/smithy-model/com-amazonaws-pinpointsmsvoice-pinpointsmsvoice.json")`:
//! `IamRoleArn`/`LogGroupArn`, `DeliveryStreamArn`/`IamRoleArn`, and `TopicArn`, every one
//! of them optional.
//!
//! The rows span which of the three shapes the definition carries, which of each shape's
//! members it fills, and whether it carries all three at once; each row is driven through
//! BOTH writers and the two read-backs are compared with each other as well as with what
//! was sent, so a mapping that one writer carries and the other drops fails the row.
//!
//! The suite drives the real `restJson1` router, so the requests here are the bytes an AWS
//! SDK `pinpoint-sms-voice` client sends. The account holds the entitlement this API's
//! resource-scoped actions need, which `legacy_surface_authorization.rs` owns.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pinpointsmsvoice_pinpointsmsvoice::{
    ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState, EmulatedService,
};

/// The configuration set every row works inside.
const SET: &str = "record-mapping-set";

/// The destination name every row files its destination under.
const DESTINATION: &str = "dest";

/// The emulator under test, over the one shared engine.
struct Harness {
    router: MultiServiceRouter,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite drives unsigned requests; signature verification is the framework's
        // own concern and is covered where it lives.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        // The account holds the entitlement this API's resource-scoped actions need, so
        // the rows below exercise what those actions DO; the entitlement itself is pinned
        // in `legacy_surface_authorization.rs`.
        engine
            .state::<ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState>()
            .lock()
            .legacy_surface_entitled = true;
        Self { router }
    }

    /// One HTTP call: the status and the parsed JSON body (`Value::Null` when empty).
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let payload = body.map_or_else(bytes::Bytes::new, |value| {
            bytes::Bytes::from(value.to_string())
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!(
                "https://sms-voice.pinpoint.us-east-1.amazonaws.com{path}"
            ))
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/sms-voice/aws4_request, \
                 SignedHeaders=host, Signature=00",
            )
            .header("content-type", "application/json")
            .body(payload)
            .expect("the test request must build");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let raw = response.into_body();
        let body = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or(Value::Null)
        };
        (status, body)
    }

    async fn create_set(&self) -> (u16, Value) {
        self.call(
            "POST",
            "/v1/sms-voice/configuration-sets",
            Some(json!({ "ConfigurationSetName": SET })),
        )
        .await
    }

    async fn create_destination(&self, definition: Value) -> (u16, Value) {
        self.call(
            "POST",
            &format!("/v1/sms-voice/configuration-sets/{SET}/event-destinations"),
            Some(json!({ "EventDestinationName": DESTINATION, "EventDestination": definition })),
        )
        .await
    }

    async fn update_destination(&self, definition: Value) -> (u16, Value) {
        self.call(
            "PUT",
            &format!(
                "/v1/sms-voice/configuration-sets/{SET}/event-destinations/{DESTINATION}"
            ),
            Some(json!({ "EventDestination": definition })),
        )
        .await
    }

    /// The one stored destination, as `GetConfigurationSetEventDestinations` reports it.
    async fn read_destination(&self, case: &str) -> Value {
        let (status, body) = self
            .call(
                "GET",
                &format!("/v1/sms-voice/configuration-sets/{SET}/event-destinations"),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read-back must answer 200, got {status} {body}"
        );
        let destinations = body
            .get("EventDestinations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            destinations.len(),
            1,
            "case {case:?}: expected exactly the one stored destination, got {body}"
        );
        destinations
            .into_iter()
            .next()
            .expect("the list holds one destination")
    }
}

/// The nested destinations of `definition`, keyed by member name: the part of the read-back
/// this suite is about, with `Name`, `Enabled` and `MatchingEventTypes` (which no row
/// varies) left out.
fn nested_destinations(definition: &Value) -> Map<String, Value> {
    [
        "CloudWatchLogsDestination",
        "KinesisFirehoseDestination",
        "SnsDestination",
    ]
    .into_iter()
    .filter_map(|member| {
        definition
            .get(member)
            .map(|value| (member.to_string(), value.clone()))
    })
    .collect()
}

/// RULE: whichever writer stored it, a nested destination reads back member for member as
/// it was sent — so the two writers agree.
///
/// Each row is created in one emulator and written by an update over an unrelated starting
/// definition in another, and the two read-backs must be the same value and the value sent.
#[tokio::test]
async fn a_nested_destination_reads_back_as_sent_through_either_writer() {
    // (case, the definition the row sends)
    //
    // The ARNs name the account this emulator serves, because a role ARN of another
    // account is refused by authorization before any of this runs
    // (`legacy_surface_authorization.rs` owns that rule).
    let rows: &[(&str, Value)] = &[
        (
            "the CloudWatch Logs target, both members",
            json!({ "CloudWatchLogsDestination": {
                "IamRoleArn": "arn:aws:iam::000000000000:role/voice-logs",
                "LogGroupArn": "arn:aws:logs:us-east-1:000000000000:log-group:/voice:*",
            } }),
        ),
        (
            "the CloudWatch Logs target, the role alone",
            json!({ "CloudWatchLogsDestination": {
                "IamRoleArn": "arn:aws:iam::000000000000:role/voice-logs",
            } }),
        ),
        (
            "the CloudWatch Logs target, the log group alone",
            json!({ "CloudWatchLogsDestination": {
                "LogGroupArn": "arn:aws:logs:us-east-1:000000000000:log-group:/other:*",
            } }),
        ),
        (
            "the Firehose target, both members",
            json!({ "KinesisFirehoseDestination": {
                "DeliveryStreamArn": "arn:aws:firehose:us-east-1:000000000000:deliverystream/voice",
                "IamRoleArn": "arn:aws:iam::000000000000:role/voice-firehose",
            } }),
        ),
        (
            "the Firehose target, the stream alone",
            json!({ "KinesisFirehoseDestination": {
                "DeliveryStreamArn": "arn:aws:firehose:us-east-1:000000000000:deliverystream/other",
            } }),
        ),
        (
            "the Firehose target, the role alone",
            json!({ "KinesisFirehoseDestination": {
                "IamRoleArn": "arn:aws:iam::000000000000:role/other-firehose",
            } }),
        ),
        (
            "the SNS target",
            json!({ "SnsDestination": { "TopicArn": "arn:aws:sns:us-east-1:000000000000:voice" } }),
        ),
        (
            "all three targets at once",
            json!({
                "CloudWatchLogsDestination": {
                    "IamRoleArn": "arn:aws:iam::000000000000:role/all-logs",
                    "LogGroupArn": "arn:aws:logs:us-east-1:000000000000:log-group:/all:*",
                },
                "KinesisFirehoseDestination": {
                    "DeliveryStreamArn": "arn:aws:firehose:us-east-1:000000000000:deliverystream/all",
                    "IamRoleArn": "arn:aws:iam::000000000000:role/all-firehose",
                },
                "SnsDestination": { "TopicArn": "arn:aws:sns:us-east-1:000000000000:all" },
            }),
        ),
        ("no target at all", json!({ "Enabled": true })),
    ];

    /// What an update starts from, so the update path rewrites an existing record rather
    /// than one the row itself wrote: a target the rows never send.
    fn starting_definition() -> Value {
        json!({
            "Enabled": false,
            "SnsDestination": { "TopicArn": "arn:aws:sns:us-east-1:000000000000:starting" },
        })
    }

    for (case, definition) in rows {
        let expected = nested_destinations(definition);

        // Writer 1: the create handler builds the record unconditionally.
        let created = Harness::new();
        assert_eq!(
            created.create_set().await.0,
            200,
            "case {case:?}: the set must be created"
        );
        let (status, body) = created.create_destination(definition.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the create must answer 200, got {status} {body}"
        );
        let after_create = nested_destinations(&created.read_destination(case).await);
        assert_eq!(
            Value::Object(after_create.clone()),
            Value::Object(expected.clone()),
            "case {case:?}: expected {} from the create path, got {}",
            Value::Object(expected.clone()),
            Value::Object(after_create.clone())
        );

        // Writer 2: the update handler merges the members the request carries into a
        // record that already exists.
        let updated = Harness::new();
        assert_eq!(
            updated.create_set().await.0,
            200,
            "case {case:?}: the set must be created"
        );
        assert_eq!(
            updated.create_destination(starting_definition()).await.0,
            200,
            "case {case:?}: the starting destination must be created"
        );
        let (status, body) = updated.update_destination(definition.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the update must answer 200, got {status} {body}"
        );
        let after_update = nested_destinations(&updated.read_destination(case).await);

        // Every target the row sent reads back exactly as the create path stored it; a
        // target the row did NOT send is the one the starting definition left in place,
        // which is the update handler's own merge rule and not this mapping's business.
        for (member, value) in &expected {
            assert_eq!(
                after_update.get(member),
                Some(value),
                "case {case:?}: expected {member} to read back as {value} from the update path, got {:?}",
                after_update.get(member)
            );
        }
        for (member, value) in &after_create {
            if expected.contains_key(member) {
                assert_eq!(
                    after_update.get(member),
                    Some(value),
                    "case {case:?}: the two writers disagree about {member}: create stored {value}, update stored {:?}",
                    after_update.get(member)
                );
            }
        }
    }
}
