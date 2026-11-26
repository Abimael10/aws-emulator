// SPDX-License-Identifier: CC0-1.0

//! Which VALUES an event destination definition may hand this service, and which it may
//! not.
//!
//! RULE: whether a definition passes an IAM role another account holds is decided from the
//! VALUE of every ARN the definition carries, never from the member the value arrives in —
//! so the same foreign role ARN is refused HTTP 403 `AccessDeniedException` with
//! "Cross-account pass role is not allowed." in EVERY ARN-bearing member of the three
//! destination shapes, nothing is stored for the refused request, and a value that is not a
//! foreign role (this account's role, a role ARN naming no account, another IAM entity, the
//! resource kind the member usually carries, a value that is not an ARN at all) is served
//! and read back wherever it is sent.
//!
//! The rows differ along what the rule generalises over: which member carries the value,
//! the form of the role ARN (partition, account, a path in the role name), the operation
//! handed it (`Create...` and `Update...`), and — for the served half — what kind of value
//! it is.
//!
//! The suite drives the real `restJson1` router, so the requests here are the bytes an AWS
//! SDK `pinpoint-sms-voice` client sends. The account holds the entitlement this API's
//! resource-scoped actions need, which `legacy_surface_authorization.rs` owns; the refusal
//! is answered ahead of that decision either way.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pinpointsmsvoice_pinpointsmsvoice::{
    ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState, EmulatedService,
};

/// The configuration set every row works inside.
const SET: &str = "passed-role-set";

/// The destination seeded through the API before each update row, so an update that is
/// refused has something it would otherwise have rewritten.
const BASELINE_DESTINATION: &str = "passed-role-baseline";

/// The baseline destination's one stored member, which a refused update must leave alone.
const BASELINE_TOPIC: &str = "arn:aws:sns:us-east-1:000000000000:voice-baseline";

/// The HTTP status the cross-account pass-role refusal carries.
const DENIED_STATUS: u16 = 403;
/// The error code it carries, which a client branches on.
const DENIED_CODE: &str = "AccessDeniedException";
/// The whole sentence it carries.
const DENIED_MESSAGE: &str = "Cross-account pass role is not allowed.";

/// Every ARN-bearing member of the shapes `EventDestinationDefinition` nests, as
/// (destination member, string member): the complete set of string members those three
/// shapes declare, from
/// `smithy_query(query="shape",shape="CloudWatchLogsDestination,KinesisFirehoseDestination,SnsDestination")`.
/// A value reaches this service through one of these and through nothing else.
const ARN_MEMBERS: &[(&str, &str)] = &[
    ("CloudWatchLogsDestination", "IamRoleArn"),
    ("CloudWatchLogsDestination", "LogGroupArn"),
    ("KinesisFirehoseDestination", "DeliveryStreamArn"),
    ("KinesisFirehoseDestination", "IamRoleArn"),
    ("SnsDestination", "TopicArn"),
];

/// The emulator under test, over the one shared engine.
struct Harness {
    router: MultiServiceRouter,
}

