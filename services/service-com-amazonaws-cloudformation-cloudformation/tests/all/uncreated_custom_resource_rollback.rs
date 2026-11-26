// SPDX-License-Identifier: CC0-1.0

//! A CUSTOM RESOURCE whose provider never answered its `Create`, and what the ROLLBACK of
//! that failed create then asks that provider for.
//!
//! RULE: a custom resource is held at the request CloudFormation sent its provider until the
//! provider PUTs an answer to the request's `ResponseURL`, and a request that goes unanswered
//! inside the wait fails that resource with CloudFormation's own sentence for it — the one
//! naming the `RequestId` the provider's log is searched by. CloudFormation does not know what
//! that provider did before it stopped answering, so the rollback of such a create asks it to
//! DELETE the resource rather than assuming there is nothing there: a `Delete` the provider
//! answers carries the rollback through (`ROLLBACK_COMPLETE`), and one it does not answer
//! either is a rollback that could not be carried out, which leaves the stack in the failed
//! rollback status (`ROLLBACK_FAILED`) — the state an operator has to clean up from, never the
//! complete one.
//!
//! A `DeletionPolicy` of `Retain` is the one declaration that asks nothing of the provider at
//! all ("CloudFormation keeps the resource without deleting the resource or its contents"), so
//! its rollback has no request to be answered and completes.
//!
//! The complement is here too: a custom resource the provider DID answer is deleted by the same
//! `Delete`, which is what says the rollback's own request is owed for the resource and not for
//! the answer.
//!
//! SECOND RULE, which is what makes the first one about an answer that never came rather than
//! about one there was no time for: **the window a wait gives whoever was asked runs from the
//! instant the CALL was made, not from the instant the stack operation started** — a stack
//! operation walks its resources one after another, so a resource it reaches late in a long
//! walk is asked about long after the operation began, and measuring the window from the
//! operation would leave such a resource only what is left of it, or nothing at all.
//!
//! Every case drives the emulator through its own wire: the stack operations over
//! CloudFormation's awsQuery endpoint, the provider's answer as the plain unsigned `PUT` to
//! the published `ResponseURL` a provider really makes, and the resources the rollback had to
//! remove read back through the service that OWNS them (AWS Systems Manager).
//!
//! The rows differ along what the rules generalise over: how the template spells the custom
//! resource's type (the author's own `Custom::<Name>` namespace and the generic
//! `AWS::CloudFormation::CustomResource`), how much the create had really made beside it that
//! the rollback has to remove (nothing, one resource, two), what the resource's own
//! `DeletionPolicy` says (the default, and `Retain`), whether the provider answers the
//! rollback's own `Delete`, and how long the operation had been walking by the time it reached
//! the resource (just past one window, and many windows).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Map, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::provisioning::custom_resource;
use service_com_amazonaws_cloudformation_cloudformation::state::stabilising::CREATE_STABILISATION_WINDOW;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};
use smithy_server_runtime::MultiServiceRouter;

use crate::custom_resource_providers::{
    awaited_request, emulator, provider_token, put_response, POLL_ATTEMPTS, POLL_INTERVAL,
};
use crate::harness::{call, create_stack, member, store, xml_text, REGION};
use crate::update_rollback_recovery::{
    parameter, parameter_value, reason_of, stack_events, template,
    template_with_an_unsignalled_gate,
};

/// The logical id every row declares its custom resource under.
const CUSTOM: &str = "Waiter";

/// The `PhysicalResourceId` the responsive provider of the complement below mints.
const MINTED: &str = "waiter-instance-1";

/// The suffix every status a stack operation in flight carries ends in.
const IN_PROGRESS: &str = "_IN_PROGRESS";

/// The request types a provider is sent, as the request document spells them.
const CREATE_REQUEST: &str = "Create";
const DELETE_REQUEST: &str = "Delete";

/// What the provider of one case does with the requests CloudFormation sends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    /// It answers nothing at all, so every wait for it runs out — reached by moving each
    /// wait past its own window rather than by sleeping it out.
    Silent,
    /// It answers only the `Delete` the rollback sends, which is the request a provider that
    /// crashed on the create can still be running for.
    AnswersTheDeletes,
    /// It answers whatever it is sent, minting the id carried here for a `Create`.
    AnswersEverything(&'static str),
}

