// SPDX-License-Identifier: CC0-1.0

//! The named resource policies attached to an event bus: what
//! `PutResourcePolicy` stores and what `GetResourcePolicy` and
//! `ListResourcePolicies` report back.
//!
//! RULE: `PutResourcePolicy` attaches the document a request carries to the
//! named policy of the event bus its `ResourceArn` addresses, under a freshly
//! minted revision id, and every later read of that bus reports exactly what was
//! stored — the document byte for byte, under the name it was written as, with
//! the revision the write answered. A write conditioned on `ExpectedRevisionId`
//! is applied only when that value is the policy's current revision (or the
//! `NO_POLICY` sentinel for a policy that does not exist yet) and is otherwise
//! refused with `ConflictException`, leaving what is stored untouched. A policy
//! name outside the two the service admits is refused with
//! `InvalidInputException`; the Resource Access Manager policy is refused to the
//! bus owner with `AccessDeniedException` on a write while both names stay
//! readable; a "default" document past the policy size quota is refused with
//! `PolicyLengthExceededException`; one that would grant public access with
//! `PublicPolicyException`; and an ARN that addresses no stored bus with
//! `ResourceNotFoundException`. Nothing is attached by a refused write.
//!
//! The rows span that class rather than one recorded write: documents differing
//! in the shape of their `Statement` member, their statement count, their
//! effect, their conditions, their character set and their size; preconditions
//! differing in which revision they name; and every policy name the model
//! admits and several it does not.
//!
//! Nothing here waits on the wall clock.

use std::sync::Arc;

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema_server::{CborCodec, DeserializableStruct};
use engine::InMemoryEngine;
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError,
};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::get_resource_policy::{GetResourcePolicy, GetResourcePolicyError};
use eventsv2::ops::list_resource_policies::{ListResourcePolicies, ListResourcePoliciesError};
use eventsv2::ops::put_resource_policy::{PutResourcePolicy, PutResourcePolicyError};
use eventsv2::ops::types::{
    CreateEventBusInput, GetResourcePolicyInput, GetResourcePolicyOutput,
    ListResourcePoliciesInput, ListResourcePoliciesOutput, PutResourcePolicyInput,
    PutResourcePolicyOutput,
};

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The region every request below is signed for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// The policy an operation that names no `PolicyName` addresses, per
/// `PolicyName`'s documented value list.
const DEFAULT_POLICY: &str = "default";

/// The policy AWS Resource Access Manager owns, the other entry of that list.
const AWS_RAM_POLICY: &str = "AWS_RAM";

/// The `ExpectedRevisionId` sentinel a policy that does not exist yet matches,
/// per `PutResourcePolicy`'s `ExpectedRevisionId`.
const NO_POLICY: &str = "NO_POLICY";

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

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

    /// The service's own store, for the one write only AWS Resource Access
    /// Manager makes and for the assertions that are about what is STORED.
    fn store(&self) -> engine::ServiceState<eventsv2::ComAmazonawsEventbridgev2Awseventsv2State> {
        eventsv2::state::service_state(&*self.engine)
            .expect("the in-memory engine always has this service's store")
    }

    /// A bus called `name`, as its create minted it. A policy write needs no
    /// particular lifecycle state of the bus — `PutResourcePolicy` models no
    /// state error — so nothing waits here.
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

    async fn put(
        &self,
        input: PutResourcePolicyInput,
    ) -> Result<PutResourcePolicyOutput, PutResourcePolicyError> {
        PutResourcePolicy::new(Arc::clone(&self.engine))
            .handle(input, &self.ctx())
            .await
    }

    /// An unconditional write of `document` to the customer-managed policy.
    async fn put_default(
        &self,
        bus: &str,
        document: &str,
    ) -> Result<PutResourcePolicyOutput, PutResourcePolicyError> {
        self.put(PutResourcePolicyInput {
            resource_arn: bus.to_string(),
            policy_document: document.to_string(),
            ..Default::default()
        })
        .await
    }

    async fn get(
        &self,
        bus: &str,
        policy_name: Option<&str>,
    ) -> Result<GetResourcePolicyOutput, GetResourcePolicyError> {
        GetResourcePolicy::new(Arc::clone(&self.engine))
            .handle(
                GetResourcePolicyInput {
                    resource_arn: bus.to_string(),
                    policy_name: policy_name.map(str::to_string),
                },
                &self.ctx(),
            )
            .await
    }

    async fn list(
        &self,
        bus: &str,
        max_results: i32,
        next_token: Option<String>,
    ) -> Result<ListResourcePoliciesOutput, ListResourcePoliciesError> {
        ListResourcePolicies::new(Arc::clone(&self.engine))
            .handle(
                ListResourcePoliciesInput {
                    resource_arn: bus.to_string(),
                    max_results,
                    next_token,
                },
                &self.ctx(),
            )
            .await
    }

    /// The policy name and revision of every policy the bus reports, in the
    /// order it reports them.
    async fn summaries(&self, case: &str, bus: &str) -> Vec<(String, String)> {
        self.list(bus, 0, None)
            .await
            .unwrap_or_else(|e| {
                panic!("case {case:?}: ListResourcePolicies must succeed, got {e:?}")
            })
            .policy_summaries
            .unwrap_or_else(|| panic!("case {case:?}: ListResourcePolicies reported no summaries"))
            .into_iter()
            .map(|summary| (summary.policy_name, summary.revision_id))
            .collect()
    }

    /// Attach a policy the way AWS Resource Access Manager does, which is the
    /// only writer of the "AWS_RAM" policy and so the only way that policy comes
    /// to exist. The revision id is minted by the service's own id mint, not by
    /// this test.
    fn store_ram_policy(&self, case: &str, bus: &str, document: &str) -> String {
        let store = self.store();
        let mut state = store.lock();
        let revision =
            eventsv2::state::new_policy_revision_id(state.next_id_seed(), std::time::SystemTime::now());
        state
            .event_buses
            .get_mut(bus)
            .unwrap_or_else(|| panic!("case {case:?}: the bus {bus} must be stored"))
            .store_resource_policy(AWS_RAM_POLICY, document.to_string(), revision.clone());
        revision
    }
}

