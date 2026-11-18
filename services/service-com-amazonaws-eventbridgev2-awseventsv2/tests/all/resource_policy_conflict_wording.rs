// SPDX-License-Identifier: CC0-1.0

//! The wording of the refusal a resource-policy write gets when its
//! `ExpectedRevisionId` precondition does not hold, which a client reads
//! character by character.
//!
//! RULE: the `Message` of the `ConflictException` a revision-gated
//! resource-policy write answers is the fixed text of that error class — it
//! names neither the `ExpectedRevisionId` the request carried, the policy it
//! addressed, nor the bus it addressed it on — for every precondition that does
//! not hold and whatever the store holds, and the policy that was already
//! attached is left exactly as it was.
//!
//! The rows span that class rather than one recorded write: preconditions
//! differing in form (a revision-shaped token, the sentinel spelled as a
//! revision, the sentinel on a policy that now exists, a superseded revision
//! this store really minted), in length (one character, and the modelled
//! maximum of 256), in whether the addressed policy exists at all, and in
//! whether the request names the policy or lets it default — plus the same
//! refusal as it reaches a client on the wire.
//!
//! Nothing here waits on the wall clock.

use std::sync::Arc;

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema_server::CborCodec;
use engine::InMemoryEngine;
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError,
};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::get_resource_policy::GetResourcePolicy;
use eventsv2::ops::put_resource_policy::{PutResourcePolicy, PutResourcePolicyErrorModeled};
use eventsv2::ops::types::{
    CreateEventBusInput, GetResourcePolicyInput, PutResourcePolicyInput,
};
use eventsv2::scaffolded::Scaffolded;

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The region every request below is signed for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// The policy an operation that names no `PolicyName` addresses, per
/// `PolicyName`'s documented value list.
const DEFAULT_POLICY: &str = "default";

/// The `ExpectedRevisionId` sentinel a policy that does not exist yet matches,
/// per `PutResourcePolicy`'s `ExpectedRevisionId`.
const NO_POLICY: &str = "NO_POLICY";

/// The modelled maximum length of `PolicyRevisionId` (`@length` 1..256), which a
/// row walks so the wording is pinned at the longest precondition the member
/// admits as well as at the shortest.
const REVISION_ID_MAX_LEN: usize = 256;

/// The whole `Message` of the `ConflictException` a failed revision precondition
/// answers: the fixed text of that error class, written out here rather than
/// read from the service, so this suite pins the wording instead of restating
/// whatever the service carries.
const REVISION_CONFLICT_MESSAGE: &str = "The resource policy revision does not match \
     ExpectedRevisionId, or the event bus was recreated since that revision was read. Re-read \
     the policy and re-evaluate before retrying. A recent policy change can take up to a minute \
     to be reflected on all endpoints, so a read taken immediately after one may still return \
     the earlier revision.";

/// A SigV4 `Authorization` header naming the region and this service's signing
/// name, which is how a request names both.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/events/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// The request metadata of a call signed for [`REGION`].
fn parts() -> http::request::Parts {
    let (parts, ()) = http::Request::builder()
        .method("POST")
        .uri("/service/AWSEventsV2/operation/PutResourcePolicy")
        .header("authorization", authorization())
        .header("content-type", "application/cbor")
        .body(())
        .expect("the harness request is well formed")
        .into_parts();
    parts
}

