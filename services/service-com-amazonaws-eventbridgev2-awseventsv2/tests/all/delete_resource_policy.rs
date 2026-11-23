// SPDX-License-Identifier: CC0-1.0

//! `DeleteResourcePolicy`: detaching a named resource policy from an event bus.
//!
//! RULE: a delete detaches the policy its `ResourceArn` and `PolicyName`
//! address, answers the revision the store held for it, and leaves the bus
//! holding nothing under that name, so every later read reports the policy gone;
//! a delete that finds no such policy removes nothing and answers no revision —
//! an ARN naming no bus of this store holds no policy either, so it is answered
//! the same way — which makes an unconditional delete idempotent. A delete
//! conditioned on `ExpectedRevisionId` is applied only when that value is the
//! policy's current revision and is otherwise refused with `ConflictException`,
//! leaving what is stored untouched; the `NO_POLICY` sentinel is not a value the
//! precondition can carry and is refused with `InvalidInputException`. A policy
//! name outside the two the service admits is refused with
//! `InvalidInputException`, and the Resource Access Manager policy is refused to
//! the bus owner with `AccessDeniedException`. Nothing is detached by a refused
//! delete.
//!
//! The rows span that class rather than one recorded delete: documents differing
//! in the shape of their `Statement`, their statement count, their character set
//! and their size; the policy addressed by name and by omission; preconditions
//! differing in which revision they name; every policy name the model admits and
//! several it does not; and ARNs that address no bus of this store.
//!
//! Nothing here waits on the wall clock: the one lifecycle span a row needs is
//! waited out by polling for the condition it owes.

use std::sync::Arc;
use std::time::Instant;

use engine::InMemoryEngine;
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError,
};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::delete_event_bus::DeleteEventBus;
use eventsv2::ops::delete_resource_policy::{DeleteResourcePolicy, DeleteResourcePolicyError};
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::get_resource_policy::{GetResourcePolicy, GetResourcePolicyError};
use eventsv2::ops::list_resource_policies::{ListResourcePolicies, ListResourcePoliciesError};
use eventsv2::ops::put_resource_policy::{PutResourcePolicy, PutResourcePolicyError};
use eventsv2::ops::types::{
    CreateEventBusInput, DeleteEventBusInput, DeleteResourcePolicyInput,
    DeleteResourcePolicyOutput, DescribeEventBusInput, GetResourcePolicyInput,
    GetResourcePolicyOutput, ListResourcePoliciesInput, ListResourcePoliciesOutput,
    PutResourcePolicyInput, PutResourcePolicyOutput,
};

use crate::modelled_input_refusal::{POLL_DEADLINE, POLL_INTERVAL};

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The region every request below is signed for; it must reach the minted ARNs.
const REGION: &str = "us-west-2";

/// The policy an operation that names no `PolicyName` addresses, per
/// `PolicyName`'s documented value list.
const DEFAULT_POLICY: &str = "default";

/// The policy AWS Resource Access Manager owns, the other entry of that list.
const AWS_RAM_POLICY: &str = "AWS_RAM";