/// Require that `error` is the modelled refusal `code` on `status`.
fn assert_refusal<E: OperationError + std::fmt::Debug>(
    case: &str,
    error: &E,
    code: &str,
    status: u16,
) {
    assert_eq!(
        (error.shape_name(), error.http_status()),
        (code, status),
        "case {case:?}: expected {code} ({status}), got {error:?} at {}",
        error.http_status()
    );
}

// ---------------------------------------------------------------------------
// The documents the rows span
// ---------------------------------------------------------------------------

/// A policy document allowing one specific account to publish to `bus`, the
/// smallest well-formed policy a row needs.
fn account_policy(bus: &str, account: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"AllowOneAccount","Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{account}:root"}},"Action":"events:PutEvents","Resource":"{bus}"}}]}}"#
    )
}

/// A document of exactly `bytes` bytes of UTF-8: the policy above with its `Sid`
/// padded out, so a row about the SIZE quota differs from its neighbour only in
/// size.
fn document_of_size(bus: &str, bytes: usize) -> String {
    let unpadded = account_policy(bus, ACCOUNT);
    let padding = bytes.saturating_sub(unpadded.len());
    let document = unpadded.replace("AllowOneAccount", &format!("A{}", "a".repeat(padding + 14)));
    assert_eq!(
        document.len(),
        bytes,
        "the padded document must be exactly {bytes} bytes, got {}",
        document.len()
    );
    document
}

/// The documents one bus is written in turn, spanning what the round-trip rule
/// generalises over: the shape of `Statement` (a list or a single object), the
/// number of statements, the effect, whether a condition is present, the
/// character set, and the size.
fn round_trip_documents(bus: &str) -> Vec<(&'static str, String)> {
    vec![
        ("one statement in a list", account_policy(bus, ACCOUNT)),
        (
            "a single statement as an object rather than a list",
            format!(
                r#"{{"Version":"2012-10-17","Statement":{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}},"Action":"events:PutEvents","Resource":"{bus}"}}}}"#
            ),
        ),
        (
            "two statements, the second an explicit Deny",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"Allow","Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}},"Action":"events:PutEvents","Resource":"{bus}"}},{{"Sid":"Deny","Effect":"Deny","Principal":"*","Action":"events:PutEvents","Resource":"{bus}","Condition":{{"StringNotEquals":{{"aws:PrincipalAccount":"{ACCOUNT}"}}}}}}]}}"#
            ),
        ),
        (
            "a wildcard principal narrowed by a caller condition",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":"*","Action":"events:PutEvents","Resource":"{bus}","Condition":{{"StringEquals":{{"aws:PrincipalOrgID":"o-abcd1234"}}}}}}]}}"#
            ),
        ),
        (
            "a document carrying non-ASCII text",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"Zürich-Tokyo-東京","Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}},"Action":"events:PutEvents","Resource":"{bus}"}}]}}"#
            ),
        ),
        (
            "a document just under the policy size quota",
            document_of_size(bus, 20_000),
        ),
    ]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// RULE, the round trip: every write stores the document it carried under a NEW
