// SPDX-License-Identifier: CC0-1.0

//! The WINDOW a custom resource's provider is given to answer the request CloudFormation sent
//! it, where the resource's own declaration states that window.
//!
//! RULE: **a custom resource is held at the request its provider was sent until that provider
//! answers at the request's `ResponseURL`, and the wait is bounded by the span the declaration
//! asked for — `ServiceTimeout`, "the maximum time, in seconds, that can elapse before a custom
//! resource operation times out" — so a resource whose declaration states a span is still held
//! inside it, and fails with CloudFormation's own sentence for an answer that never came once
//! that span has closed.** A declaration that states no span at all has asked for no particular
//! window and is held to this service's own ceiling on an open wait, so the stack reaches a
//! terminal status either way: no stack whose only resource is a Lambda- or topic-backed custom
//! resource stays `*_IN_PROGRESS` for ever, whatever its provider does.
//!
//! The rows differ along what the rule generalises over: whether the declaration states a span
//! at all, how it spells one (a JSON number and text, since a property value is text however
//! the template wrote it), and where the span falls — the documented minimum, a span shorter
//! than this service's own ceiling, a span longer than it, and the documented maximum. A span
//! OUTSIDE the documented range is refused before any provider is asked anything, which is
//! `stack_configuration_and_provider_limits.rs`'s row rather than one here.
//!
//! Nothing here sleeps a window out: the instant the provider was ASKED is what the window runs
//! from, so a case reaches the instant its window had closed by moving that instant into the
//! past — the same move `uncreated_custom_resource_rollback.rs` makes — and every status is read
//! back over CloudFormation's own wire.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Map, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::provisioning::custom_resource;
use service_com_amazonaws_cloudformation_cloudformation::state::stabilising::CREATE_STABILISATION_WINDOW;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};
use smithy_server_runtime::MultiServiceRouter;

use crate::custom_resource_providers::{
    awaited_request, emulator, provider_token, POLL_ATTEMPTS, POLL_INTERVAL,
};
use crate::harness::{call, create_stack, failed_resource_reason, member, xml_text, REGION};
use crate::uncreated_custom_resource_rollback::aged_provider_waits;

/// The logical id every row declares its custom resource under.
const CUSTOM: &str = "Waiter";

/// The suffix every status a stack operation in flight carries ends in.
const IN_PROGRESS: &str = "_IN_PROGRESS";

/// The `RequestType` the create sends the provider, as the request document spells it.
const CREATE_REQUEST: &str = "Create";

/// One instant past a window, which is where a wait of that window has run out.
const ONE_INSTANT: Duration = Duration::from_secs(1);

/// One row: how the declaration states the window its provider is given, and how long that
/// window then is.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The custom resource's `Type`: the author's own `Custom::<Name>` namespace or the
    /// generic `AWS::CloudFormation::CustomResource`.
    resource_type: &'static str,
    /// The `ServiceTimeout` the declaration states, exactly as the template spells it, or
    /// `None` for a declaration that states none.
    declared: fn() -> Option<Value>,
    /// How long the wait for the provider's answer then runs.
    window: fn() -> Duration,
    /// How far into its window the row looks to find the wait STILL HOLDING, or `None` for a
    /// window too short to look inside of on any host.
    ///
    /// Each row looks at an age no pass of this service would give up at for any OTHER reason:
    /// a row whose declared span is longer than this service's own ceiling looks just past
    /// that ceiling, which is what says the span the declaration asked for is the one in
    /// force.
    holds_at: fn() -> Option<Duration>,
}

/// The class the rule covers: every window a declaration can ask for, and the one it asks for
/// nothing with.
///
/// The spans are the property's own documented range — "the value must be an integer from 1 to
/// 14400" (`AWS::CloudFormation::CustomResource`'s `ServiceTimeout`) — at each published bound
/// and on either side of this service's ceiling on an open wait.
const ROWS: &[Row] = &[
    Row {
        case: "no ServiceTimeout at all, so the service's own ceiling bounds the wait",
        stack: "service-timeout-unstated",
        resource_type: "AWS::CloudFormation::CustomResource",
        declared: || None,
        window: || CREATE_STABILISATION_WINDOW,
        holds_at: || Some(CREATE_STABILISATION_WINDOW / 2),
    },
    Row {
        case: "a span stated as a JSON number, shorter than that ceiling",
        stack: "service-timeout-short",
        resource_type: "Custom::SlowProvisioner",
        declared: || Some(json!(30)),
        window: || Duration::from_secs(30),
        holds_at: || Some(Duration::from_secs(15)),
    },
    Row {
        case: "a span stated as text, longer than that ceiling",
        stack: "service-timeout-long",
        resource_type: "Custom::SlowProvisioner",
        declared: || Some(json!("3600")),
        window: || Duration::from_secs(3600),
        holds_at: || Some(CREATE_STABILISATION_WINDOW + ONE_INSTANT),
    },
    Row {
        case: "the documented minimum, which is a window too short to look inside of",
        stack: "service-timeout-minimum",
        resource_type: "Custom::BriefProvisioner",
        declared: || Some(json!(1)),
        window: || Duration::from_secs(1),
        holds_at: || None,
    },
    Row {
        case: "the documented maximum",
        stack: "service-timeout-maximum",
        resource_type: "AWS::CloudFormation::CustomResource",
        declared: || Some(json!(14_400)),
        window: || Duration::from_secs(14_400),
        holds_at: || Some(CREATE_STABILISATION_WINDOW + ONE_INSTANT),
    },
];

