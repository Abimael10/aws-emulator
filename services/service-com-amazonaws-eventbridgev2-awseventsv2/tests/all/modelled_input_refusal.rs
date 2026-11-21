// SPDX-License-Identifier: CC0-1.0

//! What this service answers a request it refuses before any handler runs, read where a
//! client reads it: off the `rpcv2Cbor` wire.
//!
//! Three rules, each asserted over a SET of inputs rather than one recorded case.
//!
//! RULE 1 — a request the model's own constraints refuse (a member the input shape
//! requires that the request left out, or a value outside a declared `@length` /
//! `@pattern`) is answered `400` with the error shape this service's model declares for
//! one, `InvalidInputException`, whose `Message` member carries AWS's constraint-violation
//! envelope: `<n> validation error(s) detected: Value '<value>' at '<member>' failed to
//! satisfy constraint: <requirement>`. The violation count, the value as submitted, the
//! member and its modelled requirement are the variable parts; everything else is fixed.
//! No operation of this service answers the framework's synthetic `ValidationException`,
//! which its model does not declare at all.
//!
//! RULE 2 — every create of this service REQUIRES the `@idempotencyToken` `ClientToken`
//! its input models, which the model itself leaves optional because a code-generated
//! client fills it in: a request stating one is served and mints its resource, a repeat
//! carrying the same token replays the one resource that token minted, and a second create
//! of a name already taken under a token of its own is the name collision. What a create
//! stating NO token is answered is `crate::create_requires_an_idempotency_token`'s rule.
//!
//! RULE 3 — `DescribeEventBus` resolves the whole `EventBusArn` it is given, identifier
//! segment included: an ARN carrying an identifier this service never minted addresses no
//! resource of this service and is refused with the fixed text of the
//! `ResourceNotFoundException` class, which interpolates nothing of the request, while an
//! ARN whose identifier the service DID mint names a bus that is not there and is refused
//! as that bus — the wording every operation that resolves a bus by NAME uses.
//!
//! Nothing here waits on the wall clock: each wait is for a CONDITION (a bus reporting
//! `ACTIVE`, a deleted bus ceasing to resolve), polled under a generous deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema_server::CborCodec;
use engine::InMemoryEngine;
use smithy_server_runtime::{DeserializableStruct, MultiServiceRouter, Operation};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::create_event_source::CreateEventSource;
use eventsv2::ops::create_subscriber::CreateSubscriber;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::describe_event_source::DescribeEventSource;
use eventsv2::ops::describe_subscriber::DescribeSubscriber;
use eventsv2::ops::delete_event_bus::DeleteEventBus;
use eventsv2::ops::put_events::PutEvents;
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventBusInput, CreateEventSourceInput,
    CreateSubscriberInput, DeleteEventBusInput, DescribeEventBusInput, DescribeEventSourceInput,
    DescribeSubscriberInput, EventSourceConfiguration, InvokeConfiguration, PutEventsInput,
    PutEventsRequestEntry, StorageConfiguration,
};

/// The region every request below is signed for, which the minted ARNs carry.
pub(crate) const REGION: &str = "us-west-2";

/// The account the emulator mints ARNs under.
const ACCOUNT: &str = "000000000000";

/// The error shape this service's model declares for a request parameter that is missing
/// or not valid, and the `__type` a refusal of one carries.
pub(crate) const INVALID_INPUT_EXCEPTION: &str = "InvalidInputException";

/// The error shape a read of a resource that is not there answers with.
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";

/// The synthetic shape the framework would otherwise refuse a request with, which this
/// service's model does not declare: no answer below may name it.
const UNMODELLED_REFUSAL: &str = "ValidationException";

/// The error shape a create of a name already taken answers with, and the whole
/// `Message` of that class.
const RESOURCE_ALREADY_EXISTS_EXCEPTION: &str = "ResourceAlreadyExistsException";
const RESOURCE_ALREADY_EXISTS: &str = eventsv2::state::RESOURCE_ALREADY_EXISTS_MESSAGE;

/// The whole `Message` of a refusal of a value that addresses no resource of this
/// service.
const RESOURCE_DOES_NOT_EXIST: &str = "Resource does not exist";

/// `EventBusName`'s modelled `@pattern`, as a refusal quotes it: the model declares
/// `^[A-Za-z0-9][\.\-_A-Za-z0-9]*$` and AWS quotes the expression without the anchors a
/// whole-value pattern wraps it in.
/// Source: `smithy_query(query="shape", shape="EventBusName")`.
const EVENT_BUS_NAME_PATTERN: &str = r"[A-Za-z0-9][\.\-_A-Za-z0-9]*";

/// `EventBusName`'s modelled `@length` maximum.
/// Source: `smithy_query(query="shape", shape="EventBusName")`.
const EVENT_BUS_NAME_MAX: usize = 256;

/// `ClientToken`'s modelled `@length` maximum.
/// Source: `smithy_query(query="shape", shape="ClientToken")`.
const CLIENT_TOKEN_MAX: usize = 64;

/// `ClientToken`'s modelled `@pattern`, quoted the same way.
/// Source: `smithy_query(query="shape", shape="ClientToken")`.
const CLIENT_TOKEN_PATTERN: &str = r"[\u0021-\u007E]+";

/// `RetentionPeriodInDays`' modelled `@range`, which a storage configuration a request
/// names must sit inside and the effective retention period of a bus therefore does too.
/// Source: `smithy_query(query="shape", shape="RetentionPeriodInDays")`.
const RETENTION_MIN: i32 = 1;
const RETENTION_MAX: i32 = 365;