/// revision, and the reads report exactly that — the same document, the same
/// revision, under the name it was written as.
#[tokio::test]
async fn a_written_policy_is_what_every_later_read_reports() {
    let harness = Harness::new();
    let bus = harness.bus("the round trip", "policy-round-trip-bus").await;
    let mut revisions: Vec<String> = Vec::new();

    for (case, document) in round_trip_documents(&bus) {
        let written = harness
            .put_default(&bus, &document)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: PutResourcePolicy must succeed, got {e:?}"));

        assert_eq!(
            (written.resource_arn.as_str(), written.policy_name.as_str()),
            (bus.as_str(), DEFAULT_POLICY),
            "case {case:?}: the write must answer the bus and policy it wrote, got {written:?}"
        );
        let revision = written
            .revision_id
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: the write reported no RevisionId"));
        assert!(
            !revisions.contains(&revision),
            "case {case:?}: every write mints a new revision, but {revision:?} was seen before \
             in {revisions:?}"
        );
        revisions.push(revision.clone());

        // The read reports what was stored, byte for byte.
        let read = harness
            .get(&bus, None)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: GetResourcePolicy must succeed, got {e:?}"));
        assert_eq!(
            (
                read.resource_arn.as_str(),
                read.policy_name.as_str(),
                read.policy_document.as_str(),
                read.revision_id.as_str()
            ),
            (
                bus.as_str(),
                DEFAULT_POLICY,
                document.as_str(),
                revision.as_str()
            ),
            "case {case:?}: the read must report the policy just written, got {read:?}"
        );

        // A read changes nothing a later caller sees.
        let again = harness
            .get(&bus, None)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: the second read must succeed, got {e:?}"));
        assert_eq!(
            (again.policy_document.as_str(), again.revision_id.as_str()),
            (document.as_str(), revision.as_str()),
            "case {case:?}: a read must not change what the next read sees, got {again:?}"
        );

        // And the enumeration reports that one policy, with the same revision.
        let summaries = harness.summaries(case, &bus).await;
        assert_eq!(
            summaries,
            vec![(DEFAULT_POLICY.to_string(), revision.clone())],
            "case {case:?}: the bus must report exactly the policy written, got {summaries:?}"
        );
    }

    // A policy is attached to ONE bus: a second bus reports none of it.
    let other = harness.bus("a second bus", "policy-other-bus").await;
    let summaries = harness.summaries("a second bus", &other).await;
    assert!(
        summaries.is_empty(),
        "case \"a second bus\": a bus nobody wrote a policy to must report none, got \
         {summaries:?}"
    );
    let error = harness
        .get(&other, None)
        .await
        .expect_err("case \"a second bus\": the read of an unattached policy must be refused");
    assert_refusal(
        "a second bus",
        &error,
        "ResourceNotFoundException",
        404,
    );
}

/// RULE, the precondition: a conditional write is applied only when
/// `ExpectedRevisionId` names what the store holds — the current revision, or
/// the `NO_POLICY` sentinel for a policy that does not exist yet — and every
/// other value is refused with `ConflictException` and attaches nothing.
#[tokio::test]
async fn a_conditional_write_is_applied_only_on_the_revision_it_names() {
    let harness = Harness::new();
    let bus = harness.bus("the precondition", "policy-revision-bus").await;
    let first = account_policy(&bus, ACCOUNT);
    let second = account_policy(&bus, "111111111111");

    // A policy that does not exist yet matches only the sentinel: every other
    // expectation is a conflict, and nothing is attached.
    for (case, expected) in [
        ("a revision naming nothing, on an absent policy", "ec1cf2d8-0000-4000-8000-000000000000"),
        ("the sentinel spelled as a revision, on an absent policy", "no_policy"),
    ] {
        let error = harness
            .put(PutResourcePolicyInput {
                resource_arn: bus.clone(),
                policy_document: first.clone(),
                expected_revision_id: Some(expected.to_string()),
                ..Default::default()
            })
            .await
            .expect_err(&format!("case {case:?}: the write must be refused"));
        assert_refusal(case, &error, "ConflictException", 409);
        assert!(
            harness.get(&bus, None).await.is_err(),
            "case {case:?}: a refused create-only write must attach nothing"
        );
    }

    // The sentinel is what a create-only write names.
    let created = harness
        .put(PutResourcePolicyInput {
            resource_arn: bus.clone(),
            policy_document: first.clone(),
            expected_revision_id: Some(NO_POLICY.to_string()),
            ..Default::default()
        })
        .await
        .expect("case \"the sentinel on an absent policy\": the write must succeed")
        .revision_id
        .expect("case \"the sentinel on an absent policy\": the write reported no RevisionId");

    // Once the policy exists, the sentinel no longer matches, and neither does
    // any revision but the current one.
    for (case, expected) in [
        ("the sentinel, on a policy that now exists", NO_POLICY.to_string()),
        ("a revision this store never minted", "not-a-revision-this-store-minted".to_string()),
    ] {
        let error = harness
            .put(PutResourcePolicyInput {
                resource_arn: bus.clone(),
                policy_document: second.clone(),
                expected_revision_id: Some(expected.clone()),
                ..Default::default()
            })
            .await
            .expect_err(&format!("case {case:?}: the write must be refused"));
        assert_refusal(case, &error, "ConflictException", 409);
        let read = harness
            .get(&bus, None)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: the read must succeed, got {e:?}"));
        assert_eq!(
            (read.policy_document.as_str(), read.revision_id.as_str()),
            (first.as_str(), created.as_str()),
            "case {case:?}: a refused write must leave the stored policy untouched, got {read:?}"
        );
    }

    // The current revision is what a conditional overwrite names, and the write
    // replaces the document under a revision of its own.
    let replaced = harness
        .put(PutResourcePolicyInput {
            resource_arn: bus.clone(),
            policy_document: second.clone(),
            expected_revision_id: Some(created.clone()),
            ..Default::default()
        })
        .await
        .expect("case \"the current revision\": the write must succeed")
        .revision_id
        .expect("case \"the current revision\": the write reported no RevisionId");
    assert_ne!(
        replaced, created,
        "case \"the current revision\": a successful write mints a new revision"
    );
    let read = harness
        .get(&bus, None)
        .await
        .expect("case \"the current revision\": the read must succeed");
    assert_eq!(
        (read.policy_document.as_str(), read.revision_id.as_str()),
        (second.as_str(), replaced.as_str()),
        "case \"the current revision\": the read must report the replacement, got {read:?}"
    );

    // The revision the caller just lost is now stale: retrying with it conflicts.
    let error = harness
        .put(PutResourcePolicyInput {
            resource_arn: bus.clone(),
            policy_document: first.clone(),
            expected_revision_id: Some(created.clone()),
            ..Default::default()
        })
        .await
        .expect_err("case \"the superseded revision\": the write must be refused");
    assert_refusal("the superseded revision", &error, "ConflictException", 409);

    // A write carrying no expectation is unconditional, whatever is stored.
    let unconditional = harness
        .put_default(&bus, &first)
        .await
        .expect("case \"no expectation\": the write must succeed")
        .revision_id
        .expect("case \"no expectation\": the write reported no RevisionId");
    assert_ne!(
        unconditional, replaced,
        "case \"no expectation\": a successful write mints a new revision"
    );
}

