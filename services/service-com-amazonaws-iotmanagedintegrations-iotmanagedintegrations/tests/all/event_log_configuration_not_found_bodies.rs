// SPDX-License-Identifier: CC0-1.0

//! What an absent event log configuration's `ResourceNotFoundException` BODY carries, per
//! path, over the wire.
//!
//! RULE: which members of the modelled `ResourceNotFoundException` a 404 fills belongs to
//! the PATH that resolved the resource, not to the resource alone — all three (`Message`,
//! `ResourceId`, `ResourceType`) are optional on the shape
//! (`smithy_query(query="shape",shape="ResourceNotFoundException")` -> members `Message`,
//! `ResourceId`, `ResourceType`, `httpError` 404). For an event log configuration the
//! READING path words one fixed sentence ([`ABSENT_REASON`]) and the UPDATING path words no
//! sentence at all: HTTP 404, the `x-amzn-errortype` code, and the message member on the
//! wire as JSON `null` — which is how a client tells "this path states no message" from
//! "the message is empty" — with neither `ResourceId` nor `ResourceType` on either. Both
//! paths answer the same way whatever made the configuration absent and whatever form the
//! `Id` took, and the empty body is reserved for the absence: an update of a configuration
//! the region does hold is applied and read back.
//!
//! The recorded unit this was derived from
//! (`iot-managed-integrations.event-log-configuration-lifecycle-and-duplicate-conflict`,
//! whose update of a deleted id carries no message where its read of the same id carries the
//! sentence) is ONE row of the tables below. The rows differ along what the rule generalises
//! over: the path that resolves the `Id` (read, update, delete), the cause of the absence
//! (never minted, created and then deleted, held only by another region), the form and
//! length of the `Id` inside its modelled `@length`/`@pattern`, the `EventLogLevel` the
//! update asks for (the model's whole value set), and the region the request is signed for.
//!
//! Every case drives the real restJson1 request path (`MultiServiceRouter::dispatch`), so
//! the statuses, headers and bodies asserted here are the ones a client sees.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::state::LOG_LEVEL_VALUES;
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the suite's requests are signed for unless a case names another.
const REGION: &str = "us-east-1";

/// Another region's endpoint, for the rows that address one region's configuration through
/// another's.
const OTHER_REGION: &str = "eu-central-1";

/// The collection route the event-log-configuration operations are bound to
/// (`smithy_query(query="operation",operation="CreateEventLogConfiguration,UpdateEventLogConfiguration")`
/// -> POST `/event-log-configurations` and PATCH `/event-log-configurations/{Id}`).
const COLLECTION: &str = "/event-log-configurations";

/// The wire error code the model's `@httpError` 404 shape is rendered under.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// Every member the modelled `ResourceNotFoundException` declares, all optional.
const NOT_FOUND_MEMBERS: [&str; 3] = ["Message", "ResourceId", "ResourceType"];

/// The fixed sentence the READING path of this resource words, written out here rather than
/// imported so the suite pins the text a client reads and not the implementation's own
/// constant. It quotes nothing from the request.
const ABSENT_REASON: &str = "Event log configuration doesn't exist";

/// The `@length` bounds of `LogConfigurationId`, in characters
/// (`smithy_query(query="shape",shape="LogConfigurationId")` -> length 1..200, pattern
/// `^[A-Za-z0-9]+$`).
const ID_BOUNDS: (usize, usize) = (1, 200);

/// The modelled success status of `UpdateEventLogConfiguration`
/// (`smithy_query(query="operation",operation="UpdateEventLogConfiguration")` -> 200).
const UPDATE_STATUS: u16 = 200;

/// One HTTP answer, reduced to what the assertions read.
struct Reply {
    status: u16,
    /// The restJson1 error code, from `x-amzn-errortype`.
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// A member the body reports, JSON `null` counting as not reported.
    fn member(&self, name: &str) -> Option<&Value> {
        self.body.get(name).filter(|value| !value.is_null())
    }

    fn string(&self, name: &str) -> Option<&str> {
        self.member(name).and_then(Value::as_str)
    }
}

/// A router holding one `iotmanagedintegrations` service over a fresh shared engine: the
/// same assembly `core/router` performs, with one service mounted.
fn emulator() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    // The signature itself is not under test here, so requests are unauthenticated.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::new(engine::InMemoryEngine::new())));
    router
}

/// Send one request, signed for `region`, and reduce the answer. The `Authorization`
/// credential scope is what routes the request to this service and names its region.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Reply {
    let payload = body.map_or_else(bytes::Bytes::new, |value| {
        bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize the request body"))
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://iotmanagedintegrations.{region}.amazonaws.com{path}"
        ))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTESTTEST/20260101/{region}/\
                 iotmanagedintegrations/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Reply {
        status,
        error_type,
        body,
    }
}