/// The retention period a bus created without a storage configuration holds, which every
/// read of it reports.
const DEFAULT_RETENTION: i32 = eventsv2::state::DEFAULT_RETENTION_PERIOD_IN_DAYS;

/// `EventBusArn`'s modelled `@pattern`, quoted the same way.
/// Source: `smithy_query(query="operation", operation="DescribeEventBus")`.
const EVENT_BUS_ARN_PATTERN: &str = r"arn:aws(-[a-z0-9]+)*:events:[a-z][a-z0-9]*(-[a-z0-9]+)*:([0-9]{12}):event-busv2\/[A-Za-z0-9][\.\-_A-Za-z0-9]{0,255}\/[a-z0-9]{25}";

/// Width of the identifier segment of an `EventBusArn`: the modelled pattern pins it at
/// exactly `[a-z0-9]{25}`.
const BUS_ID_LEN: usize = eventsv2::state::BUS_ID_LEN;

/// The role a subscriber's `InvokeConfiguration` names.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/event-delivery";

/// A queue ARN a subscriber targets. Nothing is delivered to it here.
const TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:refusal-target";

/// How long a poll waits for a condition the emulator owes: far above the provisioning
/// span a bus takes, so a loaded host has ample room.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one pass of a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Every operation this service's model binds, by the routing key the `rpcv2Cbor`
/// protocol resolves for it.
/// Source: `smithy_query(query="digest")` — the 25 operations bound to `AWSEventsV2`.
const EVERY_OPERATION: [&str; 25] = [
    "CreateEventBus",
    "CreateEventSource",
    "CreateSubscriber",
    "DeleteEventBus",
    "DeleteEventSource",
    "DeleteResourcePolicy",
    "DeleteSubscriber",
    "DescribeEventBus",
    "DescribeEventSource",
    "DescribeSubscriber",
    "GetResourcePolicy",
    "ListEventBuses",
    "ListEventSources",
    "ListResourcePolicies",
    "ListSubscribers",
    "ListTagsForResource",
    "PutEvents",
    "PutRawEvents",
    "PutResourcePolicy",
    "RevokeResource",
    "TagResource",
    "UntagResource",
    "UpdateEventBus",
    "UpdateEventSource",
    "UpdateSubscriber",
];

/// The empty `rpcv2Cbor` request body: a definite-length CBOR map of no entries, which is
/// a request that names no member at all.
const EMPTY_CBOR_MAP: [u8; 1] = [0xa0];

// ---------------------------------------------------------------------------
// The wire harness
// ---------------------------------------------------------------------------

/// One emulator, reached over HTTP with `rpcv2Cbor` bodies.
pub(crate) struct Wire {
    router: MultiServiceRouter,
}