/// One HTTP answer: the status and the parsed JSON body (`Value::Null` when empty).
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The `__type` discriminator of a `restJson1` error envelope, or `""` on a success.
    fn error_code(&self) -> &str {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The detail text under the member this service reports it in.
    fn message(&self) -> &str {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The `EventDestinations` member of a `GetConfigurationSetEventDestinations` answer.
    fn destinations(&self) -> Vec<Value> {
        self.body
            .get("EventDestinations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// The reported destination of this name, or `None` when the set holds none.
    fn destination(&self, name: &str) -> Option<Value> {
        self.destinations()
            .into_iter()
            .find(|reported| reported.get("Name").and_then(Value::as_str) == Some(name))
    }

    /// What the reported destination of this name carries in one member of one of its
    /// nested destination shapes, or `None` when it carries nothing there.
    fn member_of(&self, name: &str, destination: &str, member: &str) -> Option<String> {
        self.destination(name)?
            .get(destination)?
            .get(member)?
            .as_str()
            .map(str::to_string)
    }
}

impl Harness {
    /// A harness holding one configuration set and one baseline destination inside it,
    /// both created through the API.
    async fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite drives unsigned requests; signature verification is the framework's
        // own concern and is covered where it lives.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        // The account holds the entitlement this API's resource-scoped actions need, so
        // the rows below reach the handlers; the entitlement itself is pinned in
        // `legacy_surface_authorization.rs`.
        engine
            .state::<ComAmazonawsPinpointsmsvoicePinpointsmsvoiceState>()
            .lock()
            .legacy_surface_entitled = true;
        let harness = Self { router };

        let created = harness
            .call(
                "POST",
                "/v1/sms-voice/configuration-sets",
                Some(json!({ "ConfigurationSetName": SET })),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "the suite's configuration set must be created, got {} {:?}",
            created.status, created.body
        );
        let seeded = harness
            .create_destination(
                BASELINE_DESTINATION,
                json!({ "SnsDestination": { "TopicArn": BASELINE_TOPIC } }),
            )
            .await;
        assert_eq!(
            seeded.status, 200,
            "the suite's baseline destination must be created, got {} {:?}",
            seeded.status, seeded.body
        );
        harness
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Answer {
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
        Answer { status, body }
    }

    async fn create_destination(&self, name: &str, definition: Value) -> Answer {
        self.call(
            "POST",
            &format!("/v1/sms-voice/configuration-sets/{SET}/event-destinations"),
            Some(json!({ "EventDestinationName": name, "EventDestination": definition })),
        )
        .await
    }

    async fn update_destination(&self, name: &str, definition: Value) -> Answer {
        self.call(
            "PUT",
            &format!("/v1/sms-voice/configuration-sets/{SET}/event-destinations/{name}"),
            Some(json!({ "EventDestination": definition })),
        )
        .await
    }

    async fn read_destinations(&self) -> Answer {
        self.call(
            "GET",
            &format!("/v1/sms-voice/configuration-sets/{SET}/event-destinations"),
            None,
        )
        .await
    }
}

/// A definition that carries exactly one value, in one member of one destination shape.
fn one_member(destination: &str, member: &str, value: &str) -> Value {
    json!({ destination: { member: value } })
}

/// The account every ARN this emulator mints carries, so a role under it is one this
/// service may be handed.
fn this_account() -> &'static str {
    engine::emulator_account_id()
}

/// RULE: a foreign role ARN is refused whatever member of the definition carries it, and
/// nothing of the refused request is stored — on `Create...` and on `Update...` alike.
#[tokio::test]
async fn a_foreign_role_is_refused_in_every_member_that_can_carry_it() {
    // The forms differ in partition, in which other account holds the role, and in whether
    // the role name carries a path — none of which changes the decision.
    let role_forms: &[(&str, &str)] = &[
        ("a plain role of another account", "arn:aws:iam::123456789012:role/voice"),
        (
            "a role under a path in another account",
            "arn:aws:iam::210987654321:role/service-role/voice",
        ),
        (
            "a role of another account in another partition",
            "arn:aws-cn:iam::123456789012:role/voice",
        ),
    ];

    let harness = Harness::new().await;

    for &(destination, member) in ARN_MEMBERS {
        for (form, role_arn) in role_forms {
            let case = format!("{form} in {destination}${member}");
            let definition = one_member(destination, member, role_arn);
            let created_name = format!("refused-{destination}-{member}");

            for (operation, answer) in [
                (
                    "CreateConfigurationSetEventDestination",
                    harness
                        .create_destination(&created_name, definition.clone())
                        .await,
                ),
                (
                    "UpdateConfigurationSetEventDestination",
                    harness
                        .update_destination(BASELINE_DESTINATION, definition.clone())
                        .await,
                ),
            ] {
                assert_eq!(
                    answer.status, DENIED_STATUS,
                    "case {case:?} on {operation}: expected HTTP {DENIED_STATUS}, got {} {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.error_code(),
                    DENIED_CODE,
                    "case {case:?} on {operation}: expected the code {DENIED_CODE}, got {:?}",
                    answer.body
                );
                assert_eq!(
                    answer.message(),
                    DENIED_MESSAGE,
                    "case {case:?} on {operation}: expected the message {DENIED_MESSAGE:?}, \
                     got {:?}",
                    answer.body
                );
            }

            // Nothing of a refused request reaches the store: the create left no
            // destination behind, and the update left the baseline exactly as it was.
            let read = harness.read_destinations().await;
            assert!(
                read.destination(&created_name).is_none(),
                "case {case:?}: a refused create must store no destination, got {:?}",
                read.destination(&created_name)
            );
            assert_eq!(
                read.member_of(BASELINE_DESTINATION, "SnsDestination", "TopicArn")
                    .as_deref(),
                Some(BASELINE_TOPIC),
                "case {case:?}: a refused update must leave the stored destination as it was, \
                 got {:?}",
                read.destination(BASELINE_DESTINATION)
            );
        }
    }
}

/// RULE (the other side): a value that is not a role another account holds is served
/// wherever it is sent and read back verbatim — the decision is about the value, so no
/// member is refused for its name and none is excused by it.
#[tokio::test]
async fn a_value_that_names_no_foreign_role_is_served_in_every_member() {
    let harness = Harness::new().await;
    let this_account = this_account();

    // (why this value passes, the value)
    let served_values: Vec<(&str, String)> = vec![
        (
            "a role of this account",
            format!("arn:aws:iam::{this_account}:role/voice"),
        ),
        (
            "a role ARN naming no account at all",
            "arn:aws:iam:::role/voice".to_string(),
        ),
        (
            // Another IAM entity is not a role, so there is no role being passed.
            "an IAM user of another account",
            "arn:aws:iam::123456789012:user/voice".to_string(),
        ),
        (
            // The kind of resource the model documents for a CloudWatch Logs destination's
            // log group member: "The name of the Amazon CloudWatch Log Group that you want
            // to record events in."
            "a CloudWatch log group of another account",
            "arn:aws:logs:us-east-1:123456789012:log-group:/voice:*".to_string(),
        ),
        (
            "a Firehose delivery stream of another account",
            "arn:aws:firehose:us-east-1:123456789012:deliverystream/voice".to_string(),
        ),
        (
            "an SNS topic of another account",
            "arn:aws:sns:us-east-1:123456789012:voice".to_string(),
        ),
        (
            // Not an ARN at all: what is wrong with it is its shape, which this decision
            // does not judge.
            "a value that is not an ARN",
            "voice-role".to_string(),
        ),
    ];

    for &(destination, member) in ARN_MEMBERS {
        for (index, (why, value)) in served_values.iter().enumerate() {
            let case = format!("{why} in {destination}${member}");
            let name = format!("served-{destination}-{member}-{index}");
            let definition = one_member(destination, member, value);

            let created = harness.create_destination(&name, definition.clone()).await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: expected the create to be served, got {} {:?}",
                created.status, created.body
            );

            let read = harness.read_destinations().await;
            assert_eq!(
                read.member_of(&name, destination, member).as_deref(),
                Some(value.as_str()),
                "case {case:?}: expected the stored destination to report the value sent, \
                 got {:?}",
                read.destination(&name)
            );

            // The same value through the update path, onto the destination just created.
            let updated = harness.update_destination(&name, definition).await;
            assert_eq!(
                updated.status, 200,
                "case {case:?}: expected the update to be served, got {} {:?}",
                updated.status, updated.body
            );
            let read = harness.read_destinations().await;
            assert_eq!(
                read.member_of(&name, destination, member).as_deref(),
                Some(value.as_str()),
                "case {case:?}: expected the updated destination to report the value sent, \
                 got {:?}",
                read.destination(&name)
            );
        }
    }
}