/// One row: how the stack declares the custom resource whose provider never answers the
/// create, what the create had really made beside it, and whether the rollback's own `Delete`
/// is answered.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The custom resource's `Type`: `Custom::<Name>`, the author's own namespace, or
    /// `AWS::CloudFormation::CustomResource`, the generic spelling — the two forms a custom
    /// resource's type has.
    resource_type: &'static str,
    /// The `DeletionPolicy` the declaration carries, or `None` for the documented default.
    deletion_policy: Option<&'static str>,
    /// The status the rollback's teardown of the record reports for the custom resource:
    /// `DELETE_COMPLETE` for the default policy, and `DELETE_SKIPPED` for `Retain`, which is
    /// "the resource is kept without deleting the resource or its contents" — the same
    /// reporting as for every other resource the teardown asks nobody about.
    teardown: &'static str,
    /// Whether the rollback owes the provider a `Delete` for this declaration: it does for
    /// every policy but `Retain`, which releases the resource instead of removing it.
    owes_a_delete: bool,
    /// The Parameter Store names the create really made beside the custom resource, which
    /// the rollback has to delete: what proves the rollback RAN rather than merely reporting
    /// a status.
    siblings: &'static [&'static str],
}

/// The class the rule covers: a create no provider answered, however the stack declares it
/// and whatever else the create had made.
const ROWS: &[Row] = &[
    Row {
        case: "the generic type, with one sibling the rollback has to delete",
        stack: "uncreated-generic",
        resource_type: "AWS::CloudFormation::CustomResource",
        deletion_policy: None,
        teardown: resource_status::DELETE_COMPLETE,
        owes_a_delete: true,
        siblings: &["/uncreated-generic/sibling"],
    },
    Row {
        case: "the author's own namespace, with two siblings the rollback has to delete",
        stack: "uncreated-namespaced",
        resource_type: "Custom::SlowProvisioner",
        deletion_policy: None,
        teardown: resource_status::DELETE_COMPLETE,
        owes_a_delete: true,
        siblings: &["/uncreated-namespaced/first", "/uncreated-namespaced/second"],
    },
    Row {
        case: "the custom resource is the whole stack, so the rollback has nothing else",
        stack: "uncreated-alone",
        resource_type: "Custom::Waiter",
        deletion_policy: None,
        teardown: resource_status::DELETE_COMPLETE,
        owes_a_delete: true,
        siblings: &[],
    },
    Row {
        case: "a Retain policy, which asks the provider for nothing at all",
        stack: "uncreated-retained",
        resource_type: "Custom::RetainedWaiter",
        deletion_policy: Some("Retain"),
        teardown: resource_status::DELETE_SKIPPED,
        owes_a_delete: false,
        siblings: &["/uncreated-retained/sibling"],
    },
];

/// The custom resource one row declares: the provider topic as its `ServiceToken`, the row's
/// own `DeletionPolicy`, and a dependency on every sibling, so the custom resource is the
/// LAST resource the create walks — the shape a stack that deploys its own provider has.
fn custom_resource(row: &Row) -> Value {
    let mut declaration = Map::new();
    declaration.insert("Type".to_string(), json!(row.resource_type));
    declaration.insert(
        "Properties".to_string(),
        json!({"ServiceToken": provider_token()}),
    );
    if let Some(policy) = row.deletion_policy {
        declaration.insert("DeletionPolicy".to_string(), json!(policy));
    }
    if !row.siblings.is_empty() {
        declaration.insert(
            "DependsOn".to_string(),
            json!(sibling_ids(row.siblings.len())),
        );
    }
    Value::Object(declaration)
}

/// The logical ids the siblings of one row are declared under.
fn sibling_ids(count: usize) -> Vec<String> {
    (0..count).map(|index| format!("Sibling{index}")).collect()
}

/// The template one row deploys.
fn template_of(row: &Row) -> String {
    let mut resources = Map::new();
    for (logical_id, name) in sibling_ids(row.siblings.len())
        .into_iter()
        .zip(row.siblings)
    {
        resources.insert(logical_id, parameter(name, "made-by-the-create"));
    }
    resources.insert(CUSTOM.to_string(), custom_resource(row));
    template(Value::Object(resources))
}

/// Every provider request the stack's open waits carry right now: the `RequestType` of each
/// one and the logical id it is about.
///
/// This is what says WHICH requests CloudFormation sent the provider, because a request it
/// sent is one it is waiting for an answer to.
fn awaited_requests(
    engine: &Arc<engine::InMemoryEngine>,
    stack_name: &str,
) -> Vec<(String, String)> {
    let held = store(engine);
    let held = held.lock();
    let Some(stack) = held
        .stacks
        .iter()
        .find(|stack| stack.stack_name == stack_name)
    else {
        return Vec::new();
    };
    stack
        .stabilisations
        .iter()
        .filter_map(|barrier| {
            let request = barrier.provider_request.as_ref()?;
            Some((request.request_type.clone(), barrier.logical_id.clone()))
        })
        .collect()
}

