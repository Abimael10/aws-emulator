// SPDX-License-Identifier: CC0-1.0

//! Bringing the wait before a RETRIED asynchronous Lambda invocation FORWARD, which the suites
//! of this target that drive a whole ladder of attempts share.
//!
//! An asynchronous invocation whose handler errors is returned to Lambda's queue with its next
//! attempt due the wait AWS publishes for it — one minute before the first retry, two minutes
//! before the second — and that instant lives on the queue entry the accepting call made. So a
//! suite that wants the ladder rather than the wait moves the instant, exactly as a suite that
//! wants a function `Active` moves its activation instant: the move is what waiting would do,
//! without waiting, and every other part of the delivery is the emulator's own.
//!
//! No test here asserts the wait itself; the suite that does is the owning crate's
//! (`services/service-com-amazonaws-lambda-awsgirapiservice/tests/all/async_retry_backoff_interval.rs`).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState;

/// Bring the next delivery attempt of every accepted asynchronous Lambda event on `engine`
/// forward to now, and answer whether any was still waiting for one.
pub(crate) fn make_next_attempt_due(engine: &Arc<engine::InMemoryEngine>) -> bool {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let now = SystemTime::now();
    let past = now - Duration::from_secs(1);
    let mut moved = false;
    for function in store.functions.values_mut() {
        for entry in &mut function.async_invocations {
            if entry.attempt_due > now {
                entry.attempt_due = past;
                moved = true;
            }
        }
    }
    moved
}
