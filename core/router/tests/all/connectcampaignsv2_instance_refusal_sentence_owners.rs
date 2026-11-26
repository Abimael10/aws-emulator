// SPDX-License-Identifier: CC0-1.0

//! Which ConnectCampaignsV2 operation states which instance-scoped 404 sentence.
//!
//! The service words a refusal about a `connectInstanceId` it cannot serve with one of
//! five sentences, each composed in ONE place in that crate's `state` module, and each
//! stated by a fixed set of operations: the sentence a 404 carries is the one the
//! ANSWERING operation's own wire states, so two operations that state one sentence
//! cannot disagree about a request they both refuse, and an operation that states
//! another is not dragged onto theirs.
//!
//! This suite pins that mapping over the wire for every instance-scoped operation,
//! including the onboarding START — whose wire reports the instance ID as unresolved
//! and which no suite drove through this class before — so the crate's single statement
//! of which operations share which sentence is checked against the handlers rather than
//! maintained by hand beside them.
//!
//! It is cross-service because the instance lookups these refusals report on are not
//! this service's alone: an instance exists because Amazon Connect's own registry holds
//! it, and Campaigns adds its own onboarding on top, so the rows below reach every
//! refusal by moving one instance through BOTH stores — unknown to Connect, held by
//! Connect but never onboarded, and onboarded here with its configuration deleted
//! since.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_connect_amazonconnectservice::state as connect_state;
use service_com_amazonaws_connectcampaignsv2_amazonconnectcampaignservicev2::state as campaigns_state;
use smithy_server_runtime::MultiServiceRouter;

/// The region every row signs for.
const REGION: &str = "us-east-1";

/// How long a row waits for the clock-owed onboarding job to settle before giving up:
/// generous, because the wait is on the job's reported status and not on a span of the
/// clock.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// Which of the service's instance-scoped sentences an operation's own wire states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sentence {
    /// The instance's Campaigns CONFIGURATION reported as not found, naming it.
    Config,
    /// The instance reported as not usable by Campaigns.
    Instance,
    /// The instance reported as not onboarded.
    NotOnboarded,
    /// The instance ID itself reported as unresolved, naming it.
    InstanceId,
    /// The instance's onboarding JOB record reported as not existing.
    Job,
}

impl Sentence {
    /// The text this sentence carries for `instance`, taken from the ONE place the
    /// service composes it — so a row asserts which sentence an operation states, which
    /// is what the handlers and the crate's statement of the mapping have to agree on.
    /// The wording of each sentence is pinned by the owning crate's own suites.
    fn text(self, instance: &str) -> String {
        match self {
            Sentence::Config => campaigns_state::instance_config_not_found_message(instance),
            Sentence::Instance => campaigns_state::INSTANCE_NOT_FOUND_MESSAGE.to_string(),
            Sentence::NotOnboarded => campaigns_state::INSTANCE_NOT_ONBOARDED_MESSAGE.to_string(),
            Sentence::InstanceId => campaigns_state::instance_id_not_found_message(instance),
            Sentence::Job => campaigns_state::onboarding_job_not_found_message(instance),
        }
    }
}