impl Wire {
    pub(crate) fn new() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(eventsv2::EmulatedService::new(engine));
        Self { router }
    }

    /// Dispatch `body` to `operation` and hand back the status and the response body.
    pub(crate) async fn send(&self, operation: &str, body: Vec<u8>) -> (u16, bytes::Bytes) {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("/service/AWSEventsV2/operation/{operation}"))
            .header("content-type", "application/cbor")
            .header("smithy-protocol", "rpc-v2-cbor")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/events/\
                     aws4_request, SignedHeaders=host;x-amz-date, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(body))
            .expect("the harness request is well formed");
        let response = self.router.dispatch(request).await;
        (response.status().as_u16(), response.into_body())
    }

    /// Dispatch the typed `input` of operation `O`.
    pub(crate) async fn call<O: Operation>(&self, input: &O::Input) -> (u16, bytes::Bytes) {
        self.send(O::NAME, encoded::<O>(input)).await
    }

    /// Dispatch the typed `input` of `O`, requiring a 200 and decoding the output.
    pub(crate) async fn ok<O: Operation>(&self, case: &str, input: &O::Input) -> O::Output
    where
        O::Output: DeserializableStruct,
    {
        let (status, body) = self.call::<O>(input).await;
        assert_eq!(
            status,
            200,
            "case {case:?}: {} must answer 200, got {status} with {:?}",
            O::NAME,
            rendered(&body)
        );
        let codec = CborCodec::default();
        let mut deserializer = codec.create_deserializer(&body);
        O::Output::deserialize(&mut deserializer).unwrap_or_else(|e| {
            panic!(
                "case {case:?}: {}'s body must decode as its modelled output, got {e:?}",
                O::NAME
            )
        })
    }

    /// A bus called `name`, polled until it REPORTS `ACTIVE`.
    pub(crate) async fn active_bus(&self, case: &str, name: &str) -> String {
        let arn = self
            .ok::<CreateEventBus<InMemoryEngine>>(
                case,
                &CreateEventBusInput {
                    name: name.to_string(),
                    client_token: Some(format!("token-for-{name}")),
                    ..Default::default()
                },
            )
            .await
            .event_bus_arn
            .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus must report an EventBusArn"));

        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let reported = self
                .ok::<DescribeEventBus<InMemoryEngine>>(
                    case,
                    &DescribeEventBusInput {
                        event_bus_arn: arn.clone(),
                    },
                )
                .await
                .state;
            if reported.as_deref() == Some("ACTIVE") {
                return arn;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the bus {arn} never reported ACTIVE, last reported {reported:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// `input` encoded as the body of an `O` request.
pub(crate) fn encoded<O: Operation>(input: &O::Input) -> Vec<u8> {
    let codec = CborCodec::default();
    let mut serializer = codec.create_serializer();
    serializer
        .write_struct(O::INPUT_SCHEMA, input as &dyn SerializableStruct)
        .unwrap_or_else(|e| panic!("{}: encoding the input must succeed, got {e:?}", O::NAME));
    serializer.finish()
}

/// A body as text, for a failure message.
pub(crate) fn rendered(body: &[u8]) -> String {
    String::from_utf8_lossy(body).to_string()
}

/// The members of an `rpcv2Cbor` error envelope, decoded as CBOR rather than scanned as
/// text, so what is asserted on is the WHOLE value under the member name the service
/// really wrote.
fn envelope_members(case: &str, body: &[u8]) -> Vec<(String, String)> {
    let mut decoder = aws_smithy_cbor::Decoder::new(body);
    let declared = decoder
        .map()
        .unwrap_or_else(|error| panic!("case {case:?}: the error body is not a CBOR map: {error}"));
    let mut members = Vec::new();
    // A definite-length map states how many entries it holds; an indefinite-length one —
    // which is what this codec writes a modelled error as — ends at a break.
    let mut remaining = declared.unwrap_or(u64::MAX);
    while remaining > 0 {
        if declared.is_none() {
            let next = decoder.datatype().unwrap_or_else(|error| {
                panic!("case {case:?}: the error body ends mid-entry: {error}")
            });
            if next == aws_smithy_cbor::data::Type::Break {
                break;
            }
        }
        let key = decoder
            .str()
            .unwrap_or_else(|error| panic!("case {case:?}: an envelope key is not text: {error}"))
            .into_owned();
        let value = decoder
            .str()
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the value at {key:?} is not text: {error}")
            })
            .into_owned();
        members.push((key, value));
        remaining -= 1;
    }
    members
}

/// The error code and the whole `Message` of a refusal body.
///
/// The member is the one the MODEL declares (`Message`): a body carrying the text under
/// the framework's synthetic `message` instead is the defect these suites pin, so it is
/// reported as such rather than read.
pub(crate) fn refusal(case: &str, body: &[u8]) -> (String, String) {
    let members = envelope_members(case, body);
    let code = members
        .iter()
        .find(|(key, _)| key == "__type")
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| panic!("case {case:?}: the error body names no __type: {members:?}"));
    assert!(
        !members.iter().any(|(key, _)| key == "message"),
        "case {case:?}: the text must travel in the modelled member `Message`, but the body \
         carries the synthetic `message`: {members:?}"
    );
    let message = members
        .iter()
        .find(|(key, _)| key == "Message")
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| panic!("case {case:?}: the error body carries no Message: {members:?}"));
    (code, message)
}

/// One clause of the constraint envelope: the value as submitted, the member it sat at,
/// and the requirement it broke.
fn clause(value: &str, member: &str, requirement: &str) -> String {
    format!("Value '{value}' at '{member}' failed to satisfy constraint: {requirement}")
}

/// The clause of a member absent from the request, which echoes no value.
pub(crate) fn absent_clause(member: &str) -> String {
    format!("Value null at '{member}' failed to satisfy constraint: Member must not be null")
}

/// The requirement clause of a `@pattern`.
fn must_match(pattern: &str) -> String {
    format!("Member must satisfy regular expression pattern: {pattern}")
}

/// The whole `Message` of a constraint refusal: AWS's envelope over `clauses`.
pub(crate) fn constraint_envelope(clauses: &[String]) -> String {
    if clauses.len() == 1 {
        return format!("1 validation error detected: {}", clauses[0]);
    }
    format!(
        "{} validation errors detected: {}",
        clauses.len(),
        clauses.join("; ")
    )
}

/// A well-formed `EventBusArn` naming `name` with the identifier segment `bus_id`, in
/// `region` — an ARN this service never minted.
pub(crate) fn unminted_arn(region: &str, name: &str, bus_id: &str) -> String {
    assert_eq!(
        bus_id.len(),
        BUS_ID_LEN,
        "the modelled EventBusArn pattern pins the identifier segment at {BUS_ID_LEN} characters"
    );
    format!("arn:aws:events:{region}:{ACCOUNT}:event-busv2/{name}/{bus_id}")
}

/// A `CreateEventBusInput` naming `name`, with `client_token` as the caller supplied it:
/// `None` is the request that states no token at all, which every create of this service
/// refuses ([`CLIENT_TOKEN_REQUIRED`]).
pub(crate) fn create_bus(name: &str, client_token: Option<&str>) -> CreateEventBusInput {
    CreateEventBusInput {
        name: name.to_string(),
        client_token: client_token.map(str::to_string),
        ..Default::default()
    }
}

/// The whole `Message` a create that states no `ClientToken` is refused with — spelled out
/// here rather than read from the crate, so a reworded refusal fails the suites that drive
/// it. The error shape it carries is [`INVALID_INPUT_EXCEPTION`].
pub(crate) const CLIENT_TOKEN_REQUIRED: &str = "ClientToken is required";

/// The input member every create of this service requires the caller to state, reached
/// without naming which create's input shape it sits on.
///
/// This exists for [`with_client_token`], which is what the suites' harnesses put a token
/// on a create that states none with.
pub(crate) trait StatesClientToken {
    /// The request's own `ClientToken` member.
    fn client_token_mut(&mut self) -> &mut Option<String>;
}