/// RULE, the names: `PolicyName` admits exactly "default" and "AWS_RAM"; any
/// other well-formed name is refused with `InvalidInputException`, on a write
/// and on a read alike.
///
/// The names come from `PolicyName`'s own documented value list ("Valid values:
/// "default" … and "AWS_RAM" …. Any other value is rejected"), so the rows below
/// include entries differing from those two only in case.
#[tokio::test]
async fn a_policy_name_outside_the_documented_pair_is_refused() {
    let harness = Harness::new();
    let bus = harness.bus("the names", "policy-name-bus").await;
    let document = account_policy(&bus, ACCOUNT);

    for case in ["custom", "Default", "DEFAULT", "aws_ram", "AWS_RAM_2", "policy1"] {
        let error = harness
            .put(PutResourcePolicyInput {
                resource_arn: bus.clone(),
                policy_document: document.clone(),
                policy_name: Some(case.to_string()),
                ..Default::default()
            })
            .await
            .expect_err(&format!("case {case:?}: the write must be refused"));
        assert_refusal(case, &error, "InvalidInputException", 400);

        let error = harness
            .get(&bus, Some(case))
            .await
            .expect_err(&format!("case {case:?}: the read must be refused"));
        assert_refusal(case, &error, "InvalidInputException", 400);
    }

    // The two writers are exclusive: the bus owner may not write the Resource
    // Access Manager policy, and that refusal attaches nothing.
    let error = harness
        .put(PutResourcePolicyInput {
            resource_arn: bus.clone(),
            policy_document: document.clone(),
            policy_name: Some(AWS_RAM_POLICY.to_string()),
            ..Default::default()
        })
        .await
        .expect_err("case \"the Resource Access Manager policy\": the write must be refused");
    assert_refusal(
        "the Resource Access Manager policy",
        &error,
        "AccessDeniedException",
        403,
    );
    let error = harness
        .get(&bus, Some(AWS_RAM_POLICY))
        .await
        .expect_err("case \"the Resource Access Manager policy\": the read must be refused");
    assert_refusal(
        "the Resource Access Manager policy",
        &error,
        "ResourceNotFoundException",
        404,
    );

    // Writing "default" explicitly addresses the same policy as omitting the
    // member, which is what makes the default documented.
    let named = harness
        .put(PutResourcePolicyInput {
            resource_arn: bus.clone(),
            policy_document: document.clone(),
            policy_name: Some(DEFAULT_POLICY.to_string()),
            ..Default::default()
        })
        .await
        .expect("case \"the customer-managed policy by name\": the write must succeed");
    let read = harness
        .get(&bus, None)
        .await
        .expect("case \"the customer-managed policy by name\": the read must succeed");
    assert_eq!(
        read.revision_id.as_str(),
        named.revision_id.unwrap_or_default().as_str(),
        "case \"the customer-managed policy by name\": naming \"default\" and omitting the \
         member must address one policy, got {read:?}"
    );
}