/// The router under test together with the engine both services share.
struct Harness {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

/// Amazon Connect and ConnectCampaignsV2 mounted on ONE engine, as the assembled
/// emulator mounts them, over a fresh store.
fn harness() -> Harness {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the core's concern; the authorization header below is a
    // routing signal only.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_connect_amazonconnectservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_connectcampaignsv2_amazonconnectcampaignservicev2::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    Harness { router, engine }
}

/// Drive one ConnectCampaignsV2 request and answer its status and parsed body.
async fn call(harness: &Harness, method: &str, path: &str, body: &Value) -> (u16, Value) {
    let payload = if body.is_null() {
        Bytes::new()
    } else {
        Bytes::from(serde_json::to_vec(body).expect("a test body always serializes"))
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://localhost{path}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/connect-campaigns/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed test request");
    let response = harness.router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The error code and message a refusal carries (restJson1 puts them in `__type` and
/// `message`).
fn refusal(body: &Value) -> (&str, &str) {
    (
        body.get("__type")
            .and_then(Value::as_str)
            .unwrap_or("<no __type>"),
        body.get("message")
            .and_then(Value::as_str)
            .unwrap_or("<no message>"),
    )
}

/// Instance ids spanning the `InstanceId` shape (len 1..256, pattern
/// `^[-_.a-zA-Z0-9]+$`, from `smithy_query(query="operation",
/// operation="GetConnectInstanceConfig")`): the shortest id, the uuid form a Connect
/// instance id takes, and every punctuation character the pattern admits.
fn instance_ids() -> Vec<(&'static str, String)> {
    vec![
        ("one character (length minimum)", "a".to_string()),
        (
            "uuid form",
            "3f2504e0-4f89-41d3-9a0c-0305e82c3301".to_string(),
        ),
        ("every admitted punctuation", "A-z_0.9".to_string()),
    ]
}

/// One request to drive against `instance`, and the sentence the answering operation's
/// own wire states.
struct Row {
    operation: &'static str,
    method: &'static str,
    path: String,
    body: Value,
    sentence: Sentence,
}

/// Every operation that resolves a `connectInstanceId` against ConnectCampaignsV2's
/// instance store, with the request bodies the model requires (from
/// `smithy_query(query="digest")` of that service's model).
fn instance_scoped_rows(instance: &str) -> Vec<Row> {
    let instance_path = format!("/v2/connect-instance/{instance}");
    let lambda = json!({"lambda": {
        "functionArn": "arn:aws:lambda:us-east-1:000000000000:function:sentence-probe",
    }});
    vec![
        Row {
            operation: "GetConnectInstanceConfig",
            method: "GET",
            path: format!("{instance_path}/config"),
            body: Value::Null,
            sentence: Sentence::Config,
        },
        Row {
            operation: "PutConnectInstanceIntegration",
            method: "PUT",
            path: format!("{instance_path}/integrations"),
            body: json!({"integrationConfig": lambda}),
            sentence: Sentence::Config,
        },
        Row {
            operation: "DeleteConnectInstanceIntegration",
            method: "POST",
            path: format!("{instance_path}/integrations/delete"),
            body: json!({"integrationIdentifier": lambda}),
            sentence: Sentence::Config,
        },
        Row {
            operation: "DeleteConnectInstanceConfig",
            method: "DELETE",
            path: format!("{instance_path}/config"),
            body: Value::Null,
            sentence: Sentence::NotOnboarded,
        },
        Row {
            operation: "GetInstanceCommunicationLimits",
            method: "GET",
            path: format!("{instance_path}/communication-limits"),
            body: Value::Null,
            sentence: Sentence::Instance,
        },
        Row {
            operation: "PutInstanceCommunicationLimits",
            method: "PUT",
            path: format!("{instance_path}/communication-limits"),
            body: json!({"communicationLimitsConfig": {"allChannelSubtypes": {
                "communicationLimitsList": [
                    {"maxCountPerRecipient": 2, "frequency": 1, "unit": "DAY"},
                ],
            }}}),
            sentence: Sentence::Instance,
        },
        Row {
            operation: "ListConnectInstanceIntegrations",
            method: "GET",
            path: format!("{instance_path}/integrations"),
            body: Value::Null,
            sentence: Sentence::Instance,
        },
        Row {
            operation: "CreateCampaign",
            method: "PUT",
            path: "/v2/campaigns".to_string(),
            body: json!({
                "name": "sentence-probe-campaign",
                "connectInstanceId": instance,
                // `outboundMode` and `defaultOutboundConfig` are both required.
                "channelSubtypeConfig": {"telephony": {
                    "outboundMode": {"agentless": {}},
                    "defaultOutboundConfig": {"connectContactFlowId": "flow-0001"},
                }},
            }),
            sentence: Sentence::Instance,
        },
        Row {
            operation: "GetInstanceOnboardingJobStatus",
            method: "GET",
            path: format!("{instance_path}/onboarding"),
            body: Value::Null,
            sentence: Sentence::Job,
        },
        Row {
            operation: "DeleteInstanceOnboardingJob",
            method: "DELETE",
            path: format!("{instance_path}/onboarding"),
            body: Value::Null,
            sentence: Sentence::Job,
        },
    ]
}

/// How far `instance` got through the two stores before a row ran.
#[derive(Clone, Copy, Debug)]
enum Reach {
    /// Amazon Connect holds no instance under the id, so there was never anything to
    /// onboard.
    UnknownToConnect,
    /// Connect's registry holds the instance, but Campaigns was never asked to onboard
    /// it.
    RegisteredOnly,
    /// Campaigns onboarded it here and its configuration was deleted since.
    ConfigDeleted,
}

/// Put `instance` into the state `reach` describes, in a freshly built harness.
async fn set_up(reach: Reach, instance: &str) -> Harness {
    let harness = harness();
    match reach {
        Reach::UnknownToConnect => {}
        Reach::RegisteredOnly => register_connect_instance(&harness, instance),
        Reach::ConfigDeleted => {
            onboard(&harness, instance).await;
            let (status, body) = call(
                &harness,
                "DELETE",
                &format!("/v2/connect-instance/{instance}/config"),
                &Value::Null,
            )
            .await;
            assert_eq!(
                status, 200,
                "reach {reach:?} / {instance}: expected the configuration to be deleted, \
                 got {status} {body}"
            );
        }
    }
    harness
}

/// Give the account the Connect instance `instance` in Amazon Connect's OWN registry
/// inside the shared engine — the registry every Campaigns instance gate is resolved
/// against.
///
/// Written through the registration seam the Connect crate exposes rather than over
/// `CreateInstance`, which mints the id it stores while these rows name their own.
fn register_connect_instance(harness: &Harness, instance: &str) {
    assert!(
        connect_state::register_instance(&*harness.engine, REGION, instance),
        "instance {instance:?}: Amazon Connect's registry took no record"
    );
}

/// Onboard `instance` to Campaigns and return once its clock-owed job reports
/// SUCCEEDED, so the instance really holds a configuration.
async fn onboard(harness: &Harness, instance: &str) {
    register_connect_instance(harness, instance);
    let (status, body) = call(
        harness,
        "PUT",
        &format!("/v2/connect-instance/{instance}/onboarding"),
        &json!({"encryptionConfig": {"enabled": false}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "instance {instance}: expected the onboarding job to start, got {status} {body}"
    );
    // The wait is on the job's reported STATUS, not on a span of the clock: it is read
    // again until it has settled, under a deadline generous enough for a loaded host.
    let deadline = Instant::now() + campaigns_state::ONBOARDING_JOB_DURATION + SETTLE_DEADLINE;
    loop {
        let (status, body) = call(
            harness,
            "GET",
            &format!("/v2/connect-instance/{instance}/onboarding"),
            &Value::Null,
        )
        .await;
        let reported = body
            .pointer("/connectInstanceOnboardingJobStatus/status")
            .and_then(Value::as_str);
        if status == 200 && reported == Some(campaigns_state::ONBOARDING_STATUS_SUCCEEDED) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "instance {instance}: the onboarding job never reported SUCCEEDED, last \
             answer {status} {body}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn each_instance_scoped_refusal_states_its_own_operations_sentence() {
    for setup in [
        Reach::UnknownToConnect,
        Reach::RegisteredOnly,
        Reach::ConfigDeleted,
    ] {
        for (id_case, instance) in instance_ids() {
            let harness = set_up(setup, &instance).await;
            for row in instance_scoped_rows(&instance) {
                let case = format!("{} / {id_case} / {setup:?}", row.operation);
                let (status, body) = call(&harness, row.method, &row.path, &row.body).await;
                let (code, message) = refusal(&body);
                assert_eq!(
                    status, 404,
                    "case {case:?}: expected HTTP 404 for an instance this service cannot \
                     serve, got {status} {body}"
                );
                assert!(
                    code.ends_with("ResourceNotFoundException"),
                    "case {case:?}: expected ResourceNotFoundException, got {code:?}"
                );
                assert_eq!(
                    message,
                    row.sentence.text(&instance),
                    "case {case:?}: expected the {:?} sentence, got {message:?}",
                    row.sentence
                );
            }
        }
    }
}

#[tokio::test]
async fn the_onboarding_start_states_the_instance_id_as_unresolved() {
    // The onboarding START is the one operation whose wire reports the instance ID
    // itself — not the instance's usability and not its configuration — so it is driven
    // on its own: for every other reach of the stores it is SERVED rather than refused.
    for (id_case, instance) in instance_ids() {
        let harness = set_up(Reach::UnknownToConnect, &instance).await;
        let (status, body) = call(
            &harness,
            "PUT",
            &format!("/v2/connect-instance/{instance}/onboarding"),
            &json!({"encryptionConfig": {"enabled": false}}),
        )
        .await;
        let (code, message) = refusal(&body);
        assert!(
            status == 404 && code.ends_with("ResourceNotFoundException"),
            "case {id_case:?}: expected the onboarding start's own 404 for an instance \
             Amazon Connect does not hold, got {status} {code:?} {body}"
        );
        assert_eq!(
            message,
            Sentence::InstanceId.text(&instance),
            "case {id_case:?}: expected the instance-id sentence, got {message:?}"
        );

        // And once Connect holds the instance, that same call is SERVED — so the row
        // above is the instance lookup failing and not the operation being broken.
        register_connect_instance(&harness, &instance);
        let (status, body) = call(
            &harness,
            "PUT",
            &format!("/v2/connect-instance/{instance}/onboarding"),
            &json!({"encryptionConfig": {"enabled": false}}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {id_case:?}: expected the onboarding start to be served for a registered \
             instance, got {status} {body}"
        );
    }
}

#[tokio::test]
async fn deleting_a_configuration_twice_states_the_not_onboarded_sentence() {
    // The second delete is the arm that finds no configuration to remove — the one
    // whose sentence is the service's NOT ONBOARDED one and not its configuration one,
    // which is what distinguishes the removal of a configuration from the READ of it.
    let instance = "deleted-twice-instance";
    let harness = harness();
    onboard(&harness, instance).await;
    let path = format!("/v2/connect-instance/{instance}/config");

    let (status, body) = call(&harness, "DELETE", &path, &Value::Null).await;
    assert_eq!(
        status, 200,
        "case {:?}: expected the first delete to remove the configuration, got {status} {body}",
        "first DeleteConnectInstanceConfig"
    );

    let (status, body) = call(&harness, "DELETE", &path, &Value::Null).await;
    let (code, message) = refusal(&body);
    assert!(
        status == 404
            && code.ends_with("ResourceNotFoundException")
            && message == Sentence::NotOnboarded.text(instance),
        "case {:?}: expected the not-onboarded sentence for a configuration that is \
         already gone, got {status} {code:?} {message:?}",
        "second DeleteConnectInstanceConfig"
    );

    // The READ of the same resource states the other sentence for the same request, so
    // the two are genuinely different sentences and not one spelled twice.
    let (status, body) = call(&harness, "GET", &path, &Value::Null).await;
    let (code, message) = refusal(&body);
    assert!(
        status == 404
            && code.ends_with("ResourceNotFoundException")
            && message == Sentence::Config.text(instance),
        "case {:?}: expected the configuration sentence, got {status} {code:?} {message:?}",
        "GetConnectInstanceConfig after the delete"
    );
}

#[test]
fn the_five_instance_sentences_are_five_different_texts() {
    // What makes the mapping above assertable: no two of the sentences read alike for
    // one request, and each is composed from the request exactly where its own class
    // names the resource the caller asked about.
    let instance = "sentence-identity-instance";
    let composed = [
        (Sentence::Config, true),
        (Sentence::Instance, false),
        (Sentence::NotOnboarded, false),
        (Sentence::InstanceId, true),
        (Sentence::Job, true),
    ];
    for (sentence, names_the_instance) in composed {
        let text = sentence.text(instance);
        assert_eq!(
            text.contains(instance),
            names_the_instance,
            "case {sentence:?}: expected naming the instance to be {names_the_instance}, \
             got {text:?}"
        );
        for (other, _) in composed {
            if other != sentence {
                assert_ne!(
                    text,
                    other.text(instance),
                    "case {sentence:?}: it must not read the same as {other:?}, got {text:?}"
                );
            }
        }
    }
}