/// One emulator over one shared engine, with a request context every typed call
/// reuses.
struct Harness {
    engine: Arc<InMemoryEngine>,
    definition: eventsv2::ServiceDefinition<InMemoryEngine>,
    callback: EmbeddedEmulator,
    parts: http::request::Parts,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let definition = eventsv2::ServiceDefinition::new(Arc::clone(&engine));
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        let callback = EmbeddedEmulator::from_router(router);
        Self {
            engine,
            definition,
            callback,
            parts: parts(),
        }
    }

    fn ctx(&self) -> OperationContext<'_> {
        OperationContext::new(&self.parts, self.definition.identity(), self.callback.clone())
    }

    /// A bus called `name`, as its create minted it. A policy write needs no
    /// particular lifecycle state of the bus, so nothing waits here.
    async fn bus(&self, case: &str, name: &str) -> String {
        CreateEventBus::new(Arc::clone(&self.engine))
            .handle(
                CreateEventBusInput {
                    name: name.to_string(),
                    client_token: Some(format!("{name}-token")),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: CreateEventBus must succeed, got {e:?}"))
            .event_bus_arn
            .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus reported no EventBusArn"))
    }

    /// The whole refusal of one conditional write: the wire error code, the HTTP
    /// status and the `Message` the `ConflictException` carries.
    async fn conflict(&self, case: &str, input: PutResourcePolicyInput) -> (String, u16, String) {
        let error = PutResourcePolicy::new(Arc::clone(&self.engine))
            .handle(input, &self.ctx())
            .await
            .err()
            .unwrap_or_else(|| panic!("case {case:?}: the conditional write must be refused"));
        let shape = error.shape_name().to_string();
        let status = error.http_status();
        let message = match error {
            Scaffolded::Modeled(PutResourcePolicyErrorModeled::ConflictException(conflict)) => {
                conflict.message.clone().unwrap_or_default()
            }
            other => panic!("case {case:?}: expected ConflictException, got {other:?}"),
        };
        (shape, status, message)
    }

    /// The document and revision `GetResourcePolicy` reports for the
    /// customer-managed policy of `bus`, or `None` when it holds none.
    async fn stored(&self, bus: &str) -> Option<(String, String)> {
        GetResourcePolicy::new(Arc::clone(&self.engine))
            .handle(
                GetResourcePolicyInput {
                    resource_arn: bus.to_string(),
                    policy_name: None,
                },
                &self.ctx(),
            )
            .await
            .ok()
            .map(|read| (read.policy_document, read.revision_id))
    }
}

/// A policy document allowing one specific account to publish to `bus`, the
/// smallest well-formed policy a row needs.
fn account_policy(bus: &str, account: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"AllowOneAccount","Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{account}:root"}},"Action":"events:PutEvents","Resource":"{bus}"}}]}}"#
    )
}

/// One refused write: the row it belongs to, the wire error code, the status and
/// the whole `Message`.
type Refusal = (String, String, u16, String);

/// Require of every collected refusal that it is the modelled conflict on 409
/// carrying exactly the fixed text, and that nothing of the request or of the
/// store leaked into it.
fn assert_fixed_wording(refusals: &[Refusal], leakable: &[&str]) {
    for (case, shape, status, message) in refusals {
        assert_eq!(
            shape, "ConflictException",
            "case {case:?}: expected ConflictException, got {shape:?} with {message:?}"
        );
        assert_eq!(
            *status, 409,
            "case {case:?}: expected HTTP 409, got {status} with {message:?}"
        );
        assert_eq!(
            message, REVISION_CONFLICT_MESSAGE,
            "case {case:?}: expected the fixed message {REVISION_CONFLICT_MESSAGE:?}, got \
             {message:?}"
        );
        // The same requirement said the other way round: no value the refusal
        // could have reached for — the precondition, the policy name, the bus,
        // the revision the store holds — appears in it.
        for leaked in leakable {
            assert!(
                !message.contains(leaked),
                "case {case:?}: the message must interpolate nothing, but {message:?} carries \
                 {leaked:?}"
            );
        }
    }
}

