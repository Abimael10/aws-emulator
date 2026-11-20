// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SPACE a function's ephemeral storage hands out: `CreateFunction`,
//! `GetFunctionConfiguration` and `Invoke`
//! (`POST /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: the ephemeral storage hands space out in whole BLOCKS, so a file takes
//! up the blocks its bytes need, and what a filesystem reading of `/tmp` reports as available
//! is exactly the space a write may still take — a growth to exactly that many bytes is
//! admitted, and one byte past it is refused as a full device refuses one, whichever call asks
//! for it.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what the
//! rule generalises over: the configured size of the storage, the LENGTH written (nothing, one
//! byte, one byte short of a block, a whole block, one byte past a block, a megabyte, and a
//! megabyte and a byte), the CALL the growth is asked for with (truncating by path, truncating
//! by descriptor, reserving space on a descriptor, writing past the end of a file, writing at a
//! position), and which side of the boundary the ask falls on.
//!
//! The expected occupancy of every row is computed from the block size the filesystem itself
//! reports (`os.statvfs(...).f_frsize`), so the assertion is the rule rather than one host's
//! numbers.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};
// The one multi-entry deployment-archive writer this target has.
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A SigV4 credential scope naming this service's signing name (`lambda`) in `us-east-1`.
/// Nothing in it is verified: the harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The `Runtime` every function of this suite is configured with.
const RUNTIME: &str = "python3.12";

/// The `Timeout` every function this suite creates carries, in seconds. The handler's own work
/// is accounting and one fill of the storage, so this is sized to cover this test binary's own
/// scheduling on a loaded host rather than the work.
const FUNCTION_TIMEOUT_SECONDS: i32 = 120;

/// How long a poll waits for a created function to report the `Active` state the provisioning
/// window owes it. Generous on purpose: the loop ends on the CONDITION, and this is only the
/// deadline past which the emulator is considered broken.
const ACTIVE_DEADLINE: Duration = Duration::from_secs(60);

/// How long one look at a provisioning function waits before the next; the loop ends on the
/// condition, never on this.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How many times a row re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 4;

/// The sizes a row configures the storage with, in MB: the documented default and one above it
/// (`EphemeralStorage$Size`: "The default value is 512, but can be any whole number between 512
/// and 10,240 MB"). Both are sizes this host holds in full
/// (`state::execution::MAX_EPHEMERAL_HELD_BYTES`), so on each of them what a reading reports as
/// available is also what may really be written.
const SIZES_MB: &[i64] = &[512, 1024];

/// The lengths one file is written at, in bytes: nothing, one byte, one short of a block, a
/// whole block, one past a block, a megabyte, and a megabyte and a byte. The block size itself
/// is read off the filesystem, so these are stated against the block size the storage reports.
const LENGTHS: &[u64] = &[0, 1, 4_095, 4_096, 4_097, 1_048_576, 1_048_577];

/// What a dispatched request answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// One service mounted on its own engine, reached the way the production router reaches it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// Dispatch one request, with the extra headers `headers` names.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
    headers: &[(&str, &str)],
) -> Answer {
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.us-east-1.amazonaws.com{uri}"))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let request = request
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// Create the probe function with `ephemeral_mb` of ephemeral storage, and wait for the
/// `Active` state the create's provisioning window owes it by POLLING for that state, so no
/// row waits a fixed span.
async fn create_function(router: &MultiServiceRouter, name: &str, ephemeral_mb: i64) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": "index.handler",
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "MemorySize": 256,
            "EphemeralStorage": { "Size": ephemeral_mb },
            "Code": { "ZipFile": base64::engine::general_purpose::STANDARD
                .encode(package(&[("index.py", PROBE_SOURCE)])) },
        }))
        .expect("the create body serializes"),
        &[],
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    poll_until_active(router, name).await;
}