/// The `ExpectedRevisionId` sentinel standing for a policy that does not exist
/// yet, which `DeleteResourcePolicy`'s `ExpectedRevisionId` documents as "not
/// valid here".
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
        .uri("/service/AWSEventsV2/operation/DeleteResourcePolicy")
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
    /// Manager makes.
    fn store(&self) -> engine::ServiceState<eventsv2::ComAmazonawsEventbridgev2Awseventsv2State> {
        eventsv2::state::service_state(&*self.engine)
            .expect("the in-memory engine always has this service's store")
    }

    /// A bus called `name`, as its create minted it. A policy delete needs no
    /// particular lifecycle state of the bus — `DeleteResourcePolicy` models no
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

    /// The bus deleted and gone from the registry: the delete starts the span
    /// the clock owes its disappearance, and the loop looks again until a read
    /// no longer resolves it. It waits for that CONDITION rather than for a
    /// fixed time, and stops as soon as it holds.
    async fn forget_bus(&self, case: &str, bus: &str) {
        DeleteEventBus::new(Arc::clone(&self.engine))
            .handle(
                DeleteEventBusInput {
                    event_bus_arn: bus.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: DeleteEventBus must succeed, got {e:?}"));

        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let resolved = DescribeEventBus::new(Arc::clone(&self.engine))
                .handle(
                    DescribeEventBusInput {
                        event_bus_arn: bus.to_string(),
                    },
                    &self.ctx(),
                )
                .await;
            if resolved.is_err() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the bus {bus:?} must leave the registry, but a read still \
                 resolves it"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    async fn put(
        &self,
        input: PutResourcePolicyInput,
    ) -> Result<PutResourcePolicyOutput, PutResourcePolicyError> {
        PutResourcePolicy::new(Arc::clone(&self.engine))
            .handle(input, &self.ctx())
            .await
    }

    /// An unconditional write of `document` to the customer-managed policy, and
    /// the revision it minted.
    async fn put_default(&self, case: &str, bus: &str, document: &str) -> String {
        self.put(PutResourcePolicyInput {
            resource_arn: bus.to_string(),
            policy_document: document.to_string(),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("case {case:?}: PutResourcePolicy must succeed, got {e:?}"))
        .revision_id
        .unwrap_or_else(|| panic!("case {case:?}: PutResourcePolicy reported no RevisionId"))
    }

    async fn delete(
        &self,
        input: DeleteResourcePolicyInput,
    ) -> Result<DeleteResourcePolicyOutput, DeleteResourcePolicyError> {
        DeleteResourcePolicy::new(Arc::clone(&self.engine))
            .handle(input, &self.ctx())
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
    ) -> Result<ListResourcePoliciesOutput, ListResourcePoliciesError> {
        ListResourcePolicies::new(Arc::clone(&self.engine))
            .handle(
                ListResourcePoliciesInput {
                    resource_arn: bus.to_string(),
                    max_results: 0,
                    next_token: None,
                },
                &self.ctx(),
            )
            .await
    }

    /// The policy name and revision of every policy the bus reports, in the
    /// order it reports them.
    async fn summaries(&self, case: &str, bus: &str) -> Vec<(String, String)> {
        self.list(bus)
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
        let revision = eventsv2::state::new_policy_revision_id(
            state.next_id_seed(),
            std::time::SystemTime::now(),
        );
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
        "case {case:?}: expected {code} ({status}), got {error:?}"
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
/// padded out, so a row about a LARGE document differs from its neighbour only
/// in size.
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

/// The policies deleted in turn, spanning what the delete rule generalises over:
/// the document (the shape of `Statement`, the statement count, the character
/// set, the size) and how the request addresses the policy — by naming
/// "default" or by naming nothing at all, which resolves to the same policy.
fn deletable_policies(bus: &str) -> Vec<(&'static str, String, Option<&'static str>)> {
    vec![
        (
            "one statement in a list, the policy named",
            account_policy(bus, ACCOUNT),
            Some(DEFAULT_POLICY),
        ),
        (
            "one statement in a list, the policy left to the default",
            account_policy(bus, ACCOUNT),
            None,
        ),
        (
            "a single statement as an object rather than a list",
            format!(
                r#"{{"Version":"2012-10-17","Statement":{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}},"Action":"events:PutEvents","Resource":"{bus}"}}}}"#
            ),
            None,
        ),
        (
            "two statements, the second an explicit Deny",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"Allow","Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}},"Action":"events:PutEvents","Resource":"{bus}"}},{{"Sid":"Deny","Effect":"Deny","Principal":"*","Action":"events:PutEvents","Resource":"{bus}","Condition":{{"StringNotEquals":{{"aws:PrincipalAccount":"{ACCOUNT}"}}}}}}]}}"#
            ),
            Some(DEFAULT_POLICY),
        ),
        (
            "a document carrying non-ASCII text",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"Zürich-Tokyo-東京","Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}},"Action":"events:PutEvents","Resource":"{bus}"}}]}}"#
            ),
            None,
        ),
        (
            "a document just under the policy size quota",
            document_of_size(bus, 20_000),
            Some(DEFAULT_POLICY),
        ),
    ]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// RULE, the delete: whatever was attached, the delete answers the revision the
/// store held for it and the bus holds nothing under that name afterwards — the
/// read is refused and the enumeration reports none — and a second delete of the
/// same policy removes nothing and answers no revision.
#[tokio::test]
async fn a_deleted_policy_is_gone_from_every_later_read() {
    let harness = Harness::new();
    let bus = harness.bus("the delete", "policy-delete-bus").await;

    for (case, document, policy_name) in deletable_policies(&bus) {
        let revision = harness.put_default(case, &bus, &document).await;

        let deleted = harness
            .delete(DeleteResourcePolicyInput {
                resource_arn: bus.clone(),
                policy_name: policy_name.map(str::to_string),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| {
                panic!("case {case:?}: DeleteResourcePolicy must succeed, got {e:?}")
            });
        assert_eq!(
            deleted.revision_id.as_deref(),
            Some(revision.as_str()),
            "case {case:?}: the delete must report the revision it removed, got {deleted:?}"
        );

        // The policy is gone from every later read of the bus.
        let Err(error) = harness.get(&bus, policy_name).await else {
            panic!("case {case:?}: the read of a deleted policy must be refused")
        };
        assert_refusal(case, &error, "ResourceNotFoundException", 404);
        let summaries = harness.summaries(case, &bus).await;
        assert!(
            summaries.is_empty(),
            "case {case:?}: a bus whose only policy was deleted must report none, got \
             {summaries:?}"
        );

        // Deleting what is no longer there removes nothing and answers no
        // revision: an unconditional delete is idempotent.
        let again = harness
            .delete(DeleteResourcePolicyInput {
                resource_arn: bus.clone(),
                policy_name: policy_name.map(str::to_string),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: the repeated delete must succeed, got {e:?}"));
        assert_eq!(
            again.revision_id, None,
            "case {case:?}: a delete that removed nothing must report no RevisionId, got \
             {again:?}"
        );
    }

    // And the bus still takes a policy after the delete: what the delete removed
    // was the policy, not the bus's ability to hold one. The revision is a NEW
    // one, never a revision the store handed out before.
    let case = "a write after the delete";
    let document = account_policy(&bus, ACCOUNT);
    let revision = harness.put_default(case, &bus, &document).await;
    let read = harness
        .get(&bus, None)
        .await
        .unwrap_or_else(|e| panic!("case {case:?}: GetResourcePolicy must succeed, got {e:?}"));
    assert_eq!(
        (read.policy_document.as_str(), read.revision_id.as_str()),
        (document.as_str(), revision.as_str()),
        "case {case:?}: the read must report the policy written after the delete, got {read:?}"
    );
}

/// RULE, the scope of a delete: a policy is attached to ONE bus, so deleting it
/// from one leaves every other bus's policy exactly as it was.
#[tokio::test]
async fn a_delete_touches_only_the_bus_it_addresses() {
    let harness = Harness::new();
    let case = "two buses, one delete";
    let first = harness.bus(case, "policy-delete-scope-one").await;
    let second = harness.bus(case, "policy-delete-scope-two").await;

    let first_document = account_policy(&first, ACCOUNT);
    let second_document = account_policy(&second, "111111111111");
    harness.put_default(case, &first, &first_document).await;
    let kept = harness.put_default(case, &second, &second_document).await;

    harness
        .delete(DeleteResourcePolicyInput {
            resource_arn: first.clone(),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("case {case:?}: DeleteResourcePolicy must succeed, got {e:?}"));

    let summaries = harness.summaries(case, &second).await;
    assert_eq!(
        summaries,
        vec![(DEFAULT_POLICY.to_string(), kept.clone())],
        "case {case:?}: the other bus must still report its own policy, got {summaries:?}"
    );
    let read = harness
        .get(&second, None)
        .await
        .unwrap_or_else(|e| panic!("case {case:?}: the other bus's read must succeed, got {e:?}"));
    assert_eq!(
        (read.policy_document.as_str(), read.revision_id.as_str()),
        (second_document.as_str(), kept.as_str()),
        "case {case:?}: the other bus's policy must be untouched, got {read:?}"
    );
}

/// RULE, the precondition: a conditional delete is applied only when
/// `ExpectedRevisionId` is the policy's current revision; every other value is
/// refused — `ConflictException` for a revision that is not the current one,
/// `InvalidInputException` for the sentinel the delete may not carry — and
/// leaves the policy exactly as it was.
#[tokio::test]
async fn a_conditional_delete_applies_only_when_the_revision_matches() {
    let harness = Harness::new();
    let bus = harness.bus("the precondition", "policy-delete-condition-bus").await;

    // Two writes in turn, so one revision is CURRENT and one is stale.
    let first_document = account_policy(&bus, ACCOUNT);
    let stale = harness.put_default("the first write", &bus, &first_document).await;
    let document = account_policy(&bus, "111111111111");
    let current = harness.put_default("the second write", &bus, &document).await;
    assert_ne!(
        stale, current,
        "every write mints a new revision, but {stale:?} was handed out twice"
    );

    // Rows differing in which revision the precondition names, and in the form
    // of a value no write ever minted.
    let refused: Vec<(&str, String, &str, u16)> = vec![
        (
            "a revision superseded by a later write",
            stale.clone(),
            "ConflictException",
            409,
        ),
        (
            "a revision this store never minted",
            "not-a-revision-this-store-minted".to_string(),
            "ConflictException",
            409,
        ),
        (
            "a single character, which is the shortest the member admits",
            "x".to_string(),
            "ConflictException",
            409,
        ),
        (
            "the sentinel for a policy that does not exist yet",
            NO_POLICY.to_string(),
            "InvalidInputException",
            400,
        ),
    ];

    for (case, expected, code, status) in refused {
        let Err(error) = harness
            .delete(DeleteResourcePolicyInput {
                resource_arn: bus.clone(),
                expected_revision_id: Some(expected.clone()),
                ..Default::default()
            })
            .await
        else {
            panic!("case {case:?}: the delete expecting {expected:?} must be refused")
        };
        assert_refusal(case, &error, code, status);

        // A refused delete detaches nothing: the policy is still exactly what
        // the last successful write stored.
        let read = harness
            .get(&bus, None)
            .await
            .unwrap_or_else(|e| panic!("case {case:?}: the policy must still be readable, got {e:?}"));
        assert_eq!(
            (read.policy_document.as_str(), read.revision_id.as_str()),
            (document.as_str(), current.as_str()),
            "case {case:?}: a refused delete must leave the policy untouched, got {read:?}"
        );
    }

    // The current revision is the one value that applies.
    let case = "the current revision";
    let deleted = harness
        .delete(DeleteResourcePolicyInput {
            resource_arn: bus.clone(),
            expected_revision_id: Some(current.clone()),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("case {case:?}: the conditional delete must succeed, got {e:?}"));
    assert_eq!(
        deleted.revision_id.as_deref(),
        Some(current.as_str()),
        "case {case:?}: the delete must report the revision it removed, got {deleted:?}"
    );

    // And a conditional delete is NOT idempotent: the revision it named no
    // longer exists, so retrying it is the conflict the member documents, while
    // the policy really is gone.
    let case = "the conditional delete retried";
    let Err(error) = harness
        .delete(DeleteResourcePolicyInput {
            resource_arn: bus.clone(),
            expected_revision_id: Some(current.clone()),
            ..Default::default()
        })
        .await
    else {
        panic!("case {case:?}: the retried conditional delete must be refused")
    };
    assert_refusal(case, &error, "ConflictException", 409);
    let Err(error) = harness.get(&bus, None).await else {
        panic!("case {case:?}: the read of the deleted policy must be refused")
    };
    assert_refusal(case, &error, "ResourceNotFoundException", 404);
}

/// RULE, which names a delete admits: the bus owner deletes the
/// customer-managed policy only — naming the Resource Access Manager policy is
/// refused with `AccessDeniedException` — and a well-formed name that is neither
/// of the two addresses no policy at all, refused with `InvalidInputException`.
/// Neither refusal detaches anything.
#[tokio::test]
async fn a_delete_admits_only_the_policy_the_bus_owner_writes() {
    let harness = Harness::new();
    let case = "the two policies";
    let bus = harness.bus(case, "policy-delete-names-bus").await;
    let document = account_policy(&bus, ACCOUNT);
    let default_revision = harness.put_default(case, &bus, &document).await;
    let ram_document = account_policy(&bus, "111111111111");
    let ram_revision = harness.store_ram_policy(case, &bus, &ram_document);

    // Rows spanning every policy name the model admits beyond the
    // customer-managed one, and several it does not: the catalog is
    // `PolicyName`'s documented value list — "default" and "AWS_RAM" — so a name
    // outside it is not a policy of this service, however well formed.
    let refused: Vec<(&str, &str, &str, u16)> = vec![
        (
            "the policy AWS Resource Access Manager owns",
            AWS_RAM_POLICY,
            "AccessDeniedException",
            403,
        ),
        (
            "the customer-managed name in another case",
            "Default",
            "InvalidInputException",
            400,
        ),
        (
            "the Resource Access Manager name in another case",
            "aws_ram",
            "InvalidInputException",
            400,
        ),
        (
            "a name of the caller's own",
            "my_policy",
            "InvalidInputException",
            400,
        ),
        (
            "a name extending one of the two",
            "AWS_RAM_2",
            "InvalidInputException",
            400,
        ),
    ];

    for (case, policy_name, code, status) in refused {
        let Err(error) = harness
            .delete(DeleteResourcePolicyInput {
                resource_arn: bus.clone(),
                policy_name: Some(policy_name.to_string()),
                ..Default::default()
            })
            .await
        else {
            panic!("case {case:?}: deleting {policy_name:?} must be refused")
        };
        assert_refusal(case, &error, code, status);
    }

    // Nothing the refusals addressed was detached: the bus still reports both
    // policies, each with the revision it was stored under.
    let summaries = harness.summaries(case, &bus).await;
    assert_eq!(
        summaries,
        vec![
            (AWS_RAM_POLICY.to_string(), ram_revision.clone()),
            (DEFAULT_POLICY.to_string(), default_revision.clone()),
        ],
        "case {case:?}: a refused delete must detach nothing, got {summaries:?}"
    );
}

/// RULE, the policy a delete addresses: an unconditional delete reports what it
/// removed, so every ARN holding no policy under the requested name — a bus this
/// store holds and one it does not — answers 200 with no `RevisionId` rather
/// than a refusal.
#[tokio::test]
async fn an_unconditional_delete_of_a_policy_that_is_not_there_succeeds() {
    let harness = Harness::new();

    // A bus this store really holds, and one it has held and retired: both are
    // addressed exactly like the ARNs below, which is what makes the rows about
    // the POLICY rather than about the shape of the value.
    let held = harness
        .bus("a bus this store holds", "policy-delete-present-bus")
        .await;
    let retired = harness
        .bus("a bus this store retired", "policy-delete-retired-bus")
        .await;
    let retired_document = account_policy(&retired, ACCOUNT);
    harness
        .put_default("the retired bus's policy", &retired, &retired_document)
        .await;
    harness
        .forget_bus("a bus this store retired", &retired)
        .await;

    // Rows spanning what the rule generalises over: whether the ARN names a bus
    // of this store at all, and which part of a well-formed `EventBusArn` —
    // its pattern is where the id segment's `[a-z0-9]{25}` comes from — names
    // something else. Each names no attached policy.
    let cases: Vec<(&str, String, Option<&str>)> = vec![
        ("a bus this store holds, holding no policy", held.clone(), None),
        (
            "a bus this store holds, the policy named rather than defaulted",
            held.clone(),
            Some(DEFAULT_POLICY),
        ),
        (
            "a bus this store minted and has retired, policy and all",
            retired.clone(),
            None,
        ),
        (
            "an identifier this service never minted",
            format!("arn:aws:events:{REGION}:{ACCOUNT}:event-busv2/absent/aaaaaaaaaaaaaaaaaaaaaaaaa"),
            None,
        ),
        (
            "a bus of another region",
            format!("arn:aws:events:eu-west-1:{ACCOUNT}:event-busv2/absent/bbbbbbbbbbbbbbbbbbbbbbbbb"),
            Some(DEFAULT_POLICY),
        ),
        (
            "a bus of another account",
            format!("arn:aws:events:{REGION}:111111111111:event-busv2/absent/ccccccccccccccccccccccccc"),
            None,
        ),
        (
            "a bus name no create of this store ever used",
            format!("arn:aws:events:{REGION}:{ACCOUNT}:event-busv2/never-created/ddddddddddddddddddddddddd"),
            None,
        ),
    ];

    for (case, arn, policy_name) in cases {
        let served = harness
            .delete(DeleteResourcePolicyInput {
                resource_arn: arn.clone(),
                policy_name: policy_name.map(str::to_string),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| {
                panic!("case {case:?}: deleting on {arn:?} must succeed, got {e:?}")
            });
        assert_eq!(
            served.revision_id, None,
            "case {case:?}: a delete that removed nothing must report no RevisionId, got \
             {served:?}"
        );

        // Repeating it answers the same thing: the success says nothing was
        // there, so nothing about the store changed.
        let again = harness
            .delete(DeleteResourcePolicyInput {
                resource_arn: arn.clone(),
                policy_name: policy_name.map(str::to_string),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| {
                panic!("case {case:?}: the repeated delete on {arn:?} must succeed, got {e:?}")
            });
        assert_eq!(
            again.revision_id, None,
            "case {case:?}: the repeated delete must report no RevisionId, got {again:?}"
        );
    }

    // The success is about what was attached, not about the ARN: a bus this
    // store holds WITH a policy still answers the revision it removed.
    let case = "a policy really attached";
    let document = account_policy(&held, ACCOUNT);
    let revision = harness.put_default(case, &held, &document).await;
    let deleted = harness
        .delete(DeleteResourcePolicyInput {
            resource_arn: held.clone(),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("case {case:?}: DeleteResourcePolicy must succeed, got {e:?}"));
    assert_eq!(
        deleted.revision_id.as_deref(),
        Some(revision.as_str()),
        "case {case:?}: the delete must report the revision it removed, got {deleted:?}"
    );
}

/// RULE, the precondition on a policy that is not there: `ExpectedRevisionId`
/// names a revision that must exist, so a conditional delete addressed where no
/// policy is attached — an ARN naming no bus of this store included — is the
/// conflict the member documents, while the sentinel it may not carry stays the
/// invalid parameter it is.
#[tokio::test]
async fn a_conditional_delete_of_a_policy_that_is_not_there_is_refused() {
    let harness = Harness::new();
    let held = harness.bus("a bus this store holds", "policy-delete-condition-absent").await;
    let absent =
        format!("arn:aws:events:{REGION}:{ACCOUNT}:event-busv2/absent/eeeeeeeeeeeeeeeeeeeeeeeee");

    // Rows differing in whether the ARN names a bus of this store, and in the
    // precondition the request carries.
    let cases: Vec<(&str, String, &str, &str, u16)> = vec![
        (
            "a bus this store holds, a revision it never minted",
            held.clone(),
            "not-a-revision-this-store-minted",
            "ConflictException",
            409,
        ),
        (
            "a bus this store holds, the sentinel a delete may not carry",
            held.clone(),
            NO_POLICY,
            "InvalidInputException",
            400,
        ),
        (
            "no bus of this store, a revision it never minted",
            absent.clone(),
            "not-a-revision-this-store-minted",
            "ConflictException",
            409,
        ),
        (
            "no bus of this store, the sentinel a delete may not carry",
            absent.clone(),
            NO_POLICY,
            "InvalidInputException",
            400,
        ),
    ];

    for (case, arn, expected, code, status) in cases {
        let Err(error) = harness
            .delete(DeleteResourcePolicyInput {
                resource_arn: arn.clone(),
                expected_revision_id: Some(expected.to_string()),
                ..Default::default()
            })
            .await
        else {
            panic!("case {case:?}: the delete on {arn:?} expecting {expected:?} must be refused")
        };
        assert_refusal(case, &error, code, status);
    }
}