/// RULE, the reads of both names: neither name is reserved on a read, so the bus
/// owner reads the Resource Access Manager policy too, and the enumeration
/// reports every attached policy in name order, one page at a time.
#[tokio::test]
async fn both_named_policies_are_readable_and_enumerated_in_name_order() {
    let harness = Harness::new();
    let bus = harness.bus("both policies", "policy-pair-bus").await;
    let owner_document = account_policy(&bus, ACCOUNT);
    let ram_document = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"RamShare","Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::222222222222:root"}},"Action":"events:PutEvents","Resource":"{bus}"}}]}}"#
    );

    let owner_revision = harness
        .put_default(&bus, &owner_document)
        .await
        .expect("case \"both policies\": the owner's write must succeed")
        .revision_id
        .expect("case \"both policies\": the write reported no RevisionId");
    let ram_revision = harness.store_ram_policy("both policies", &bus, &ram_document);

    for (case, policy_name, document, revision) in [
        (
            "the customer-managed policy",
            DEFAULT_POLICY,
            &owner_document,
            &owner_revision,
        ),
        (
            "the Resource Access Manager policy",
            AWS_RAM_POLICY,
            &ram_document,
            &ram_revision,
        ),
    ] {
        let read = harness
            .get(&bus, Some(policy_name))
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: the read must succeed, got {e:?}"));
        assert_eq!(
            (
                read.policy_name.as_str(),
                read.policy_document.as_str(),
                read.revision_id.as_str()
            ),
            (policy_name, document.as_str(), revision.as_str()),
            "case {case:?}: the read must report the stored policy, got {read:?}"
        );
    }

    // Both are enumerated, ordered by policy name.
    let summaries = harness.summaries("both policies", &bus).await;
    assert_eq!(
        summaries,
        vec![
            (AWS_RAM_POLICY.to_string(), ram_revision.clone()),
            (DEFAULT_POLICY.to_string(), owner_revision.clone()),
        ],
        "case \"both policies\": both policies must be reported in name order, got {summaries:?}"
    );

    // And a page smaller than the enumeration carries a token that resumes it
    // exactly where it stopped.
    let first_page = harness
        .list(&bus, 1, None)
        .await
        .expect("case \"one policy per page\": the first page must succeed");
    let first_names: Vec<String> = first_page
        .policy_summaries
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|summary| summary.policy_name)
        .collect();
    assert_eq!(
        first_names,
        vec![AWS_RAM_POLICY.to_string()],
        "case \"one policy per page\": the first page must carry the first policy, got \
         {first_page:?}"
    );
    let token = first_page
        .next_token
        .clone()
        .expect("case \"one policy per page\": a page with policies left must carry a token");
    let second_page = harness
        .list(&bus, 1, Some(token))
        .await
        .expect("case \"one policy per page\": the second page must succeed");
    let second_names: Vec<String> = second_page
        .policy_summaries
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|summary| summary.policy_name)
        .collect();
    assert_eq!(
        second_names,
        vec![DEFAULT_POLICY.to_string()],
        "case \"one policy per page\": the second page must resume after the first, got \
         {second_page:?}"
    );
    assert_eq!(
        second_page.next_token, None,
        "case \"one policy per page\": the last page must carry no token, got {:?}",
        second_page.next_token
    );
}