/// RULE: every conditional write whose `ExpectedRevisionId` does not hold is
/// refused as `ConflictException` (409) whose whole `Message` is the fixed text
/// of that class, with nothing of the request or of the store in it — on a
/// policy that does not exist yet and on one that does.
#[tokio::test]
async fn a_failed_revision_precondition_is_refused_with_the_fixed_wording_of_its_class() {
    let harness = Harness::new();
    let bus = harness.bus("the wording", "policy-conflict-wording-bus").await;
    let first = account_policy(&bus, ACCOUNT);
    let second = account_policy(&bus, "111111111111");
    let mut refusals: Vec<Refusal> = Vec::new();

    // --- a policy that does not exist yet: only the sentinel matches ---------
    for (case, expected, policy_name) in [
        (
            "absent policy, a revision-shaped token",
            "ec1cf2d8-0000-4000-8000-000000000000".to_string(),
            None,
        ),
        (
            "absent policy, the sentinel in lower case",
            NO_POLICY.to_ascii_lowercase(),
            None,
        ),
        (
            "absent policy, a one-character precondition",
            "x".to_string(),
            None,
        ),
        (
            "absent policy, the longest precondition the member admits",
            "z".repeat(REVISION_ID_MAX_LEN),
            Some(DEFAULT_POLICY.to_string()),
        ),
        (
            "absent policy, a precondition quoting the policy name",
            format!("{DEFAULT_POLICY}-revision"),
            Some(DEFAULT_POLICY.to_string()),
        ),
    ] {
        refusals.push({
            let (shape, status, message) = harness
                .conflict(
                    case,
                    PutResourcePolicyInput {
                        resource_arn: bus.clone(),
                        policy_document: first.clone(),
                        policy_name,
                        expected_revision_id: Some(expected),
                    },
                )
                .await;
            (case.to_string(), shape, status, message)
        });
        assert!(
            harness.stored(&bus).await.is_none(),
            "case {case:?}: a refused create-only write must attach nothing"
        );
    }

    // --- a policy that now exists: only its current revision matches ---------
    let created = PutResourcePolicy::new(Arc::clone(&harness.engine))
        .handle(
            PutResourcePolicyInput {
                resource_arn: bus.clone(),
                policy_document: first.clone(),
                expected_revision_id: Some(NO_POLICY.to_string()),
                policy_name: None,
            },
            &harness.ctx(),
        )
        .await
        .expect("case \"the create-only write\": the write must succeed")
        .revision_id
        .expect("case \"the create-only write\": the write reported no RevisionId");

    // A revision this store really minted and then superseded, so one row's
    // precondition is a token the service itself handed out.
    let replaced = PutResourcePolicy::new(Arc::clone(&harness.engine))
        .handle(
            PutResourcePolicyInput {
                resource_arn: bus.clone(),
                policy_document: second.clone(),
                expected_revision_id: Some(created.clone()),
                policy_name: None,
            },
            &harness.ctx(),
        )
        .await
        .expect("case \"the conditional overwrite\": the write must succeed")
        .revision_id
        .expect("case \"the conditional overwrite\": the write reported no RevisionId");

    for (case, expected, policy_name) in [
        (
            "present policy, the sentinel",
            NO_POLICY.to_string(),
            None,
        ),
        (
            "present policy, a revision this store never minted",
            "not-a-revision-this-store-minted".to_string(),
            None,
        ),
        (
            "present policy, the superseded revision",
            created.clone(),
            None,
        ),
        (
            "present policy, the policy named rather than defaulted",
            created.clone(),
            Some(DEFAULT_POLICY.to_string()),
        ),
    ] {
        refusals.push({
            let (shape, status, message) = harness
                .conflict(
                    case,
                    PutResourcePolicyInput {
                        resource_arn: bus.clone(),
                        policy_document: first.clone(),
                        policy_name,
                        expected_revision_id: Some(expected),
                    },
                )
                .await;
            (case.to_string(), shape, status, message)
        });
        // The refused write advanced nothing and replaced nothing.
        let stored = harness.stored(&bus).await;
        assert_eq!(
            stored,
            Some((second.clone(), replaced.clone())),
            "case {case:?}: a refused write must leave the stored policy untouched, got \
             {stored:?}"
        );
    }

    assert_eq!(
        refusals.len(),
        9,
        "every row must have produced a refusal, got {refusals:?}"
    );
    assert_fixed_wording(
        &refusals,
        &[
            DEFAULT_POLICY,
            NO_POLICY,
            "policy-conflict-wording-bus",
            "arn:aws:",
            &created,
            &replaced,
            "not-a-revision-this-store-minted",
            "ec1cf2d8-0000-4000-8000-000000000000",
        ],
    );
}

