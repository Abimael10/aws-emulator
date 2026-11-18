// SPDX-License-Identifier: CC0-1.0

//! `Enabled` as a DELIVERY GATE on an Amazon SQS event source mapping: what a paused mapping
//! does with the messages sent while it is paused, and what the same mapping does with them
//! once it is enabled again.
//!
//! The RULE pinned here: **"when true, the event source mapping is active. When false, Lambda
//! pauses polling and invocation" (`CreateEventSourceMapping`'s own `Enabled`, which
//! `UpdateEventSourceMapping` carries too) — so a mapping reporting `Disabled` takes nothing
//! out of its queue: a message sent meanwhile stays WAITING on the source queue (not in
//! flight, since no consumer took it) and no invocation is made for it; and the SAME mapping,
//! enabled again, polls that message and hands it to its function.** Pausing a consumer is a
//! control a client uses during maintenance, so both halves have to hold: the withhold and
//! the resumption.
//!
//! Every observation is one a client makes: the bodies the handler forwarded to a results
//! queue, the mapping's own `State`, and the source queue's own message counts. The waits are
//! on conditions — a state reported, a body forwarded — with a generous deadline, so a loaded
//! host delays a row instead of weakening it.
//!
//! The rows differ along HOW the mapping came to be paused, which is what the rule
//! generalises over: a mapping created with polling paused, and one created polling and
//! paused by an update after it had delivered.

use std::time::{Duration, Instant};

use serde_json::json;
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    await_mapping_state, call, create_mapping, create_queue, create_running_function,
    given_enabled_mapping, queue_message_counts, router_with_queues, send_message, taken_bodies,
    FORWARDING_HANDLER, LOOK_AGAIN, MAPPINGS_URI, RESULTS_QUEUE_URL, STATE_DISABLED,
    STATE_ENABLED,
};

/// How long an observation of work the emulator owes — a batch polled, a handler run, a
/// message forwarded — waits before it is a failure. Generous, and taken from what the
/// emulator allows ONE attempt rather than picked here: a row fails only when the work did
/// not happen at all.
const BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(Duration::from_secs(60));

/// The source queue's `VisibilityTimeout` in seconds, and the function's own `Timeout`: a
/// queue maps onto a function only while it outlasts that function's timeout.
const VISIBILITY_TIMEOUT: i64 = 30;
/// See [`VISIBILITY_TIMEOUT`].
const FUNCTION_TIMEOUT: i64 = 10;

/// The body sent while the mapping polls, and the one sent while it is paused.
const WARM_UP: &str = "delivered-while-enabled";
const WITHHELD: &str = "sent-while-paused";

/// Read the results queue until `body` has been forwarded, answering every body seen.
async fn awaited_forward(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    body: &str,
) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    let mut seen: Vec<String> = Vec::new();
    loop {
        seen.extend(taken_bodies(router, case, url).await);
        if seen.iter().any(|forwarded| forwarded == body) {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the handler to be handed {body:?}, it forwarded {seen:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `UpdateEventSourceMapping` setting `Enabled`, waited out to the state that setting asks
/// for.
async fn set_enabled(router: &MultiServiceRouter, case: &str, uuid: &str, enabled: bool) {
    let answered = call(
        router,
        "PUT",
        &format!("{MAPPINGS_URI}/{uuid}"),
        &json!({ "Enabled": enabled }),
    )
    .await;
    assert_eq!(
        answered.status,
        202,
        "case {case:?}: expected UpdateEventSourceMapping(Enabled={enabled}) to answer 202, got \
         {} {}",
        answered.status,
        answered.text()
    );
    let wanted = if enabled {
        STATE_ENABLED
    } else {
        STATE_DISABLED
    };
    await_mapping_state(router, case, uuid, wanted).await;
}

/// One row: how the mapping came to be paused.
struct Row {
    case: &'static str,
    /// Whether the mapping is created polling (and paused by an update afterwards) or created
    /// with polling already paused.
    created_enabled: bool,
}

/// RULE: a paused mapping takes nothing out of its queue — the message stays waiting there
/// and nothing is invoked for it — and the same mapping, enabled, hands that message to its
/// function.
#[tokio::test]
async fn a_paused_mapping_withholds_its_queue_and_delivers_it_once_enabled() {
    let rows = [
        Row {
            case: "paused by an update after it had delivered",
            created_enabled: true,
        },
        Row {
            case: "created with polling paused",
            created_enabled: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let suffix = if row.created_enabled { "updated" } else { "created" };
        let router = router_with_queues();
        let (results_url, _) =
            create_queue(&router, case, &format!("paused-results-{suffix}"), &json!({})).await;
        let (source_url, source_arn) = create_queue(
            &router,
            case,
            &format!("paused-source-{suffix}"),
            &json!({ "VisibilityTimeout": VISIBILITY_TIMEOUT.to_string() }),
        )
        .await;
        let function = format!("paused-fn-{suffix}");
        create_running_function(
            &router,
            case,
            &function,
            FORWARDING_HANDLER,
            FUNCTION_TIMEOUT,
            &json!({ RESULTS_QUEUE_URL: results_url }),
        )
        .await;

        let request = json!({
            "FunctionName": function,
            "EventSourceArn": source_arn,
            "BatchSize": 10,
            "Enabled": row.created_enabled,
        });
        let uuid = if row.created_enabled {
            let uuid = given_enabled_mapping(&router, case, &request).await;
            // The positive control: while it polls, a message sent to the source queue
            // reaches the handler, which forwards it.
            send_message(&router, case, &source_url, WARM_UP, None).await;
            let forwarded = awaited_forward(&router, case, &results_url, WARM_UP).await;
            assert_eq!(
                forwarded,
                vec![WARM_UP.to_string()],
                "case {case:?}: the polling mapping must hand the handler exactly the message \
                 it was sent, it forwarded {forwarded:?}"
            );
            set_enabled(&router, case, &uuid, false).await;
            uuid
        } else {
            let created = create_mapping(&router, &request).await;
            assert_eq!(
                created.status,
                202,
                "case {case:?}: expected CreateEventSourceMapping to answer 202, got {} {}",
                created.status,
                created.text()
            );
            let uuid = created
                .string("UUID")
                .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", created.text()));
            await_mapping_state(&router, case, &uuid, STATE_DISABLED).await;
            uuid
        };

        // Paused: the message sent now is still WAITING on the source queue — no consumer
        // took it, so it is neither in flight nor gone — and the handler was handed nothing.
        send_message(&router, case, &source_url, WITHHELD, None).await;
        let counts = queue_message_counts(&router, case, &source_url).await;
        assert_eq!(
            counts,
            (1, 0, 0),
            "case {case:?}: a paused mapping polls nothing, so the message must be waiting on \
             the source queue, got {counts:?} as (visible, in flight, delayed)"
        );
        let quiet = taken_bodies(&router, case, &results_url).await;
        assert!(
            quiet.is_empty(),
            "case {case:?}: a paused mapping invokes nothing, so the handler can forward \
             nothing, but it forwarded {quiet:?}"
        );

        // Enabled again: the same mapping polls the message it was withholding and hands it
        // over.
        set_enabled(&router, case, &uuid, true).await;
        let delivered = awaited_forward(&router, case, &results_url, WITHHELD).await;
        assert_eq!(
            delivered,
            vec![WITHHELD.to_string()],
            "case {case:?}: the mapping enabled again must hand the handler the withheld \
             message and nothing else, it forwarded {delivered:?}"
        );
    }
}