/// Look at `name`'s configuration until it reports `Active`, or fail once [`ACTIVE_DEADLINE`]
/// has passed. Waits for the CONDITION rather than for a span, so a loaded or a fast host only
/// changes how often it looks.
async fn poll_until_active(router: &MultiServiceRouter, name: &str) {
    let deadline = Instant::now() + ACTIVE_DEADLINE;
    loop {
        let read = call(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &[],
            &[],
        )
        .await;
        if read.json()["State"] == json!("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "function {name:?} never reached Active within {ACTIVE_DEADLINE:?}, last read {} {}",
            read.status,
            read.text()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What one case answered, or `None` when the run was ended by a bound this whole test binary
/// shares rather than by anything the case asked for.
async fn answered(
    router: &MultiServiceRouter,
    name: &str,
    case: &str,
    event: &Value,
) -> Option<Value> {
    let uri = format!("{FUNCTIONS_URI}/{name}/invocations");
    let body = serde_json::to_vec(event).expect("the event serializes");
    let mut answer = call(router, "POST", &uri, &body, &[]).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answer.status, &answer.text()) {
            break;
        }
        answer = call(router, "POST", &uri, &body, &[]).await;
    }
    if is_host_bound(answer.status, &answer.text()) {
        return None;
    }
    assert_eq!(
        answer.status,
        200,
        "case {case}: expected 200, got {} {}",
        answer.status,
        answer.text()
    );
    let failure = answer.header("x-amz-function-error");
    assert_eq!(
        failure, None,
        "case {case}: the handler reads and writes its own ephemeral storage and catches every \
         refusal itself, so the invocation carries no function error, got {failure:?} with {}",
        answer.text()
    );
    Some(answer.json())
}

/// One member of `document` as a number, or a panic naming what the run reported instead.
fn number(document: &Value, member: &str, case: &str) -> u64 {
    document[member]
        .as_u64()
        .unwrap_or_else(|| panic!("case {case}: expected a number at {member:?}, got {document}"))
}

/// How much of a storage whose blocks are `block` bytes a file of `length` bytes takes up: the
/// whole blocks its bytes need.
fn occupies(length: u64, block: u64) -> u64 {
    length.div_ceil(block) * block
}

/// The whole rule, driven END TO END as one test: the tables below are real `CreateFunction`,
/// `GetFunctionConfiguration` and `Invoke` requests over the router.
///
/// One test and one stream of invocations, the way the sibling runtime suites are built. A case
/// whose run was stopped by a bound this whole binary shares says nothing about the rule and is
/// left out of the reading — but not every one can be: the tables answer how many of their
/// cases a RUN answered, and a test that read none at all is a test that asserted nothing, so
/// it fails rather than passing silently.
#[tokio::test]
async fn the_space_the_ephemeral_storage_reports_free_is_the_space_a_write_may_take() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let mut read = 0;
    for size_mb in SIZES_MB {
        let probe = format!("probe-ephemeral-space-{size_mb}");
        create_function(&router, &probe, *size_mb).await;
        read += a_file_takes_up_whole_blocks(&router, &probe, *size_mb).await;
        read +=
            a_growth_past_what_is_free_is_refused_in_every_form(&router, &probe, *size_mb).await;
    }
    // The storage filled to exactly what it reports free: driven on the SMALLEST storage a
    // function may carry, because what the row costs this host is that size and the arithmetic
    // it reads is the same on every one of them.
    read += the_whole_of_what_is_free_may_be_written(&router, SIZES_MB[0]).await;
    assert!(
        read > 0,
        "expected at least one case of the tables above to be answered by a run, got {read}: \
         every invocation reported a bound this whole test binary shares, so nothing of the \
         rule was read"
    );
}

/// A file of any length takes up the whole BLOCKS its bytes need — reported by the filesystem,
/// taken off what the storage has free, and given back when the file goes — while the length a
/// reading reports for it is its bytes.
///
/// Answers how many cases a run answered.
async fn a_file_takes_up_whole_blocks(
    router: &MultiServiceRouter,
    probe: &str,
    size_mb: i64,
) -> usize {
    let case = format!("a file of each length on a {size_mb} MB storage");
    let Some(document) = answered(
        router,
        probe,
        &case,
        &json!({"case": "occupies", "lengths": LENGTHS}),
    )
    .await
    else {
        return 0;
    };
    let block = number(&document, "block_size", &case);
    let total = number(&document, "total", &case);
    assert_eq!(
        total,
        u64::try_from(size_mb).expect("a configured size in MB") * 1024 * 1024,
        "case {case}: expected the configured size to be the size of /tmp, got {document}"
    );
    let rows = document["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected the rows of the table, got {document}"));
    assert_eq!(
        rows.len(),
        LENGTHS.len(),
        "case {case}: expected one row per length, got {document}"
    );
    for (length, row) in LENGTHS.iter().zip(rows) {
        let row_case = format!("{case}, a file of {length} bytes");
        let before = number(row, "free_before", &row_case);
        let after = number(row, "free_after", &row_case);
        let taken = before - after;
        // The file's own record is an entry of the filesystem and costs one block beside the
        // blocks of its content.
        let expected = occupies(*length, block) + block;
        assert_eq!(
            taken, expected,
            "case {row_case}: expected the file to take up {expected} bytes (the whole blocks \
             of its content, plus the block its record costs), got {taken}: {row}"
        );
        // What a reading says about the file: its LENGTH is its bytes, while the space
        // allocated for it is those whole blocks.
        assert_eq!(
            number(row, "size", &row_case),
            *length,
            "case {row_case}: expected the file to be {length} bytes long, got {row}"
        );
        assert_eq!(
            number(row, "allocated", &row_case),
            occupies(*length, block),
            "case {row_case}: expected the blocks allocated for the file to be the whole blocks \
             its bytes need, got {row}"
        );
        // Every reading of what is free is a whole number of blocks, which is what makes the
        // reported figure exact rather than rounded down to one.
        for member in ["free_before", "free_after", "free_after_removing"] {
            let free = number(row, member, &row_case);
            assert_eq!(
                free % block,
                0,
                "case {row_case}: expected {member} to be a whole number of {block}-byte \
                 blocks, got {free}: {row}"
            );
        }
        // And removing the file gives back everything it held, its record included.
        assert_eq!(
            number(row, "free_after_removing", &row_case),
            before,
            "case {row_case}: expected removing the file to give back what it held, got {row}"
        );
    }
    1
}