/// RULE again, where a client reads it: on the wire the 409 body carries the
/// fixed text and nothing of the request.
#[tokio::test]
async fn the_conflict_wording_is_what_the_wire_body_carries() {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(eventsv2::EmulatedService::new(Arc::clone(&engine)));

    let name = "wire-policy-conflict-bus";
    let (status, body) = cbor_call::<CreateEventBus<InMemoryEngine>>(
        &router,
        &CreateEventBusInput {
            name: name.to_string(),
            client_token: Some("wire-conflict-token".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"wire, the bus\": CreateEventBus must answer 200, got {status}"
    );
    assert!(
        !body.is_empty(),
        "case \"wire, the bus\": the create must answer a body"
    );

    // The ARN the create minted, read out of the store the create wrote it to,
    // so the writes below address the bus itself rather than a guessed ARN.
    let store = eventsv2::state::service_state(&*engine)
        .expect("the in-memory engine always has this service's store");
    let arn = {
        let state = store.lock();
        state
            .event_buses
            .keys()
            .find(|arn| arn.contains(name))
            .cloned()
            .expect("case \"wire, the bus\": the created bus must be stored")
    };
    let document = account_policy(&arn, ACCOUNT);

    let (status, body) = cbor_call::<PutResourcePolicy<InMemoryEngine>>(
        &router,
        &PutResourcePolicyInput {
            resource_arn: arn.clone(),
            policy_document: document.clone(),
            policy_name: None,
            expected_revision_id: None,
        },
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"wire, the unconditional write\": the write must answer 200, got {status} with \
         body {:?}",
        String::from_utf8_lossy(&body)
    );

    let stale = "a-precondition-this-store-never-minted";
    let (status, body) = cbor_call::<PutResourcePolicy<InMemoryEngine>>(
        &router,
        &PutResourcePolicyInput {
            resource_arn: arn.clone(),
            policy_document: document.clone(),
            policy_name: Some(DEFAULT_POLICY.to_string()),
            expected_revision_id: Some(stale.to_string()),
        },
    )
    .await;
    let rendered = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status, 409,
        "case \"wire, the conflict\": the conditional write must answer 409, got {status} with \
         body {rendered:?}"
    );
    assert!(
        rendered.contains(REVISION_CONFLICT_MESSAGE),
        "case \"wire, the conflict\": the body must carry the fixed message \
         {REVISION_CONFLICT_MESSAGE:?}, got {rendered:?}"
    );
    assert!(
        rendered.contains("ConflictException"),
        "case \"wire, the conflict\": the body must name the error shape, got {rendered:?}"
    );
    for leaked in [stale, name, "arn:aws:", DEFAULT_POLICY] {
        assert!(
            !rendered.contains(leaked),
            "case \"wire, the conflict\": the body must interpolate nothing, but {rendered:?} \
             carries {leaked:?}"
        );
    }
}

/// Encode `input` as this service's wire protocol, dispatch it through the
/// router, and hand back the HTTP status and the raw response body.
async fn cbor_call<O: Operation>(
    router: &MultiServiceRouter,
    input: &O::Input,
) -> (u16, bytes::Bytes) {
    let codec = CborCodec::default();
    let mut serializer = codec.create_serializer();
    serializer
        .write_struct(O::INPUT_SCHEMA, input as &dyn SerializableStruct)
        .unwrap_or_else(|e| panic!("{}: encoding the input must succeed, got {e:?}", O::NAME));
    let body = serializer.finish();

    let request = http::Request::builder()
        .method("POST")
        .uri(format!("/service/AWSEventsV2/operation/{}", O::NAME))
        .header("authorization", authorization())
        .header("content-type", "application/cbor")
        .header("smithy-protocol", "rpc-v2-cbor")
        .body(bytes::Bytes::from(body))
        .unwrap_or_else(|e| panic!("{}: the request must be well formed, got {e:?}", O::NAME));

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, response.into_body())
}
