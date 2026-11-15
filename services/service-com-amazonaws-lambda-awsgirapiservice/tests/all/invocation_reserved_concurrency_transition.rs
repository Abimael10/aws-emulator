// SPDX-License-Identifier: CC0-1.0

//! What a change to a function's RESERVED concurrency does to the very next invocation,
//! driven over the wire with nothing in flight beside it.
//!
//! RULE: the reservation `PutFunctionConcurrency` stores is the function-level bound every
//! later invocation is admitted against from the instant that call stored it — "Sets the
//! maximum number of simultaneous executions for a function, and reserves capacity for that
//! concurrency level … and prevents it from scaling beyond that level", applying "to the
//! function as a whole, including all published versions and the unpublished version"
//! (`PutFunctionConcurrency`). So an invocation of a function whose reservation is ZERO is
//! refused — "Lambda returns `TooManyRequestsException` if running the function would cause
//! you to exceed a concurrency limit at either the account level
//! (`ConcurrentInvocationLimitExceeded`) or function level
//! (`ReservedFunctionConcurrentInvocationLimitExceeded`)" (`Invoke`) — and a reservation
//! RAISED above what the function is processing admits invocations again, without waiting for
//! anything: the admission reads the reservation in force, not the one that was in force when
//! the function was created.
//!
//! The rows differ along what the rule generalises over: the value written (zero, one, a
//! larger reservation, and the reservation removed altogether), the DIRECTION of the change
//! (the first write of a reservation, a raise, a lowering back to zero, a removal, and a
//! reservation put again after a removal), and the qualifier the invocation that follows it
//! addresses — because one reservation is the pool of every qualifier of the function alike.
//!
//! Each row writes the reservation, reads it back through `GetFunctionConcurrency` so what
//! the invocation is decided against is what the service really holds, and then invokes: a
//! row whose change was answered but not honoured by the data plane fails on the invocation.
//! Nothing here sleeps for a transition — the reservation takes effect at the instant it is
//! stored, and the only wait is the poll for the function to report `Active`.
//!
//! The suite RUNS handlers, so it takes this target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::alias_update::{call, router_on, Answer, FUNCTIONS_URI};
use crate::event_source_mappings::await_function_active;
use crate::function_concurrency::{get_concurrency, put_concurrency};
use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::synchronous_invoke_run::package;

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The code, status and sentence of the modelled throttle an invocation the reservation has
/// no room for is refused with.
const THROTTLE_CODE: &str = "TooManyRequestsException";
/// See [`THROTTLE_CODE`].
const THROTTLE_STATUS: u16 = 429;
/// See [`THROTTLE_CODE`].
const THROTTLE_MESSAGE: &str = "Rate Exceeded.";

/// The `Reason` that throttle names when the capacity that was full is the FUNCTION's own
/// reservation, one of the model's `ThrottleReason` values
/// (`smithy_query(query="shape", shape="ThrottleReason")`).
const RESERVED_REASON: &str = "ReservedFunctionConcurrentInvocationLimitExceeded";

/// The handler every invocation below runs: it answers at once and holds nothing, so what
/// decides each row is the reservation in force and not what another invocation is doing.
const ANSWERS: &str = "def lambda_handler(event, context):\n    return {'ran': event['row']}\n";

/// Create the function, publish version 1, point an alias at it, and wait until Lambda
/// reports it `Active` — the state an invocation needs.
async fn given_function(router: &MultiServiceRouter, name: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Publish": true,
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(ANSWERS)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup: CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    let alias = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/aliases"),
        &json!({ "Name": "live", "FunctionVersion": "1" }),
    )
    .await;
    assert_eq!(
        alias.status,
        201,
        "setup: CreateAlias on {name:?} must succeed, got {} {}",
        alias.status,
        alias.text()
    );
    await_function_active(router, "the function the rows reserve for", name).await;
}

/// Write the reservation `reserved` names — a value, or its removal — and read back what the
/// service holds for it afterwards.
async fn write_reservation(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    reserved: Option<i64>,
) {
    match reserved {
        Some(value) => {
            let stored = put_concurrency(router, name, value).await;
            assert_eq!(
                stored.status,
                200,
                "case {case:?}: PutFunctionConcurrency({value}) must succeed, got {} {}",
                stored.status,
                stored.text()
            );
            let read = get_concurrency(router, name).await;
            assert_eq!(
                read.number("/ReservedConcurrentExecutions"),
                Some(value),
                "case {case:?}: GetFunctionConcurrency must report the reservation just \
                 written, got {}",
                read.text()
            );
        }
        None => {
            let removed = call(
                router,
                "DELETE",
                &format!("/2017-10-31/functions/{name}/concurrency"),
                &Value::Null,
            )
            .await;
            assert_eq!(
                removed.status,
                204,
                "case {case:?}: DeleteFunctionConcurrency must succeed, got {} {}",
                removed.status,
                removed.text()
            );
            let read = get_concurrency(router, name).await;
            assert_eq!(
                read.number("/ReservedConcurrentExecutions"),
                None,
                "case {case:?}: after the removal the function must report no reservation, \
                 got {}",
                read.text()
            );
        }
    }
}