/// A growth one byte past what the storage reports free is refused the way a full device
/// refuses one — whichever call asks for it — and nothing of it is written: the file is the
/// length it was and the storage has as much free as it had.
///
/// Answers how many cases a run answered.
async fn a_growth_past_what_is_free_is_refused_in_every_form(
    router: &MultiServiceRouter,
    probe: &str,
    size_mb: i64,
) -> usize {
    // The calls a program grows a file with, each asked for exactly one byte past what the
    // storage reports free. A single write of that many bytes is left out: the bytes would have
    // to be built in the handler first, and the sibling suite drives a fill in chunks instead.
    let forms: &[&str] = &[
        "truncate",
        "ftruncate",
        "fallocate",
        "write_past_the_end",
        "positioned_write",
    ];
    let case = format!("a growth past what a {size_mb} MB storage has free");
    let Some(document) = answered(
        router,
        probe,
        &case,
        &json!({"case": "boundary", "forms": forms}),
    )
    .await
    else {
        return 0;
    };
    let rows = document["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected the rows of the table, got {document}"));
    assert_eq!(
        rows.len(),
        forms.len(),
        "case {case}: expected one row per form of a growth, got {document}"
    );
    for (form, row) in forms.iter().zip(rows) {
        let row_case = format!("{case}, asked for by {form}");
        let path = row["path"].as_str().unwrap_or_default();
        assert_eq!(
            row["refusal"],
            json!(format!("[Errno 28] No space left on device: '{path}'")),
            "case {row_case}: expected the growth to be refused as a full device refuses one, \
             got {row}"
        );
        assert_eq!(
            number(row, "size_after", &row_case),
            number(row, "size_before", &row_case),
            "case {row_case}: expected the refused growth to leave the file the length it was, \
             got {row}"
        );
        assert_eq!(
            number(row, "free_after", &row_case),
            number(row, "free_before", &row_case),
            "case {row_case}: expected the refused growth to leave the storage as free as it \
             was, got {row}"
        );
    }
    1
}

/// The other side of that boundary: a file may be grown to exactly what the storage reports
/// free, it then holds those bytes, the storage reports nothing free, one more byte is refused,
/// and removing the file gives the whole storage back.
///
/// Answers how many cases a run answered.
async fn the_whole_of_what_is_free_may_be_written(
    router: &MultiServiceRouter,
    size_mb: i64,
) -> usize {
    let probe = format!("probe-ephemeral-space-whole-{size_mb}");
    create_function(router, &probe, size_mb).await;
    let case = format!("the whole of a {size_mb} MB storage");
    let Some(document) = answered(router, &probe, &case, &json!({"case": "whole"})).await else {
        return 0;
    };
    let asked = number(&document, "asked", &case);
    let path = document["path"].as_str().unwrap_or_default();
    assert_eq!(
        json!({
            // Grown to exactly what the reading reported free: admitted, and the file is that
            // long.
            "grew": document["grew"].clone(),
            "size": document["size"].clone(),
            "free_when_full": document["free_when_full"].clone(),
            // What it reads back there is what a grown file reads as.
            "tail": document["tail"].clone(),
            // And the next byte, asked for either way, is refused.
            "one_more_by_truncate": document["one_more_by_truncate"].clone(),
            "one_more_by_write": document["one_more_by_write"].clone(),
            // Removing it gives the whole storage back.
            "free_after_removing": document["free_after_removing"].clone(),
        }),
        json!({
            "grew": "",
            "size": asked,
            "free_when_full": 0,
            "tail": "0000000000000000",
            "one_more_by_truncate": format!("[Errno 28] No space left on device: '{path}'"),
            "one_more_by_write": format!("[Errno 28] No space left on device: '{path}'"),
            "free_after_removing": document["total"].clone(),
        }),
        "case {case}: expected the whole of what the storage reports free to be writable and \
         nothing past it, got {document}"
    );
    1
}