/// RULE, the public-access check: a "default" document whose statement allows a
/// wildcard principal with no condition identifying the caller is refused with
/// `PublicPolicyException` and is not attached; one whose wildcard is narrowed by
/// such a condition, or that names its principals, is attached.
///
/// The reading is the workspace's one reading of a public resource policy
/// (`service_common::public_resource_policy`), which is what
/// `PublicPolicyException`'s own words describe: a condition must limit "the
/// callers to specific AWS accounts", and "conditions on event content
/// (events:source, events:detail-type, events:Metadata/*) do not identify the
/// caller". The rows below span the forms a grant takes rather than one
/// document.
#[tokio::test]
async fn a_default_policy_granting_public_access_is_refused_and_not_attached() {
    let harness = Harness::new();
    let bus = harness.bus("the public check", "policy-public-bus").await;

    // An attached policy to compare against, so a refusal is visible as "nothing
    // changed" rather than as "nothing is there".
    let attached = account_policy(&bus, ACCOUNT);
    let attached_revision = harness
        .put_default(&bus, &attached)
        .await
        .expect("case \"the public check\": the first write must succeed")
        .revision_id
        .expect("case \"the public check\": the write reported no RevisionId");

    let statement = |body: &str| {
        format!(r#"{{"Version":"2012-10-17","Statement":[{{"Action":"events:PutEvents","Resource":"{bus}",{body}}}]}}"#)
    };

    let public: Vec<(&str, String)> = vec![
        (
            "a bare wildcard principal",
            statement(r#""Effect":"Allow","Principal":"*""#),
        ),
        (
            "a wildcard under a principal type",
            statement(r#""Effect":"Allow","Principal":{"AWS":"*"}"#),
        ),
        (
            "a wildcard among named principals",
            statement(
                r#""Effect":"Allow","Principal":{"AWS":["arn:aws:iam::000000000000:root","*"]}"#,
            ),
        ),
        (
            "a wildcard narrowed only by a condition on event content",
            statement(
                r#""Effect":"Allow","Principal":"*","Condition":{"StringEquals":{"events:source":"com.example.app"}}"#,
            ),
        ),
        (
            "a wildcard narrowed only by a condition on the event metadata",
            statement(
                r#""Effect":"Allow","Principal":"*","Condition":{"StringLike":{"events:Metadata/Tenant":"acme"}}"#,
            ),
        ),
        (
            "a public statement beside a narrow one",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}},"Action":"events:PutEvents","Resource":"{bus}"}},{{"Effect":"Allow","Principal":"*","Action":"events:PutEvents","Resource":"{bus}"}}]}}"#
            ),
        ),
        (
            "everyone outside a named set",
            statement(
                r#""Effect":"Allow","NotPrincipal":{"AWS":"arn:aws:iam::000000000000:root"}"#,
            ),
        ),
        (
            "a wildcard under a NEGATED caller comparison, which binds nobody",
            statement(
                r#""Effect":"Allow","Principal":"*","Condition":{"StringNotEquals":{"aws:PrincipalAccount":"000000000000"}}"#,
            ),
        ),
        (
            "a caller condition accepted with a wildcard value",
            statement(
                r#""Effect":"Allow","Principal":"*","Condition":{"ArnLikeIfExists":{"aws:SourceArn":"arn:aws:events:us-west-2:000000000000:rule/*"}}"#,
            ),
        ),
    ];

    for (case, document) in public {
        let error = harness
            .put_default(&bus, &document)
            .await
            .expect_err(&format!("case {case:?}: the write must be refused"));
        assert_refusal(case, &error, "PublicPolicyException", 400);
        let read = harness
            .get(&bus, None)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: the read must succeed, got {e:?}"));
        assert_eq!(
            (read.policy_document.as_str(), read.revision_id.as_str()),
            (attached.as_str(), attached_revision.as_str()),
            "case {case:?}: a refused policy must not be attached, got {read:?}"
        );
    }

    let accepted: Vec<(&str, String)> = vec![
        (
            "a wildcard narrowed to one account",
            statement(
                r#""Effect":"Allow","Principal":"*","Condition":{"StringEquals":{"aws:PrincipalAccount":"000000000000"}}"#,
            ),
        ),
        (
            "a wildcard narrowed to one organization",
            statement(
                r#""Effect":"Allow","Principal":"*","Condition":{"StringEquals":{"aws:PrincipalOrgID":"o-abcd1234"}}"#,
            ),
        ),
        (
            "a wildcard narrowed to a principal tag",
            statement(
                r#""Effect":"Allow","Principal":"*","Condition":{"StringEquals":{"aws:PrincipalTag/team":"platform"}}"#,
            ),
        ),
        (
            "a wildcard narrowed to one source ARN, under an IfExists operator",
            statement(
                r#""Effect":"Allow","Principal":"*","Condition":{"ArnEqualsIfExists":{"aws:SourceArn":"arn:aws:events:us-west-2:000000000000:rule/one"}}"#,
            ),
        ),
        (
            "a wildcard narrowed to the callers that carry a principal ARN",
            statement(
                r#""Effect":"Allow","Principal":"*","Condition":{"Null":{"aws:PrincipalArn":"false"}}"#,
            ),
        ),
        (
            "a wildcard narrowed to several accounts under a set qualifier",
            statement(
                r#""Effect":"Allow","Principal":"*","Condition":{"ForAnyValue:StringEquals":{"aws:PrincipalAccount":["000000000000","111111111111"]}}"#,
            ),
        ),
        (
            "a wildcard principal that is DENIED rather than allowed",
            statement(r#""Effect":"Deny","Principal":"*""#),
        ),
        (
            "named principals only",
            statement(
                r#""Effect":"Allow","Principal":{"AWS":"arn:aws:iam::111111111111:root"}"#,
            ),
        ),
        (
            "a service principal",
            statement(r#""Effect":"Allow","Principal":{"Service":"events.amazonaws.com"}"#),
        ),
    ];

    for (case, document) in accepted {
        let written = harness
            .put_default(&bus, &document)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: the write must succeed, got {e:?}"));
        let read = harness
            .get(&bus, None)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: the read must succeed, got {e:?}"));
        assert_eq!(
            (read.policy_document.as_str(), Some(read.revision_id.clone())),
            (document.as_str(), written.revision_id.clone()),
            "case {case:?}: the write must attach the document, got {read:?}"
        );
    }
}

/// RULE, the size quota: a "default" document is attached up to the quota of
/// 20,480 bytes of UTF-8 and refused with `PolicyLengthExceededException` past
/// it, which leaves what is attached untouched.
///
/// The quota is `PolicyDocument`'s own: "The "default" policy can be up to 20 KB
/// (20,480 bytes of UTF-8) by default."
#[tokio::test]
async fn a_default_policy_past_the_size_quota_is_refused() {
    let harness = Harness::new();
    let bus = harness.bus("the quota", "policy-quota-bus").await;

    for (case, bytes) in [("one byte under the quota", 20_479), ("exactly the quota", 20_480)] {
        let document = document_of_size(&bus, bytes);
        let written = harness
            .put_default(&bus, &document)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: the write must succeed, got {e:?}"));
        let read = harness
            .get(&bus, None)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: the read must succeed, got {e:?}"));
        assert_eq!(
            (read.policy_document.len(), Some(read.revision_id.clone())),
            (bytes, written.revision_id.clone()),
            "case {case:?}: a document within the quota must be attached whole"
        );
    }

    let within = document_of_size(&bus, 20_480);
    for (case, bytes) in [
        ("one byte past the quota", 20_481),
        ("well past the quota", 40_960),
    ] {
        let document = document_of_size(&bus, bytes);
        let error = harness
            .put_default(&bus, &document)
            .await
            .expect_err(&format!("case {case:?}: the write must be refused"));
        assert_refusal(case, &error, "PolicyLengthExceededException", 400);
        let read = harness
            .get(&bus, None)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: the read must succeed, got {e:?}"));
        assert_eq!(
            read.policy_document, within,
            "case {case:?}: a refused document must not be attached"
        );
    }
}

/// RULE, the grammar: a `PolicyDocument` that is not a resource-based policy
/// document is refused with `InvalidInputException` and attaches nothing.
///
/// The clauses come from the workspace's one reading of that grammar
/// (`service_common::iam_policy::resource_policy_defect`), each row below
/// breaking a different one, so a document this service stores is one a client
/// can read back as a policy.
#[tokio::test]
async fn a_policy_document_outside_the_policy_grammar_is_refused() {
    let harness = Harness::new();
    let bus = harness.bus("the grammar", "policy-grammar-bus").await;

    // An attached policy to compare against, so a refusal is visible as "nothing
    // changed" rather than as "nothing is there".
    let attached = account_policy(&bus, ACCOUNT);
    harness
        .put_default(&bus, &attached)
        .await
        .expect("case \"the grammar\": the first write must succeed");

    for (case, document) in [
        ("text that is not JSON", "not a policy".to_string()),
        ("truncated JSON", "{\"Statement\":".to_string()),
        ("JSON that is not an object", "[]".to_string()),
        ("an object carrying no statement", "{\"foo\":1}".to_string()),
        (
            "a policy version that does not exist",
            format!(
                r#"{{"Version":"3000-01-01","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}},"Action":"events:PutEvents","Resource":"{bus}"}}]}}"#
            ),
        ),
        (
            "a statement naming no principal",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"events:PutEvents","Resource":"{bus}"}}]}}"#
            ),
        ),
        (
            "a statement declaring no effect",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Principal":{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}},"Action":"events:PutEvents","Resource":"{bus}"}}]}}"#
            ),
        ),
    ] {
        let error = harness
            .put_default(&bus, &document)
            .await
            .expect_err(&format!("case {case:?}: the write must be refused"));
        assert_refusal(case, &error, "InvalidInputException", 400);
        let read = harness
            .get(&bus, None)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: the read must succeed, got {e:?}"));
        assert_eq!(
            read.policy_document, attached,
            "case {case:?}: a refused document must not be attached"
        );
    }
}