/// Move the OPERATION behind the wait `by` further into the past — the instant it started,
/// both on the stack and on the wait's own record of it — leaving the instant the provider was
/// asked where it is.
///
/// This is the shape of an operation whose walk took that long to REACH the resource the wait
/// is about: it started long ago, and the request about this resource has only just gone out.
fn age_the_operation_behind_the_wait(
    engine: &Arc<engine::InMemoryEngine>,
    stack_name: &str,
    by: Duration,
) {
    let held = store(engine);
    let mut locked = held.lock();
    let stack = locked
        .resolve_mut(REGION, stack_name)
        .unwrap_or_else(|| panic!("the stack {stack_name:?} this case created"));
    let operation = stack
        .pending
        .as_mut()
        .unwrap_or_else(|| panic!("the operation {stack_name:?} has in flight"));
    operation.started_at -= by;
    for barrier in &mut stack.stabilisations {
        barrier.opened_at -= by;
    }
}

/// Move the wait for the provider's answer `by` further into the past, which is how a case
/// reaches the instant that wait had already run out at without waiting for it.
///
/// `pub(crate)` for the reason the helpers of `custom_resource_providers.rs` are: the sibling
/// suite `rolled_back_create_stays_describable.rs` drives a create to the same wait running
/// out, and one spelling of "reach the instant the wait had expired" keeps the two from
/// drifting about how that instant is reached.
pub(crate) fn age_the_provider_wait(
    engine: &Arc<engine::InMemoryEngine>,
    stack_name: &str,
    by: Duration,
) {
    assert!(
        aged_provider_waits(engine, stack_name, by) > 0,
        "case {stack_name:?}: expected the operation to be waiting for the provider's answer, \
         got no such wait at all"
    );
}

/// Move every wait the stack is holding for a provider's answer `by` further into the past,
/// and answer how many there were — the same move [`age_the_provider_wait`] asserts one of.
///
/// `pub(crate)` beside it because it asserts NOTHING: a suite driving a whole operation to the
/// end while nothing answers ages the waits of every one of its passes, and a pass between two
/// requests holds none at all.
pub(crate) fn aged_provider_waits(
    engine: &Arc<engine::InMemoryEngine>,
    stack_name: &str,
    by: Duration,
) -> usize {
    let held = store(engine);
    let mut locked = held.lock();
    let Some(stack) = locked.resolve_mut(REGION, stack_name) else {
        return 0;
    };
    let mut aged = 0;
    for barrier in &mut stack.stabilisations {
        if barrier.provider_request.is_some() && !barrier.released() {
            barrier.asked_at -= by;
            aged += 1;
        }
    }
    aged
}