/// One `Invoke` of `reference`, carrying the row's own number so an admitted invocation's
/// answer is its own.
async fn invoke(router: &MultiServiceRouter, reference: &str, row: usize) -> Answer {
    call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{reference}/invocations"),
        &json!({ "row": row }),
    )
    .await
}

/// One row: the reservation written, the qualifier the invocation that follows addresses, and
/// whether that invocation must be admitted.
struct Row {
    case: &'static str,
    /// The value `PutFunctionConcurrency` writes, or `None` for `DeleteFunctionConcurrency`.
    reserved: Option<i64>,
    /// How the invocation names the function: `None` is the unpublished version, `Some` a
    /// qualifier appended to the name.
    qualifier: Option<&'static str>,
    admitted: bool,
}

/// The rows, walked in order on ONE function: each change is made against whatever the
/// previous row left, so the sequence is what makes the transitions observable.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a reservation of zero, written first",
            reserved: Some(0),
            qualifier: None,
            admitted: false,
        },
        Row {
            case: "the reservation raised from zero to one",
            reserved: Some(1),
            qualifier: None,
            admitted: true,
        },
        Row {
            case: "the reservation raised again, addressed at a published version",
            reserved: Some(5),
            qualifier: Some("1"),
            admitted: true,
        },
        Row {
            case: "the reservation lowered back to zero, addressed at an alias",
            reserved: Some(0),
            qualifier: Some("live"),
            admitted: false,
        },
        Row {
            case: "the reservation raised from zero a second time",
            reserved: Some(2),
            qualifier: None,
            admitted: true,
        },
        Row {
            case: "the reservation removed altogether",
            reserved: None,
            qualifier: None,
            admitted: true,
        },
        Row {
            case: "a reservation of zero written again after the removal",
            reserved: Some(0),
            qualifier: Some("live"),
            admitted: false,
        },
    ]
}

/// A reservation decides the very next invocation: zero admits none of them, and raising it
/// admits them again — each change in force from the instant it was stored.
#[tokio::test]
async fn a_written_reservation_decides_the_next_invocation() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "probe-reservation-transition";
    given_function(&router, name).await;

    for (index, row) in rows().iter().enumerate() {
        write_reservation(&router, row.case, name, row.reserved).await;
        let reference = match row.qualifier {
            None => name.to_string(),
            Some(qualifier) => format!("{name}:{qualifier}"),
        };
        let answered = invoke(&router, &reference, index).await;
        if row.admitted {
            assert_eq!(
                answered.status,
                200,
                "case {:?}: the reservation in force has room, so the invocation must be \
                 served, got {} {}",
                row.case,
                answered.status,
                answered.text()
            );
            assert_eq!(
                answered.json(),
                json!({ "ran": index }),
                "case {:?}: expected this invocation's OWN run's value, got {}",
                row.case,
                answered.text()
            );
            assert!(
                !answered.headers.contains_key("x-amz-function-error"),
                "case {:?}: expected the handler's own result, got a function error with {}",
                row.case,
                answered.text()
            );
            continue;
        }
        assert_eq!(
            answered.status,
            THROTTLE_STATUS,
            "case {:?}: a reservation of zero admits nothing, so the invocation must be \
             refused with HTTP {THROTTLE_STATUS}, got {} {}",
            row.case,
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.error_code(),
            THROTTLE_CODE,
            "case {:?}: expected {THROTTLE_CODE}, got {}",
            row.case,
            answered.text()
        );
        assert_eq!(
            answered.message(),
            THROTTLE_MESSAGE,
            "case {:?}: expected the throttle's own sentence, got {}",
            row.case,
            answered.text()
        );
        assert_eq!(
            answered.json().get("Reason").and_then(Value::as_str),
            Some(RESERVED_REASON),
            "case {:?}: expected the function-level reason, got {}",
            row.case,
            answered.text()
        );
    }
}