/// `CreateEventLogConfiguration` in `region` for the log target `resource_type`/
/// `resource_id` at `level`, asserting the modelled 201 and answering the minted `Id`.
async fn create_in(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    resource_type: &str,
    resource_id: Option<&str>,
    level: &str,
) -> String {
    let mut body = json!({"ResourceType": resource_type, "EventLogLevel": level});
    if let Some(resource_id) = resource_id {
        body["ResourceId"] = json!(resource_id);
    }
    let reply = call_in(router, region, "POST", COLLECTION, Some(body)).await;
    assert_eq!(
        reply.status, 201,
        "case {case:?}: expected HTTP 201 from CreateEventLogConfiguration, got {} with \
         body {:?}",
        reply.status, reply.body
    );
    reply
        .string("Id")
        .unwrap_or_else(|| panic!("case {case:?}: expected an Id, got body {:?}", reply.body))
        .to_string()
}

/// `GetEventLogConfiguration` for `id`, signed for `region`.
async fn get_in(router: &MultiServiceRouter, region: &str, id: &str) -> Reply {
    call_in(router, region, "GET", &format!("{COLLECTION}/{id}"), None).await
}

/// `UpdateEventLogConfiguration` for `id` asking for `level`, signed for `region`.
async fn update_in(router: &MultiServiceRouter, region: &str, id: &str, level: &str) -> Reply {
    call_in(
        router,
        region,
        "PATCH",
        &format!("{COLLECTION}/{id}"),
        Some(json!({"EventLogLevel": level})),
    )
    .await
}

/// `DeleteEventLogConfiguration` for `id`, signed for `region`.
async fn delete_in(router: &MultiServiceRouter, region: &str, id: &str) -> Reply {
    call_in(router, region, "DELETE", &format!("{COLLECTION}/{id}"), None).await
}

/// Assert `reply` is the modelled 404 of this resource: the status and the wire error code.
fn assert_modelled_not_found(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 404,
        "case {case:?}: expected HTTP 404, got {} with body {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_type.as_deref(),
        Some(NOT_FOUND),
        "case {case:?}: expected the modelled {NOT_FOUND} code, got {:?}",
        reply.error_type
    );
}

/// Assert `reply` states the absence in the WORDS of a path that words one: the fixed
/// sentence on the message member, and neither other member of the exception.
fn assert_states_the_sentence(case: &str, reply: &Reply) {
    assert_modelled_not_found(case, reply);
    assert_eq!(
        reply.string("Message"),
        Some(ABSENT_REASON),
        "case {case:?}: expected the sentence {ABSENT_REASON:?}, got {:?} in body {:?}",
        reply.string("Message"),
        reply.body
    );
    for member in ["ResourceId", "ResourceType"] {
        assert_eq!(
            reply.member(member),
            None,
            "case {case:?}: expected the 404 to carry no {member}, got {:?} in body {:?}",
            reply.member(member),
            reply.body
        );
    }
}

/// Assert `reply` states the absence with NO sentence: not one of the exception's members
/// reported, and the message member present on the wire as JSON `null` — an omitted key
/// reaches an SDK as the empty string instead of as "no message".
fn assert_states_no_sentence(case: &str, reply: &Reply) {
    assert_modelled_not_found(case, reply);
    for member in NOT_FOUND_MEMBERS {
        assert_eq!(
            reply.member(member),
            None,
            "case {case:?}: expected the 404 to carry no {member}, got {:?} in body {:?}",
            reply.member(member),
            reply.body
        );
    }
    let object = reply
        .body
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: expected a JSON object, got {:?}", reply.body));
    assert_eq!(
        object.get("Message"),
        Some(&Value::Null),
        "case {case:?}: expected Message on the wire as JSON null, got {:?} in body {:?}",
        object.get("Message"),
        reply.body
    );
}