/// Poll the stack until it leaves every `*_IN_PROGRESS` status, collecting every provider
/// request its waits carried on the way — with `provider` deciding what answers those requests.
///
/// A read is what asks a wait about, so polling is what a client does. The collected requests
/// are what the provider was really SENT, in the order the operation sent them.
async fn settled_watching_the_provider(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack_name: &str,
    provider: Provider,
) -> (String, Vec<(String, String)>) {
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut last = String::new();
    for _ in 0..POLL_ATTEMPTS {
        let (http_status, body) =
            call(router, "DescribeStacks", &[member("StackName", stack_name)]).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected DescribeStacks to answer, got {http_status} {body}"
        );
        last = xml_text(&body, "StackStatus").unwrap_or_default();
        for request in awaited_requests(engine, stack_name) {
            if !seen.contains(&request) {
                seen.push(request);
            }
        }
        if !last.ends_with(IN_PROGRESS) {
            return (last, seen);
        }
        match provider {
            // Nothing answers, so every wait this operation holds reaches the instant its
            // own window had run out.
            Provider::Silent => {
                aged_provider_waits(engine, stack_name, past_the_window());
            }
            Provider::AnswersTheDeletes => {
                answer_awaited_requests(router, engine, stack_name, None).await;
            }
            Provider::AnswersEverything(minted) => {
                answer_awaited_requests(router, engine, stack_name, Some(minted)).await;
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "case {case:?}: {stack_name:?} never left an in-progress status; last was {last:?} with \
         the requests {seen:?}"
    );
}

/// Answer the requests the stack is waiting for with `SUCCESS`, under the id each request
/// names where it names one (a `Delete`) and under `minted` for a `Create`.
///
/// `minted` is `None` for a provider that answers only what the ROLLBACK sends it: a `Create`
/// is then left unanswered, which is the request its own run never came back from.
async fn answer_awaited_requests(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    stack_name: &str,
    minted: Option<&str>,
) {
    let unanswered = {
        let held = store(engine);
        let held = held.lock();
        let Some(stack) = held
            .stacks
            .iter()
            .find(|stack| stack.stack_name == stack_name)
        else {
            return;
        };
        let stack_id = stack.stack_id.clone();
        let requests: Vec<(String, String, Option<String>)> = stack
            .stabilisations
            .iter()
            .filter(|barrier| !barrier.released() && barrier.provider_response.is_none())
            .filter_map(|barrier| {
                let request = barrier.provider_request.as_ref()?;
                Some((
                    barrier.logical_id.clone(),
                    request.request_id.clone(),
                    request.physical_resource_id.clone(),
                ))
            })
            .collect();
        (stack_id, requests)
    };
    let (stack_id, requests) = unanswered;
    for (logical_id, request_id, physical_resource_id) in requests {
        let id = match (physical_resource_id, minted) {
            (Some(named), _) => named,
            (None, Some(minted)) => minted.to_string(),
            // A request naming no physical resource is the `Create`, which this provider
            // leaves unanswered.
            (None, None) => continue,
        };
        put_response(
            router,
            engine,
            &stack_id,
            &logical_id,
            &request_id,
            &json!({"Status": "SUCCESS", "PhysicalResourceId": id}),
        )
        .await;
    }
}

/// One instant past the window a provider's answer is waited for, which is where the wait has
/// run out.
///
/// `pub(crate)` beside [`age_the_provider_wait`], which is what it is passed to: the sibling
/// suite `rolled_back_create_stays_describable.rs` ages the same wait, and one spelling of
/// "one instant past the window" keeps the two from drifting.
pub(crate) fn past_the_window() -> Duration {
    CREATE_STABILISATION_WINDOW + Duration::from_secs(1)
}

/// The whole `DescribeStackEvents` body of one stack: what the REASONS its events carry are
/// read from, which the status pairs [`stack_events`] reports do not carry.
async fn events_body(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let (http_status, body) = call(
        router,
        "DescribeStackEvents",
        &[member("StackName", stack_name)],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected DescribeStackEvents to answer, got {http_status} {body}"
    );
    body
}

/// The request CloudFormation is waiting for an answer to about the custom resource, as the
/// `RequestId` it is answered under — what the sentence a wait that ran out carries names.
fn awaited_request_id(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack_name: &str,
) -> String {
    awaited_request(engine, stack_name, CUSTOM)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a provider request about {CUSTOM} of {stack_name:?}, \
                 got none"
            )
        })
        .1
        .request_id
}