/// RULE, the refusal of an ARN that addresses no stored bus: every one of the
/// three operations answers `ResourceNotFoundException` (404) and attaches
/// nothing.
#[tokio::test]
async fn a_policy_operation_addressed_at_no_stored_bus_is_refused() {
    let harness = Harness::new();
    let bus = harness.bus("the refusals", "policy-refusal-bus").await;
    let document = account_policy(&bus, ACCOUNT);

    // Each row is a well-formed `EventBusArn` — its pattern is where the id
    // segment's `[a-z0-9]{25}` comes from — that addresses nothing.
    let cases: Vec<(&str, String)> = vec![
        (
            "an identifier this service never minted",
            format!("arn:aws:events:{REGION}:{ACCOUNT}:event-busv2/absent/aaaaaaaaaaaaaaaaaaaaaaaaa"),
        ),
        (
            "a bus of another region",
            format!("arn:aws:events:eu-west-1:{ACCOUNT}:event-busv2/absent/bbbbbbbbbbbbbbbbbbbbbbbbb"),
        ),
        (
            "a bus of another account",
            format!("arn:aws:events:{REGION}:111111111111:event-busv2/absent/ccccccccccccccccccccccccc"),
        ),
    ];

    for (case, arn) in cases {
        let error = harness
            .put_default(&arn, &document)
            .await
            .expect_err(&format!("case {case:?}: the write must be refused"));
        assert_refusal(case, &error, "ResourceNotFoundException", 404);

        let error = harness
            .get(&arn, None)
            .await
            .expect_err(&format!("case {case:?}: the read must be refused"));
        assert_refusal(case, &error, "ResourceNotFoundException", 404);

        let error = harness
            .list(&arn, 0, None)
            .await
            .expect_err(&format!("case {case:?}: the enumeration must be refused"));
        assert_refusal(case, &error, "ResourceNotFoundException", 404);
    }
}