/// The `Id`s this region holds no configuration under, and why: several forms the
/// `LogConfigurationId` `@pattern` admits that nothing ever minted, one the service minted
/// and a delete removed, and one the service minted in ANOTHER region.
async fn absent_ids(router: &MultiServiceRouter) -> Vec<(String, String)> {
    let deleted = create_in(
        router,
        "deleted",
        REGION,
        "managed-thing",
        Some("thing1"),
        LOG_LEVEL_VALUES[0],
    )
    .await;
    let removed = delete_in(router, REGION, &deleted).await;
    assert_eq!(
        removed.status, 200,
        "case \"deleted\": expected HTTP 200 from DeleteEventLogConfiguration, got {} with \
         body {:?}",
        removed.status, removed.body
    );

    let elsewhere = create_in(
        router,
        "another region",
        OTHER_REGION,
        "credential-locker",
        Some("locker1"),
        LOG_LEVEL_VALUES[1],
    )
    .await;

    let (shortest, longest) = ID_BOUNDS;
    vec![
        ("never minted, the length minimum".to_string(), "z".repeat(shortest)),
        ("never minted, the length maximum".to_string(), "z".repeat(longest)),
        ("never minted, digits only".to_string(), "0123456789".to_string()),
        ("never minted, mixed case".to_string(), "NoSuchConfiguration1".to_string()),
        ("created and then deleted".to_string(), deleted),
        ("another region's configuration".to_string(), elsewhere),
    ]
}

/// RULE: the update path states an absent configuration with NO sentence and no other member
/// of the exception — for every cause of the absence, every form of `Id`, and every
/// `EventLogLevel` the model declares.
#[tokio::test]
async fn the_update_path_states_an_absent_configuration_with_no_sentence() {
    let router = emulator();

    // One configuration this region DOES hold, so the refusals below are about the `Id`
    // asked for and not about an empty store.
    let seeded = create_in(&router, "seed", REGION, "*", None, LOG_LEVEL_VALUES[0]).await;

    for (why, id) in absent_ids(&router).await {
        // The level asked for cannot change the body: every value the model declares is
        // driven.
        for level in LOG_LEVEL_VALUES {
            let case = format!("{why}/{level}");
            let refused = update_in(&router, REGION, &id, level).await;
            assert_states_no_sentence(&case, &refused);
        }
    }

    // The seeded configuration is untouched by the refusals.
    let read = get_in(&router, REGION, &seeded).await;
    assert_eq!(
        read.status, 200,
        "case \"seed\": expected the seeded configuration to still resolve, got {} with \
         body {:?}",
        read.status, read.body
    );
}

/// RULE, the other half: the paths that WORD a sentence for the same absence keep wording
/// it, so the empty body belongs to the update path and is not a blanket emptying of this
/// resource's 404s.
#[tokio::test]
async fn the_reading_and_deleting_paths_still_word_the_sentence() {
    let router = emulator();

    for (why, id) in absent_ids(&router).await {
        assert_states_the_sentence(
            &format!("GetEventLogConfiguration/{why}"),
            &get_in(&router, REGION, &id).await,
        );
        assert_states_the_sentence(
            &format!("DeleteEventLogConfiguration/{why}"),
            &delete_in(&router, REGION, &id).await,
        );
    }
}

/// RULE: the empty body is reserved for the ABSENCE. An update of a configuration the
/// signing region holds is applied and read back, for every level the model declares, and
/// the region that owns a configuration keeps resolving it after another region's update of
/// the same `Id` was refused.
#[tokio::test]
async fn an_update_of_a_configuration_that_exists_is_still_applied() {
    let router = emulator();

    let id = create_in(
        &router,
        "applied",
        REGION,
        "ota-task",
        None,
        LOG_LEVEL_VALUES[0],
    )
    .await;

    for level in LOG_LEVEL_VALUES {
        let case = format!("applied/{level}");
        let applied = update_in(&router, REGION, &id, level).await;
        assert_eq!(
            applied.status, UPDATE_STATUS,
            "case {case:?}: expected HTTP {UPDATE_STATUS} from UpdateEventLogConfiguration, \
             got {} with body {:?}",
            applied.status, applied.body
        );
        let read = get_in(&router, REGION, &id).await;
        assert_eq!(
            read.string("EventLogLevel"),
            Some(*level),
            "case {case:?}: expected the read to report the level the update applied, got \
             {:?} in body {:?}",
            read.string("EventLogLevel"),
            read.body
        );
    }

    // Addressed through another region's endpoint the same `Id` is absent, stated with no
    // sentence — and the owning region still reports the level it was left at.
    let refused = update_in(&router, OTHER_REGION, &id, LOG_LEVEL_VALUES[0]).await;
    assert_states_no_sentence("another region's endpoint", &refused);
    let read = get_in(&router, REGION, &id).await;
    let last = *LOG_LEVEL_VALUES
        .last()
        .expect("the model declares at least one LogLevel");
    assert_eq!(
        read.string("EventLogLevel"),
        Some(last),
        "case \"another region's endpoint\": expected the owning region to still report \
         {last:?}, got {:?} in body {:?}",
        read.string("EventLogLevel"),
        read.body
    );
}