/// The probe this suite drives: its handler accounts for the space its own ephemeral storage
/// hands out, and reports what each call answered.
const PROBE_SOURCE: &str = r#"
import os

_TMP = "/tmp"


def _message(call):
    """The MESSAGE of the error `call` raised, or an empty string when it raised none."""
    try:
        call()
    except BaseException as error:
        return str(error)
    return ""


def _free():
    held = os.statvfs(_TMP)
    return held.f_bavail * held.f_frsize


def _occupies(event):
    """What a file of each length the event names takes off the storage."""
    held = os.statvfs(_TMP)
    path = _TMP + "/occupies"
    rows = []
    for length in event["lengths"]:
        wanted = int(length)
        before = _free()
        with open(path, "wb") as writing:
            writing.write(bytes(wanted))
        row = {
            "free_before": before,
            "free_after": _free(),
            "size": os.path.getsize(path),
            "allocated": os.stat(path).st_blocks * 512,
        }
        os.remove(path)
        row["free_after_removing"] = _free()
        rows.append(row)
    return {
        "block_size": held.f_frsize,
        "total": held.f_blocks * held.f_frsize,
        "rows": rows,
    }


def _boundary(event):
    """What each form of a growth ONE BYTE past what the storage reports free answers.

    The file is empty before each form, so the blocks its content needs are the blocks of the
    growth itself and what the reading reports free is what that growth may take.
    """
    path = _TMP + "/boundary"
    rows = []
    for form in event["forms"]:
        with open(path, "wb"):
            pass
        free = _free()
        past = free + 1

        def _truncate():
            os.truncate(path, past)

        def _ftruncate():
            descriptor = os.open(path, os.O_RDWR)
            try:
                os.ftruncate(descriptor, past)
            finally:
                os.close(descriptor)

        def _fallocate():
            descriptor = os.open(path, os.O_RDWR)
            try:
                os.posix_fallocate(descriptor, free, 1)
            finally:
                os.close(descriptor)

        def _write_past_the_end():
            with open(path, "r+b") as writing:
                writing.seek(past)
                writing.write(b"x")

        def _positioned_write():
            descriptor = os.open(path, os.O_RDWR)
            try:
                os.pwrite(descriptor, b"x", free)
            finally:
                os.close(descriptor)

        asked = {
            "truncate": _truncate,
            "ftruncate": _ftruncate,
            "fallocate": _fallocate,
            "write_past_the_end": _write_past_the_end,
            "positioned_write": _positioned_write,
        }[form]
        rows.append({
            "path": path,
            "size_before": os.path.getsize(path),
            "free_before": free,
            "refusal": _message(asked),
            "size_after": os.path.getsize(path),
            "free_after": _free(),
        })
        os.remove(path)
    return {"rows": rows}


def _whole(event):
    """Grow one file to exactly what the storage reports free, and report what it came to."""
    held = os.statvfs(_TMP)
    path = _TMP + "/whole"
    with open(path, "wb"):
        pass
    free = _free()
    answer = {
        "path": path,
        "total": held.f_blocks * held.f_frsize,
        "asked": free,
        "grew": _message(lambda: os.truncate(path, free)),
    }
    answer["size"] = os.path.getsize(path)
    answer["free_when_full"] = _free()
    with open(path, "rb") as reading:
        reading.seek(max(0, free - 8))
        answer["tail"] = reading.read().hex()
    answer["one_more_by_truncate"] = _message(lambda: os.truncate(path, free + 1))

    def _one_more_by_write():
        with open(path, "r+b") as writing:
            writing.seek(free)
            writing.write(b"x")

    answer["one_more_by_write"] = _message(_one_more_by_write)
    os.remove(path)
    answer["free_after_removing"] = _free()
    return answer


_CASES = {
    "occupies": _occupies,
    "boundary": _boundary,
    "whole": _whole,
}


def handler(event, context):
    return _CASES[event["case"]](event)
"#;