/// The template one row deploys: the custom resource alone, backed by the provider topic this
/// emulator really holds, with the row's own `ServiceTimeout` where it states one.
fn template_of(row: &Row) -> String {
    let mut properties = Map::new();
    properties.insert(
        custom_resource::SERVICE_TOKEN.to_string(),
        json!(provider_token()),
    );
    if let Some(declared) = (row.declared)() {
        properties.insert(custom_resource::SERVICE_TIMEOUT.to_string(), declared);
    }
    json!({
        "Resources": {
            CUSTOM: {
                "Type": row.resource_type,
                "Properties": Value::Object(properties),
            },
        },
    })
    .to_string()
}

/// The `StackStatus` the stack reports right now, read over CloudFormation's own wire — which
/// is also the read that makes a wait this service's clock has run out of due.
async fn reported_status(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let (http_status, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected DescribeStacks to answer, got {http_status} {body}"
    );
    xml_text(&body, "StackStatus").unwrap_or_default()
}

/// Poll the stack until it reports a status other than `expected`, and answer what it then
/// reported — or `expected` itself where every look of the wait reported that, which is what a
/// case asserting a wait still HOLDS reads.
///
/// Nothing is aged on the way, so the only thing that can end this wait is what the clock
/// already owes; the looks span far more than the span this service gives a status of its own
/// to become due, so neither answer turns on how fast the host runs.
async fn status_other_than(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    expected: &str,
) -> String {
    let mut last = String::new();
    for _ in 0..POLL_ATTEMPTS {
        last = reported_status(router, case, stack).await;
        if last != expected {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    last
}

/// Poll the stack until it leaves every `*_IN_PROGRESS` status, with every wait it holds for a
/// provider's answer moved `by` into the past on each look — the shape of an operation whose
/// provider answers nothing at all, so each of its passes runs its own window out.
async fn settled_with_a_silent_provider(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
    by: Duration,
) -> String {
    let mut last = String::new();
    for _ in 0..POLL_ATTEMPTS {
        last = reported_status(router, case, stack).await;
        if !last.is_empty() && !last.ends_with(IN_PROGRESS) {
            return last;
        }
        aged_provider_waits(engine, stack, by);
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: {stack:?} never left an in-progress status; last was {last:?}");
}

#[tokio::test]
async fn the_wait_for_a_providers_answer_runs_for_the_window_the_declaration_asked_for() {
    for row in ROWS {
        let case = row.case;
        let window = (row.window)();
        let (router, engine) = emulator(case).await;
        let (http_status, body) = create_stack(&router, row.stack, &template_of(row), &[]).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected CreateStack to succeed, got {http_status} {body}"
        );

        // The request IS with the provider — that is what the create is waiting for, and what
        // nothing is going to answer — and its `RequestId` is what the failure below names.
        let (_, request, _) = awaited_request(&engine, row.stack, CUSTOM).unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a provider request about {CUSTOM} of {:?}, got none",
                row.stack
            )
        });
        assert_eq!(
            request.request_type, CREATE_REQUEST,
            "case {case:?}: expected the create's own request to the provider, got {request:?}"
        );

        // Inside the window the declaration asked for, the resource is still HELD: this is the
        // span the provider has to answer in, and nothing about it has run out yet.
        let mut aged = Duration::ZERO;
        if let Some(holds_at) = (row.holds_at)() {
            assert!(
                holds_at < window,
                "case {case:?}: a look inside the window must be inside it, got {holds_at:?} of \
                 {window:?}"
            );
            assert_eq!(
                aged_provider_waits(&engine, row.stack, holds_at),
                1,
                "case {case:?}: expected the create to hold one wait for the provider's answer"
            );
            aged = holds_at;
            let reported =
                status_other_than(&router, case, row.stack, status::CREATE_IN_PROGRESS).await;
            assert_eq!(
                reported,
                status::CREATE_IN_PROGRESS,
                "case {case:?}: expected the create to still be waiting for the provider's \
                 answer {holds_at:?} into a {window:?} window, got {reported:?}"
            );
        }

        // Past it, the wait is over: the create is no longer waiting for an answer the window
        // the declaration asked for has closed on, with nothing else moved on — which is what
        // says the window in force is that one and not another.
        let past_the_window = window + ONE_INSTANT - aged;
        assert_eq!(
            aged_provider_waits(&engine, row.stack, past_the_window),
            1,
            "case {case:?}: expected the create to still hold one wait for the provider's answer"
        );
        let left = status_other_than(&router, case, row.stack, status::CREATE_IN_PROGRESS).await;
        assert_ne!(
            left,
            status::CREATE_IN_PROGRESS,
            "case {case:?}: expected the create to give up on the provider's answer once the \
             {window:?} window the declaration asked for had closed, got {left:?}"
        );

        // This provider answers nothing at all, so the rollback's own `Delete` goes unanswered
        // too and the stack rests in the failed rollback status — which is what says the stack
        // reached a TERMINAL status rather than holding for ever.
        let settled =
            settled_with_a_silent_provider(&router, &engine, case, row.stack, window + ONE_INSTANT)
                .await;
        assert_eq!(
            settled,
            status::ROLLBACK_FAILED,
            "case {case:?}: expected the window the declaration asked for to run out and the \
             create to roll back, got {settled:?}"
        );

        // ...and the resource's own event says WHY, in CloudFormation's own sentence for an
        // answer that never came, naming the request the provider owed it for.
        let reason = failed_resource_reason(
            case,
            &router,
            REGION,
            row.stack,
            CUSTOM,
            resource_status::CREATE_FAILED,
        )
        .await;
        assert_eq!(
            reason,
            custom_resource::unanswered_request_reason(&request.request_id),
            "case {case:?}: expected the failed create to report the sentence for an unanswered \
             request, got {reason:?}"
        );
    }
}