// ---------------------------------------------------------------------------
// RULE again, where a client reads it: on the wire
// ---------------------------------------------------------------------------

/// The same rule over HTTP with rpcv2Cbor bodies, so the statuses and the
/// encoded bodies are the emulator's own: a wire write answers 200 with the
/// policy it attached, the wire read reports that document and revision, the
/// wire enumeration summarises it, and a wire write on a stale revision answers
/// 409.
#[tokio::test]
async fn the_wire_answers_carry_the_attached_policy() {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(eventsv2::EmulatedService::new(Arc::clone(&engine)));
    let case = "the wire round trip";

    let bus = cbor_ok::<CreateEventBus<InMemoryEngine>>(
        &router,
        &CreateEventBusInput {
            name: "policy-wire-bus".to_string(),
            client_token: Some("a-wire-policy-bus-token".to_string()),
            ..Default::default()
        },
        case,
    )
    .await
    .event_bus_arn
    .expect("case \"the wire round trip\": CreateEventBus reported no EventBusArn");
    let document = account_policy(&bus, ACCOUNT);

    let written = cbor_ok::<PutResourcePolicy<InMemoryEngine>>(
        &router,
        &PutResourcePolicyInput {
            resource_arn: bus.clone(),
            policy_document: document.clone(),
            ..Default::default()
        },
        case,
    )
    .await;
    let revision = written
        .revision_id
        .clone()
        .expect("case \"the wire round trip\": the wire write reported no RevisionId");
    assert_eq!(
        (written.resource_arn.as_str(), written.policy_name.as_str()),
        (bus.as_str(), DEFAULT_POLICY),
        "case {case:?}: the wire write must answer the bus and policy it wrote, got {written:?}"
    );

    let read = cbor_ok::<GetResourcePolicy<InMemoryEngine>>(
        &router,
        &GetResourcePolicyInput {
            resource_arn: bus.clone(),
            policy_name: None,
        },
        case,
    )
    .await;
    assert_eq!(
        (
            read.resource_arn.as_str(),
            read.policy_name.as_str(),
            read.policy_document.as_str(),
            read.revision_id.as_str()
        ),
        (
            bus.as_str(),
            DEFAULT_POLICY,
            document.as_str(),
            revision.as_str()
        ),
        "case {case:?}: the wire read must report the attached policy, got {read:?}"
    );

    let listed = cbor_ok::<ListResourcePolicies<InMemoryEngine>>(
        &router,
        &ListResourcePoliciesInput {
            resource_arn: bus.clone(),
            max_results: 100,
            next_token: None,
        },
        case,
    )
    .await;
    let summaries: Vec<(String, String)> = listed
        .policy_summaries
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|summary| (summary.policy_name, summary.revision_id))
        .collect();
    assert_eq!(
        summaries,
        vec![(DEFAULT_POLICY.to_string(), revision.clone())],
        "case {case:?}: the wire enumeration must summarise the attached policy, got {listed:?}"
    );

    // The precondition on the wire: a stale revision is the modelled 409.
    let (status, body) = cbor_call::<PutResourcePolicy<InMemoryEngine>>(
        &router,
        &PutResourcePolicyInput {
            resource_arn: bus.clone(),
            policy_document: document.clone(),
            expected_revision_id: Some(NO_POLICY.to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        status, 409,
        "case {case:?}: a stale precondition must answer 409, got {status} with {body:?}"
    );
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

/// The same call, requiring a 200 and decoding the typed output body.
async fn cbor_ok<O: Operation>(
    router: &MultiServiceRouter,
    input: &O::Input,
    case: &str,
) -> O::Output
where
    O::Output: DeserializableStruct,
{
    let (status, body) = cbor_call::<O>(router, input).await;
    assert_eq!(
        status,
        200,
        "case {case:?}: {} must answer 200, got {status} with {:?}",
        O::NAME,
        body
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