impl StatesClientToken for CreateEventBusInput {
    fn client_token_mut(&mut self) -> &mut Option<String> {
        &mut self.client_token
    }
}

impl StatesClientToken for CreateEventSourceInput {
    fn client_token_mut(&mut self) -> &mut Option<String> {
        &mut self.client_token
    }
}

impl StatesClientToken for CreateSubscriberInput {
    fn client_token_mut(&mut self) -> &mut Option<String> {
        &mut self.client_token
    }
}

/// `input` with the `ClientToken` this service requires of every create, when the case
/// states none of its own — what a code-generated client does for the member the model
/// tags `@idempotencyToken`.
///
/// The token is a FRESH one per call ([`fresh_client_token`]), so a create a case did not
/// mean as a retry mints its own resource rather than replaying another's: a case about
/// idempotency states the token it means and this leaves it alone, and a case about a name
/// collision still collides.
pub(crate) fn with_client_token<Input: StatesClientToken>(mut input: Input) -> Input {
    if input.client_token_mut().is_none() {
        *input.client_token_mut() = Some(fresh_client_token());
    }
    input
}

/// A `ClientToken` no other call of the process has used, inside the member's modelled
/// `@length` 1..64 and `@pattern` `^[\u0021-\u007E]+$`.
pub(crate) fn fresh_client_token() -> String {
    use ::std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("harness-token-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// A `CreateEventSourceInput` on `event_bus_arn`.
pub(crate) fn create_source(
    name: &str,
    event_bus_arn: &str,
    client_token: Option<&str>,
) -> CreateEventSourceInput {
    CreateEventSourceInput {
        name: name.to_string(),
        event_bus_arn: event_bus_arn.to_string(),
        configuration: EventSourceConfiguration::AwsServiceEventsConfiguration(
            AwsServiceEventsSourceConfiguration {
                aws_service: "aws.s3".to_string(),
                ..Default::default()
            },
        ),
        description: None,
        tags: None,
        client_token: client_token.map(str::to_string),
    }
}

/// A `CreateSubscriberInput` on `event_bus_arn`.
pub(crate) fn create_subscriber(
    name: &str,
    event_bus_arn: &str,
    client_token: Option<&str>,
) -> CreateSubscriberInput {
    CreateSubscriberInput {
        name: name.to_string(),
        event_bus_arn: event_bus_arn.to_string(),
        invoke_configuration: InvokeConfiguration {
            role_arn: ROLE_ARN.to_string(),
            target_arn: TARGET_ARN.to_string(),
            ..Default::default()
        },
        client_token: client_token.map(str::to_string),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// RULE 1: the constraint refusal is the modelled shape, with AWS's envelope
// ---------------------------------------------------------------------------

/// RULE 1 over the classes of refusal the model can produce: a bad `@pattern`, a value
/// past a `@length` bound, a member absent altogether, and two broken members in one
/// request — on three different operations.
#[tokio::test]
async fn a_constraint_refusal_is_the_modelled_shape_with_aws_envelope() {
    let wire = Wire::new();
    let too_long_token = "t".repeat(CLIENT_TOKEN_MAX + 1);
    let too_long_name = "L".repeat(EVENT_BUS_NAME_MAX + 1);
    let name_pattern = must_match(EVENT_BUS_NAME_PATTERN);

    // (case, operation, the request body, the whole Message expected)
    let rows: Vec<(&str, &str, Vec<u8>, String)> = vec![
        (
            "a bus name opening with a dot",
            CreateEventBus::<InMemoryEngine>::NAME,
            encoded::<CreateEventBus<InMemoryEngine>>(&create_bus(".bad-name", Some("a-token"))),
            constraint_envelope(&[clause(".bad-name", "name", &name_pattern)]),
        ),
        (
            "a bus name opening with a hyphen",
            CreateEventBus::<InMemoryEngine>::NAME,
            encoded::<CreateEventBus<InMemoryEngine>>(&create_bus("-leading", Some("a-token"))),
            constraint_envelope(&[clause("-leading", "name", &name_pattern)]),
        ),
        (
            "a bus name carrying a space",
            CreateEventBus::<InMemoryEngine>::NAME,
            encoded::<CreateEventBus<InMemoryEngine>>(&create_bus("has space", Some("a-token"))),
            constraint_envelope(&[clause("has space", "name", &name_pattern)]),
        ),
        (
            "an idempotency token past its length maximum",
            CreateEventBus::<InMemoryEngine>::NAME,
            encoded::<CreateEventBus<InMemoryEngine>>(&create_bus(
                "length-case-bus",
                Some(&too_long_token),
            )),
            constraint_envelope(&[clause(
                &too_long_token,
                "clientToken",
                &format!("Member must have length less than or equal to {CLIENT_TOKEN_MAX}"),
            )]),
        ),
        (
            // A value past the member's maximum is longer than the elision ceiling a
            // message renderer applies, so this row is what pins the clause quoting the
            // value the caller SENT rather than a prefix of it.
            "a bus name past its length maximum, quoted in full",
            CreateEventBus::<InMemoryEngine>::NAME,
            encoded::<CreateEventBus<InMemoryEngine>>(&create_bus(
                &too_long_name,
                Some("a-token"),
            )),
            constraint_envelope(&[clause(
                &too_long_name,
                "name",
                &format!("Member must have length less than or equal to {EVENT_BUS_NAME_MAX}"),
            )]),
        ),
        (
            "an empty bus name, which breaks its length minimum and its pattern",
            CreateEventBus::<InMemoryEngine>::NAME,
            encoded::<CreateEventBus<InMemoryEngine>>(&create_bus("", Some("a-token"))),
            constraint_envelope(&[
                clause(
                    "",
                    "name",
                    "Member must have length greater than or equal to 1",
                ),
                clause("", "name", &name_pattern),
            ]),
        ),
        (
            "a bus name of the longest length the model admits, which is accepted",
            CreateEventBus::<InMemoryEngine>::NAME,
            encoded::<CreateEventBus<InMemoryEngine>>(&create_bus(
                &format!("L{}", "o".repeat(EVENT_BUS_NAME_MAX - 1)),
                Some("a-token"),
            )),
            // Not a refusal at all: the row is here so the bound is tested at its
            // published edge and not only past it.
            String::new(),
        ),
        (
            "an idempotency token outside its own pattern",
            CreateEventBus::<InMemoryEngine>::NAME,
            encoded::<CreateEventBus<InMemoryEngine>>(&create_bus("token-case-bus", Some(" "))),
            constraint_envelope(&[clause(" ", "clientToken", &must_match(CLIENT_TOKEN_PATTERN))]),
        ),
        (
            "a bus name and a token both outside their patterns",
            CreateEventBus::<InMemoryEngine>::NAME,
            encoded::<CreateEventBus<InMemoryEngine>>(&create_bus(".bad", Some(" "))),
            constraint_envelope(&[
                clause(".bad", "name", &name_pattern),
                clause(" ", "clientToken", &must_match(CLIENT_TOKEN_PATTERN)),
            ]),
        ),
        (
            "a required member absent from a create",
            CreateEventBus::<InMemoryEngine>::NAME,
            EMPTY_CBOR_MAP.to_vec(),
            constraint_envelope(&[absent_clause("name")]),
        ),
        (
            "a read addressed at something that is not an ARN",
            DescribeEventBus::<InMemoryEngine>::NAME,
            encoded::<DescribeEventBus<InMemoryEngine>>(&DescribeEventBusInput {
                event_bus_arn: "not-an-arn".to_string(),
            }),
            constraint_envelope(&[clause(
                "not-an-arn",
                "eventBusArn",
                &must_match(EVENT_BUS_ARN_PATTERN),
            )]),
        ),
        (
            "a required member absent from a read",
            DescribeEventBus::<InMemoryEngine>::NAME,
            EMPTY_CBOR_MAP.to_vec(),
            constraint_envelope(&[absent_clause("eventBusArn")]),
        ),
        (
            "a publish addressed at something that is not an ARN",
            PutEvents::<InMemoryEngine>::NAME,
            encoded::<PutEvents<InMemoryEngine>>(&PutEventsInput {
                event_bus_arn: "arn:aws:events:::nonsense".to_string(),
                entries: vec![PutEventsRequestEntry {
                    source: "demo.app".to_string(),
                    detail_type: "Demo".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            constraint_envelope(&[clause(
                "arn:aws:events:::nonsense",
                "eventBusArn",
                &must_match(EVENT_BUS_ARN_PATTERN),
            )]),
        ),
    ];

    for (case, operation, body, expected) in rows {
        let (status, body) = wire.send(operation, body).await;
        // The one row inside every bound is served rather than refused, which is the
        // other half of the same rule: nothing but a broken requirement is refused.
        if expected.is_empty() {
            assert_eq!(
                status, 200,
                "case {case:?}: {operation} must serve a request inside every bound, got \
                 {status} with {:?}",
                rendered(&body)
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: {operation} must answer 400, got {status} with {:?}",
            rendered(&body)
        );
        let (code, message) = refusal(case, &body);
        assert_eq!(
            code, INVALID_INPUT_EXCEPTION,
            "case {case:?}: {operation} must answer {INVALID_INPUT_EXCEPTION}, got {code:?} \
             with {message:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: {operation} must state {expected:?}, got {message:?}"
        );
    }
}

/// RULE 1 as a census: EVERY operation the model binds is served through the same
/// front door, so none of them answers the synthetic shape this service does not model.
///
/// The request is the empty body — no member at all — which every operation that requires
/// one refuses and every operation that requires none accepts; either way the answer must
/// not be the unmodelled refusal.
#[tokio::test]
async fn every_modelled_operation_states_its_refusal_in_the_modelled_shape() {
    let wire = Wire::new();
    for operation in EVERY_OPERATION {
        let (status, body) = wire.send(operation, EMPTY_CBOR_MAP.to_vec()).await;
        let text = rendered(&body);
        assert!(
            !text.contains(UNMODELLED_REFUSAL),
            "case {operation:?}: no answer may name {UNMODELLED_REFUSAL}, got {status} with \
             {text:?}"
        );
        if status != 400 {
            continue;
        }
        let (code, message) = refusal(operation, &body);
        assert_eq!(
            code, INVALID_INPUT_EXCEPTION,
            "case {operation:?}: a refused request must answer {INVALID_INPUT_EXCEPTION}, got \
             {code:?} with {message:?}"
        );
        assert!(
            message.contains("validation error"),
            "case {operation:?}: the refusal must carry the constraint envelope, got {message:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 2: a create is served when it states the idempotency token its input models
// ---------------------------------------------------------------------------

/// RULE 2 on the create whose whole output the model states: a request stating the
/// `ClientToken` this service requires is served and mints the bus — over rows varying the
/// name grammar and the storage configuration, so neither the answer nor the mint is keyed
/// to one request shape — and the bus it reports is the bus the store holds. The same row
/// with no token at all is refused and mints nothing, which
/// `crate::create_requires_an_idempotency_token` drives across every create.
#[tokio::test]
async fn a_bus_create_is_served_when_it_states_the_idempotency_token() {
    let wire = Wire::new();

    // (case, the bus name, the RetentionPeriodInDays the request names)
    // The retention rows are the published bounds of `RetentionPeriodInDays`
    // (`@range` 1..365) plus a request naming no storage configuration at all.
    // Source: `smithy_query(query="shape", shape="StorageConfiguration")`.
    let rows = [
        ("naming only the required member", "tokened-bus", None),
        ("the shortest name the grammar admits", "A", None),
        (
            "every punctuation character the name admits",
            "A.b-c_9",
            None,
        ),
        ("the retention floor", "tokened-floor", Some(RETENTION_MIN)),
        (
            "the retention ceiling",
            "tokened-ceiling",
            Some(RETENTION_MAX),
        ),
    ];

    let mut minted: Vec<String> = Vec::new();
    for (case, name, retention) in rows {
        let create = with_client_token(CreateEventBusInput {
            name: name.to_string(),
            storage_configuration: retention.map(|days| StorageConfiguration {
                retention_period_in_days: Some(days),
            }),
            client_token: None,
            ..Default::default()
        });
        // The token the harness states is the one member this service requires beyond the
        // model's own, so this is a well-formed create: it is served, and what it reports
        // is the bus it minted.
        let created = wire
            .ok::<CreateEventBus<InMemoryEngine>>(case, &create)
            .await;

        assert_eq!(
            created.name.as_deref(),
            Some(name),
            "case {case:?}: expected the requested name {name:?}, got {:?}",
            created.name
        );
        let arn = created.event_bus_arn.clone().unwrap_or_else(|| {
            panic!("case {case:?}: expected a minted EventBusArn, got {created:?}")
        });
        let prefix = format!("arn:aws:events:{REGION}:{ACCOUNT}:event-busv2/{name}/");
        assert!(
            arn.starts_with(&prefix) && arn.len() == prefix.len() + BUS_ID_LEN,
            "case {case:?}: expected an ARN {prefix}<{BUS_ID_LEN} characters>, got {arn:?}"
        );
        assert_eq!(
            created.state.as_deref(),
            Some("CREATING"),
            "case {case:?}: expected the bus reported CREATING, got {:?}",
            created.state
        );
        assert!(
            created.creation_time.is_some(),
            "case {case:?}: expected a CreationTime, got {created:?}"
        );
        let storage = created.storage_configuration.clone().unwrap_or_else(|| {
            panic!("case {case:?}: expected a StorageConfiguration, got {created:?}")
        });
        // The effective retention period: what the request named, or the service default
        // when it named none.
        let effective = retention.unwrap_or(DEFAULT_RETENTION);
        assert_eq!(
            storage.retention_period_in_days,
            Some(effective),
            "case {case:?}: expected RetentionPeriodInDays {effective}, got {:?}",
            storage.retention_period_in_days
        );
        assert!(
            (RETENTION_MIN..=RETENTION_MAX).contains(&effective),
            "case {case:?}: the effective retention period must sit inside the modelled \
             range {RETENTION_MIN}..={RETENTION_MAX}, got {effective}"
        );
        assert!(
            storage.retention_window_start_time.is_some(),
            "case {case:?}: expected a RetentionWindowStartTime, got {storage:?}"
        );

        // The bus the create reported is the bus the store holds: read it back under the
        // ARN it minted.
        let read = wire
            .ok::<DescribeEventBus<InMemoryEngine>>(
                case,
                &DescribeEventBusInput {
                    event_bus_arn: arn.clone(),
                },
            )
            .await;
        assert_eq!(
            read.name.as_deref(),
            Some(name),
            "case {case:?}: the stored bus must carry the requested name, got {:?}",
            read.name
        );
        assert_eq!(
            read.storage_configuration
                .as_ref()
                .and_then(|storage| storage.retention_period_in_days),
            Some(effective),
            "case {case:?}: the stored bus must carry the effective retention period, got \
             {:?}",
            read.storage_configuration
        );
        minted.push(arn);
    }

    // Each row named a bus of its own and carried a token of its own, so every one of
    // them minted a bus of its own — no create replayed another's.
    let mut distinct = minted.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        minted.len(),
        "each create of a name and a token of its own must mint its own bus, got {minted:?}"
    );
}

/// RULE 2 on the two remaining creates, which model the same token: each is served while
/// the request states one, with the resource it minted read back through its own Describe
/// — and a create naming a bus that is not there is refused for that bus, which is the
/// resource it NAMES rather than anything about the token.
#[tokio::test]
async fn an_event_source_and_a_subscriber_are_served_when_they_state_the_token() {
    let wire = Wire::new();
    let bus = wire.active_bus("the host bus", "token-rule-bus").await;

    let case = "CreateEventSource on a bus that does not exist";
    let (status, body) = wire
        .call::<CreateEventSource<InMemoryEngine>>(&with_client_token(create_source(
            "stated-token-source",
            &unminted_arn(REGION, "no-such-bus", "zzzzzzzzzzzzzzzzzzzzzzzzz"),
            None,
        )))
        .await;
    assert_eq!(
        status, 404,
        "case {case:?}: a create naming a bus that does not exist must be refused for that \
         bus, got {status} with {:?}",
        rendered(&body)
    );
    let (code, message) = refusal(case, &body);
    assert_eq!(
        code, RESOURCE_NOT_FOUND_EXCEPTION,
        "case {case:?}: expected {RESOURCE_NOT_FOUND_EXCEPTION}, got {code:?} with {message:?}"
    );
    assert!(
        message.contains("no-such-bus"),
        "case {case:?}: the refusal must name the bus the request addressed, got {message:?}"
    );

    let case = "CreateEventSource stating a ClientToken";
    let source = wire
        .ok::<CreateEventSource<InMemoryEngine>>(
            case,
            &with_client_token(create_source("stated-token-source", &bus, None)),
        )
        .await;
    let source_arn = source.event_source_arn.clone().unwrap_or_else(|| {
        panic!("case {case:?}: expected a minted EventSourceArn, got {source:?}")
    });
    let read = wire
        .ok::<DescribeEventSource<InMemoryEngine>>(
            case,
            &DescribeEventSourceInput {
                event_source_arn: source_arn.clone(),
            },
        )
        .await;
    assert_eq!(
        read.event_bus_arn.as_deref(),
        Some(bus.as_str()),
        "case {case:?}: the stored source must sit on the bus the request named, got {:?}",
        read.event_bus_arn
    );
    assert_eq!(
        read.name.as_deref(),
        Some("stated-token-source"),
        "case {case:?}: the stored source must carry the requested name, got {:?}",
        read.name
    );

    let case = "CreateSubscriber stating a ClientToken";
    let subscriber = wire
        .ok::<CreateSubscriber<InMemoryEngine>>(
            case,
            &with_client_token(create_subscriber("stated-token-subscriber", &bus, None)),
        )
        .await;
    let subscriber_arn = subscriber.subscriber_arn.clone().unwrap_or_else(|| {
        panic!("case {case:?}: expected a minted SubscriberArn, got {subscriber:?}")
    });
    let read = wire
        .ok::<DescribeSubscriber<InMemoryEngine>>(
            case,
            &DescribeSubscriberInput {
                subscriber_arn: subscriber_arn.clone(),
            },
        )
        .await;
    assert_eq!(
        read.event_bus_arn.as_deref(),
        Some(bus.as_str()),
        "case {case:?}: the stored subscriber must sit on the bus the request named, got {:?}",
        read.event_bus_arn
    );
    assert_eq!(
        read.name.as_deref(),
        Some("stated-token-subscriber"),
        "case {case:?}: the stored subscriber must carry the requested name, got {:?}",
        read.name
    );
}

/// What the token BUYS, beyond being required: a repeat carrying one and the same token
/// is the one resource that token minted, while a create carrying a token of its own mints
/// a resource of its own — and a second create of a name already taken, under a token of
/// its own, is the name collision.
#[tokio::test]
async fn a_repeated_token_replays_the_bus_it_minted_and_a_fresh_one_mints_its_own() {
    let wire = Wire::new();

    let case = "a repeated create carrying a token of its own";
    let first = wire
        .ok::<CreateEventBus<InMemoryEngine>>(
            case,
            &create_bus("repeated-named", Some("the-first-token")),
        )
        .await;
    let (status, body) = wire
        .call::<CreateEventBus<InMemoryEngine>>(&create_bus(
            "repeated-named",
            Some("a-second-token"),
        ))
        .await;
    assert_eq!(
        status, 409,
        "case {case:?}: a second create of the same name must collide, got {status} with {:?}",
        rendered(&body)
    );
    let (code, message) = refusal(case, &body);
    assert_eq!(
        code, RESOURCE_ALREADY_EXISTS_EXCEPTION,
        "case {case:?}: expected {RESOURCE_ALREADY_EXISTS_EXCEPTION}, got {code:?} with \
         {message:?}"
    );
    assert_eq!(
        message, RESOURCE_ALREADY_EXISTS,
        "case {case:?}: expected {RESOURCE_ALREADY_EXISTS:?}, got {message:?}"
    );

    let case = "a repeated create carrying one token";
    let replayed = wire
        .ok::<CreateEventBus<InMemoryEngine>>(
            case,
            &create_bus("repeated-tokened", Some("one-and-the-same-token")),
        )
        .await;
    let again = wire
        .ok::<CreateEventBus<InMemoryEngine>>(
            case,
            &create_bus("repeated-tokened", Some("one-and-the-same-token")),
        )
        .await;
    assert_eq!(
        again.event_bus_arn, replayed.event_bus_arn,
        "case {case:?}: the retry must replay the bus the token minted, got {:?} after {:?}",
        again.event_bus_arn, replayed.event_bus_arn
    );
    assert_ne!(
        first.event_bus_arn, replayed.event_bus_arn,
        "case {case:?}: the tokened create must mint a bus of its own, got {:?}",
        replayed.event_bus_arn
    );
}

// ---------------------------------------------------------------------------
// RULE 3: what a bus read answers for an ARN it cannot resolve
// ---------------------------------------------------------------------------

/// RULE 3: an `EventBusArn` carrying an identifier this service never minted addresses no
/// resource of this service, so the bus read refuses it with the fixed text of the class,
/// naming nothing of the request — over rows varying the bus name, the identifier segment
/// and the region.
#[tokio::test]
async fn a_bus_read_of_an_arn_this_service_never_minted_names_no_resource() {
    let wire = Wire::new();
    // Rows spanning the modelled `EventBusName` grammar and the region segment, each with
    // its OWN identifier, so neither can be part of the message.
    let rows = [
        ("the shortest name", REGION, "A", "aaaaaaaaaaaaaaaaaaaaaaaaa"),
        (
            "every punctuation character the name admits",
            REGION,
            "A.b-c_9",
            "bbbbbbbbbbbbbbbbbbbbbbbbb",
        ),
        (
            "a plain name",
            REGION,
            "ghost-bus",
            "ccccccccccccccccccccccccc",
        ),
        (
            "another region",
            "eu-central-1",
            "ghost-bus",
            "ddddddddddddddddddddddddd",
        ),
        (
            "the same name under another identifier",
            REGION,
            "ghost-bus",
            "eeeeeeeeeeeeeeeeeeeeeeeee",
        ),
    ];

    for (case, region, name, bus_id) in rows {
        let arn = unminted_arn(region, name, bus_id);
        let (status, body) = wire
            .call::<DescribeEventBus<InMemoryEngine>>(&DescribeEventBusInput {
                event_bus_arn: arn.clone(),
            })
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: DescribeEventBus must answer 404, got {status} with {:?}",
            rendered(&body)
        );
        let (code, message) = refusal(case, &body);
        assert_eq!(
            code, RESOURCE_NOT_FOUND_EXCEPTION,
            "case {case:?}: expected {RESOURCE_NOT_FOUND_EXCEPTION}, got {code:?} with \
             {message:?}"
        );
        assert_eq!(
            message, RESOURCE_DOES_NOT_EXIST,
            "case {case:?}: expected {RESOURCE_DOES_NOT_EXIST:?}, got {message:?}"
        );
        // The same requirement said the other way round: nothing of the request is
        // interpolated, so no part of the ARN the refusal could have reached for is in it.
        for leaked in [arn.as_str(), name, bus_id, region] {
            assert!(
                !message.contains(leaked),
                "case {case:?}: the message must interpolate nothing, but {message:?} carries \
                 {leaked:?}"
            );
        }
    }
}

/// RULE 3, the other half: an ARN whose identifier this service DID mint names a bus, so
/// a read of it after the bus is gone is refused as that bus — while an operation that
/// resolves a bus by NAME words it that way whatever the identifier.
#[tokio::test]
async fn a_bus_read_of_a_minted_arn_is_refused_as_the_bus_that_is_not_there() {
    let wire = Wire::new();
    let name = "retired-bus";
    let arn = wire.active_bus("a minted bus", name).await;

    // While it is there, the read reports it rather than refusing.
    let described = wire
        .ok::<DescribeEventBus<InMemoryEngine>>(
            "a minted bus",
            &DescribeEventBusInput {
                event_bus_arn: arn.clone(),
            },
        )
        .await;
    assert_eq!(
        described.event_bus_arn.as_deref(),
        Some(arn.as_str()),
        "case \"a minted bus\": the read must report the bus asked for, got {:?}",
        described.event_bus_arn
    );

    let (status, body) = wire
        .call::<DeleteEventBus<InMemoryEngine>>(&DeleteEventBusInput {
            event_bus_arn: arn.clone(),
        })
        .await;
    assert_eq!(
        status, 200,
        "case \"a retired bus\": DeleteEventBus must answer 200, got {status} with {:?}",
        rendered(&body)
    );

    // Deletion is asynchronous: poll for the CONDITION — the ARN no longer resolving.
    let deadline = Instant::now() + POLL_DEADLINE;
    let message = loop {
        let (status, body) = wire
            .call::<DescribeEventBus<InMemoryEngine>>(&DescribeEventBusInput {
                event_bus_arn: arn.clone(),
            })
            .await;
        if status == 404 {
            let (code, message) = refusal("a retired bus", &body);
            assert_eq!(
                code, RESOURCE_NOT_FOUND_EXCEPTION,
                "case \"a retired bus\": expected {RESOURCE_NOT_FOUND_EXCEPTION}, got {code:?} \
                 with {message:?}"
            );
            break message;
        }
        assert!(
            Instant::now() < deadline,
            "case \"a retired bus\": the deleted bus never stopped resolving, last answer \
             {status} with {:?}",
            rendered(&body)
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(
        message,
        format!("Event bus not found: {name}"),
        "case \"a retired bus\": a read of an ARN this service minted must name the bus, got \
         {message:?}"
    );

    // An operation that resolves a bus by NAME names it whatever the identifier, so an
    // ARN nothing minted is reported the same way there.
    let unminted = unminted_arn(REGION, "by-name-bus", "fffffffffffffffffffffffff");
    let (status, body) = wire
        .call::<PutEvents<InMemoryEngine>>(&PutEventsInput {
            event_bus_arn: unminted,
            entries: vec![PutEventsRequestEntry {
                source: "demo.app".to_string(),
                detail_type: "Demo".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        })
        .await;
    assert_eq!(
        status, 404,
        "case \"by name\": PutEvents must answer 404, got {status} with {:?}",
        rendered(&body)
    );
    let (code, message) = refusal("by name", &body);
    assert_eq!(
        code, RESOURCE_NOT_FOUND_EXCEPTION,
        "case \"by name\": expected {RESOURCE_NOT_FOUND_EXCEPTION}, got {code:?} with {message:?}"
    );
    assert_eq!(
        message, "Event bus not found: by-name-bus",
        "case \"by name\": the publish must name the bus it could not find, got {message:?}"
    );
}