#[tokio::test]
async fn a_create_no_provider_answered_is_rolled_back_by_a_delete_to_that_provider() {
    for row in ROWS {
        let case = row.case;
        let (router, engine) = emulator(case).await;
        let (http_status, body) = create_stack(&router, row.stack, &template_of(row), &[]).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected CreateStack to succeed, got {http_status} {body}"
        );

        // The request IS with the provider: that is what the create is waiting for, and what
        // nothing is going to answer.
        let awaited = awaited_requests(&engine, row.stack);
        assert_eq!(
            awaited,
            vec![(CREATE_REQUEST.to_string(), CUSTOM.to_string())],
            "case {case:?}: expected the create to be waiting for one Create answer about \
             {CUSTOM}, got {awaited:?}"
        );
        let create_request = awaited_request_id(&engine, case, row.stack);

        // The wait runs out. What the clock then owes is the failed create and the rollback
        // its `OnFailure` default asks for — and this provider answers the `Delete` that
        // rollback sends it, so the rollback is carried through.
        age_the_provider_wait(&engine, row.stack, past_the_window());
        let (settled, seen) = settled_watching_the_provider(
            &router,
            &engine,
            case,
            row.stack,
            Provider::AnswersTheDeletes,
        )
        .await;
        assert_eq!(
            settled,
            status::ROLLBACK_COMPLETE,
            "case {case:?}: expected the rollback whose Delete the provider answered to \
             complete, got {settled:?} with the provider requests {seen:?}"
        );

        // ...and the provider WAS asked to delete the resource it never answered for:
        // CloudFormation does not know what it did before it stopped answering. A `Retain`
        // declaration is the one that asks it for nothing.
        let delete = (DELETE_REQUEST.to_string(), CUSTOM.to_string());
        assert_eq!(
            seen.contains(&delete),
            row.owes_a_delete,
            "case {case:?}: expected the provider to have been sent a Delete: \
             {}, got {seen:?}",
            row.owes_a_delete
        );

        // The event stream says what became of the resource: its create FAILED, carrying
        // CloudFormation's own sentence for a provider that did not answer — with the request
        // id that answer was owed for — the teardown of its record is reported as for every
        // other resource of this policy, and the stack's rollback completed.
        let events = stack_events(case, &router, row.stack).await;
        let failed = (CUSTOM.to_string(), resource_status::CREATE_FAILED.to_string());
        assert!(
            events.contains(&failed),
            "case {case:?}: expected the custom resource to report {failed:?}, got {events:?}"
        );
        let reason = custom_resource::unanswered_request_reason(&create_request);
        let reported = events_body(&router, case, row.stack).await;
        assert!(
            reported.contains(&reason),
            "case {case:?}: expected the failed create to report {reason:?}, got {reported}"
        );
        let taken_down = (CUSTOM.to_string(), row.teardown.to_string());
        assert!(
            events.contains(&taken_down),
            "case {case:?}: expected the custom resource's teardown to report {taken_down:?}, \
             got {events:?}"
        );
        let rolled_back = (row.stack.to_string(), status::ROLLBACK_COMPLETE.to_string());
        assert!(
            events.contains(&rolled_back),
            "case {case:?}: expected the stack's own {rolled_back:?} event, got {events:?}"
        );

        // ...and everything the create REALLY made is gone from the service that owns it: a
        // rollback that reported itself complete while leaving one behind would leave a live,
        // name-claiming resource.
        for sibling in row.siblings {
            let held = parameter_value(&router, sibling).await;
            assert_eq!(
                held, None,
                "case {case:?}: expected the rollback to delete the sibling {sibling:?} the \
                 create had made, got {held:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_delete_the_provider_does_not_answer_either_leaves_the_stack_rollback_failed() {
    // The same provider, now silent about the rollback's own `Delete` as well — the shape of
    // a provider whose every run fails before it answers. The rollback could not be carried
    // out, so the stack rests in the FAILED rollback status rather than reporting a clean one.
    for row in ROWS.iter().filter(|row| row.owes_a_delete) {
        let case = row.case;
        let (router, engine) = emulator(case).await;
        let (http_status, body) = create_stack(&router, row.stack, &template_of(row), &[]).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected CreateStack to succeed, got {http_status} {body}"
        );

        let (settled, seen) =
            settled_watching_the_provider(&router, &engine, case, row.stack, Provider::Silent)
                .await;
        assert_eq!(
            settled,
            status::ROLLBACK_FAILED,
            "case {case:?}: expected a rollback whose own Delete went unanswered to leave the \
             stack {}, got {settled:?} with the provider requests {seen:?}",
            status::ROLLBACK_FAILED
        );

        // Both requests were really sent: the create's, and the rollback's own.
        for expected in [
            (CREATE_REQUEST.to_string(), CUSTOM.to_string()),
            (DELETE_REQUEST.to_string(), CUSTOM.to_string()),
        ] {
            assert!(
                seen.contains(&expected),
                "case {case:?}: expected the provider to have been sent {expected:?}, got \
                 {seen:?}"
            );
        }

        // ...and the stack says WHICH resource it could not delete, which is what an operator
        // cleaning up after a stuck rollback reads, with the sentence for a provider that did
        // not answer on the resource's own event.
        let summary = reason_of(&router, row.stack).await;
        assert!(
            summary.contains(CUSTOM),
            "case {case:?}: expected the stack's reason to name {CUSTOM} as the resource the \
             rollback could not delete, got {summary:?}"
        );
        let events = stack_events(case, &router, row.stack).await;
        let rollback_failed = (row.stack.to_string(), status::ROLLBACK_FAILED.to_string());
        assert!(
            events.contains(&rollback_failed),
            "case {case:?}: expected the stack's own {rollback_failed:?} event, got {events:?}"
        );
        let reported = events_body(&router, case, row.stack).await;
        let sentence = "CloudFormation did not receive a response from your Custom Resource";
        assert!(
            reported.contains(sentence),
            "case {case:?}: expected the failed rollback to report {sentence:?}, got {reported}"
        );
    }
}

/// One row of the second rule's table: how long the operation had already been running when
/// the provider was sent its request.
struct LongWalk {
    case: &'static str,
    stack: &'static str,
    resource_type: &'static str,
    /// How many windows' worth of walk the operation had behind it by the time it reached the
    /// custom resource: the near bound (just past one window) and a walk many windows long.
    windows: u32,
}

/// The class the second rule covers: an operation that reaches the resource LATE, however
/// late.
const LONG_WALKS: &[LongWalk] = &[
    LongWalk {
        case: "a walk that had just outlasted one window when it reached the resource",
        stack: "late-by-one-window",
        resource_type: "AWS::CloudFormation::CustomResource",
        windows: 1,
    },
    LongWalk {
        case: "a walk many windows long when it reached the resource",
        stack: "late-by-many-windows",
        resource_type: "Custom::LateProvisioner",
        windows: 5,
    },
];

#[tokio::test]
async fn the_wait_for_an_answer_runs_from_the_call_and_not_from_the_operations_start() {
    // The wait is the span the thing that was ASKED is given to carry the call out, so it runs
    // from the call: a resource an operation reaches late in a long walk gets the whole window
    // from its own request rather than what is left of the operation's — and none at all once
    // the operation has outlasted it.
    for row in LONG_WALKS {
        let case = row.case;
        let template_body = template(json!({
            CUSTOM: {
                "Type": row.resource_type,
                "Properties": {"ServiceToken": provider_token()},
            },
        }));
        let (router, engine) = emulator(case).await;
        let (http_status, body) = create_stack(&router, row.stack, &template_body, &[]).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected CreateStack to succeed, got {http_status} {body}"
        );

        // The operation has been running far longer than one window; the request to the
        // provider has only just gone out.
        age_the_operation_behind_the_wait(&engine, row.stack, past_the_window() * row.windows);
        for look in 0..3 {
            let (status_code, body) =
                call(&router, "DescribeStacks", &[member("StackName", row.stack)]).await;
            assert_eq!(
                status_code, 200,
                "case {case:?}: expected DescribeStacks to answer, got {status_code} {body}"
            );
            let reported = xml_text(&body, "StackStatus").unwrap_or_default();
            assert_eq!(
                reported,
                status::CREATE_IN_PROGRESS,
                "case {case:?}: expected the create to still be waiting for the provider's \
                 answer on look {look}, got {reported:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }

        // ...and the wait does END — from the instant of the call, which is what aging THAT
        // instant reaches. This provider answers nothing, so the rollback's own `Delete` goes
        // unanswered too and the stack rests in the failed rollback status.
        age_the_provider_wait(&engine, row.stack, past_the_window());
        let (settled, seen) =
            settled_watching_the_provider(&router, &engine, case, row.stack, Provider::Silent)
                .await;
        assert_eq!(
            settled,
            status::ROLLBACK_FAILED,
            "case {case:?}: expected the wait measured from the call to run out and the create \
             to roll back, got {settled:?} with the provider requests {seen:?}"
        );
    }
}

#[tokio::test]
async fn a_create_the_provider_answered_is_deleted_by_the_rollback() {
    // The complement of the rule: this provider ANSWERS, so the custom resource really
    // exists — and the create fails anyway, at a wait condition nothing signals. The
    // rollback then owes that provider the `Delete` of the resource it minted.
    let case = "a provider that answered the create, with the create failed by its own gate";
    let stack = "answered-then-rolled-back";
    let (router, engine) = emulator(case).await;
    let declared = json!({
        CUSTOM: {
            "Type": "Custom::AnsweringProvisioner",
            "Properties": {"ServiceToken": provider_token()},
        },
    });
    let (http_status, body) = create_stack(
        &router,
        stack,
        &template_with_an_unsignalled_gate(declared),
        &[],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected CreateStack to succeed, got {http_status} {body}"
    );

    let (settled, seen) = settled_watching_the_provider(
        &router,
        &engine,
        case,
        stack,
        Provider::AnswersEverything(MINTED),
    )
    .await;
    assert_eq!(
        settled,
        status::ROLLBACK_COMPLETE,
        "case {case:?}: expected the create failed by its gate to roll back, got {settled:?} \
         with the provider requests {seen:?}"
    );
    for expected in [
        (CREATE_REQUEST.to_string(), CUSTOM.to_string()),
        (DELETE_REQUEST.to_string(), CUSTOM.to_string()),
    ] {
        assert!(
            seen.contains(&expected),
            "case {case:?}: expected the provider to have been sent {expected:?}, got {seen:?}"
        );
    }
}
