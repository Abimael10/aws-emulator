// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the EPHEMERAL STORAGE of a function's execution environment:
//! `CreateFunction`, `UpdateFunctionConfiguration`, `GetFunctionConfiguration` and `Invoke`
//! (`POST /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: a function's `EphemeralStorage.Size` ("the size of the function's /tmp
//! directory in MB") is the size of a WRITABLE `/tmp` its invocations have �� a filesystem
//! reading of that directory reports exactly that size, what has been written to it is really
//! held there and comes back byte for byte, a write past what it has left is refused as a full
//! device refuses one, and the directory belongs to the execution environment: it survives
//! between invocations served in one environment and a fresh environment's is empty.
//! Everything else the environment holds stays read-only.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what the
//! rule generalises over: the configured size (the documented default, one above it and the
//! documented maximum — `EphemeralStorage$Size`: "The default value is 512, but can be any
//! whole number between 512 and 10,240 MB"), whether the storage is read or written, the form
//! of the write (text, bytes, appended, through a descriptor, through `tempfile`), how deep in
//! the directory it is, which call reads it back, which condition a refusal is worded for, and
//! whether the environment serving the invocation is a fresh one or the one before it.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};
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

/// How long past a due instant this suite waits before calling. The transition is owed by the
/// CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` every function this suite creates carries, in seconds. Sized the way the
/// sibling suites that RUN handlers size theirs: it has to cover this test binary's own
/// scheduling, not the handler's work, which is immediate.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// How many times a row re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 4;

/// How many bytes are in one MB of a configured size, which is the unit
/// `EphemeralStorage$Size` is stated in.
const BYTES_PER_MB: u64 = 1024 * 1024;

/// The sizes a row configures, in MB: the documented default, one above it, and the documented
/// maximum (`EphemeralStorage$Size`: "The default value is 512, but can be any whole number
/// between 512 and 10,240 MB").
const SIZES_MB: &[i32] = &[512, 1024, 10_240];

/// How wide one write of the FILL row is, in MB: wide enough that the row drives few writes,
/// and narrow enough that the refusal it is about falls inside the smallest storage a function
/// may carry.
const CHUNK_MB: u64 = 32;

/// How much of the storage a row allows for beside the bytes it wrote: the RECORD of one entry
/// — a filesystem keeps an inode and a directory entry for every name it holds — and the one
/// block a filesystem reading rounds what is free down to. One block each, plus a name no
/// longer than the paths these rows write to.
const ENTRY_ALLOWANCE: u64 = 4096 + 4096 + 64;

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

/// The base64 an archive holding `entries` travels as, which is what a `ZipFile` member
/// carries.
fn zip_file(entries: &[(&str, &str)]) -> String {
    base64::engine::general_purpose::STANDARD.encode(package(entries))
}

/// Create the probe function with `ephemeral_mb` of ephemeral storage, and wait out the
/// provisioning window the create documents.
async fn create_function(router: &MultiServiceRouter, name: &str, ephemeral_mb: i32) {
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
            "Code": { "ZipFile": zip_file(&[("index.py", PROBE_SOURCE), ("notes.txt", NOTES)]) },
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
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// The `EphemeralStorage.Size` the stored configuration reports for `name`.
async fn reported_size(router: &MultiServiceRouter, name: &str) -> i64 {
    let read = call(
        router,
        "GET",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        &[],
        &[],
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "GetFunctionConfiguration {name:?} must succeed, got {} {}",
        read.status,
        read.text()
    );
    read.json()["EphemeralStorage"]["Size"]
        .as_i64()
        .unwrap_or_else(|| panic!("the stored EphemeralStorage.Size of {name:?}: {}", read.text()))
}

/// Resize the function's ephemeral storage, and wait out the update window.
async fn resize(router: &MultiServiceRouter, name: &str, ephemeral_mb: i32) {
    let updated = call(
        router,
        "PUT",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        &serde_json::to_vec(&json!({ "EphemeralStorage": { "Size": ephemeral_mb } }))
            .expect("the update body serializes"),
        &[],
    )
    .await;
    assert_eq!(
        updated.status,
        200,
        "UpdateFunctionConfiguration {name:?} must succeed, got {} {}",
        updated.status,
        updated.text()
    );
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// One `Invoke` request on the function `name` names, asking for the log tail.
async fn invoke_once(router: &MultiServiceRouter, name: &str, event: &Value) -> Answer {
    call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &serde_json::to_vec(event).expect("the event serializes"),
        &[("x-amz-log-type", "Tail")],
    )
    .await
}

/// What one case answered, with the log tail of the invocation that answered it, or `None`
/// when the run was ended by a bound this whole test binary shares rather than by anything the
/// case asked for.
async fn answered(
    router: &MultiServiceRouter,
    name: &str,
    case: &str,
    event: &Value,
) -> Option<(Value, String)> {
    let mut answer = invoke_once(router, name, event).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answer.status, &answer.text()) {
            break;
        }
        answer = invoke_once(router, name, event).await;
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
    let log = answer
        .header("x-amz-log-result")
        .and_then(|tail| base64::engine::general_purpose::STANDARD.decode(tail).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default();
    Some((answer.json(), log))
}

/// The whole rule, driven END TO END as one test: the tables below are real `CreateFunction`,
/// `UpdateFunctionConfiguration`, `GetFunctionConfiguration` and `Invoke` requests over the
/// router.
///
/// One test and one stream of invocations, the way the sibling runtime suites are built. A case
/// whose run was stopped by a bound this whole binary shares says nothing about the rule and is
/// left out of the reading — but not every one can be: the tables answer how many of their
/// cases a RUN answered, and a test that read none at all is a test that asserted nothing, so
/// it fails rather than passing silently.
#[tokio::test]
async fn ephemeral_storage_is_a_writable_tmp_of_the_size_the_function_configured() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let mut read = the_configured_size_is_the_size_of_tmp(&router).await;
    read += what_is_written_to_tmp_is_held_there(&router).await;
    read += the_size_is_enforced_and_not_only_reported(&router).await;
    read += a_fill_completes_while_it_fits_and_is_refused_where_it_does_not(&router).await;
    read += the_entries_of_the_storage_cost_it_what_a_filesystem_charges(&router).await;
    read += a_write_outside_tmp_is_still_refused(&router).await;
    read += the_calls_a_program_makes_on_an_open_file_are_answered(&router).await;
    read += the_output_descriptors_reach_the_invocation_log(&router).await;
    read += tmp_belongs_to_the_execution_environment(&router).await;
    assert!(
        read > 0,
        "expected at least one case of the tables above to be answered by a run, got {read}: \
         every invocation reported a bound this whole test binary shares, so nothing of the \
         rule was read"
    );
}

/// A filesystem reading of `/tmp` reports the size the function is configured for, for every
/// size a function may carry — and the stored configuration reports the same number.
///
/// Answers how many cases a run answered.
async fn the_configured_size_is_the_size_of_tmp(router: &MultiServiceRouter) -> usize {
    let mut read = 0;
    for size_mb in SIZES_MB {
        let case = format!("a function configured for {size_mb} MB");
        let probe = format!("probe-ephemeral-{size_mb}");
        create_function(router, &probe, *size_mb).await;
        let stored = reported_size(router, &probe).await;
        assert_eq!(
            stored,
            i64::from(*size_mb),
            "case {case}: the stored configuration must report the configured size, got {stored}"
        );
        let Some((document, _)) = answered(router, &probe, &case, &json!({"case": "report"})).await
        else {
            continue;
        };
        let total = u64::try_from(*size_mb).expect("a configured size in MB") * BYTES_PER_MB;
        assert_eq!(
            document,
            json!({
                "total": total,
                "free": total,
                "block_size": 4096,
                "is_directory": true,
                "writable": true,
                "entries": [],
                "read_only_flag": 0,
            }),
            "case {case}: expected a writable, empty /tmp of {total} bytes, got {document}"
        );
        read += 1;
    }
    read
}

/// What a handler writes to `/tmp` is really held there: it reads back byte for byte, however
/// it was written, and the filesystem reports it.
///
/// Answers how many cases a run answered.
async fn what_is_written_to_tmp_is_held_there(router: &MultiServiceRouter) -> usize {
    let probe = "probe-ephemeral-writes";
    create_function(router, probe, SIZES_MB[0]).await;
    let case = "every form of a write";
    let Some((document, _)) = answered(router, probe, case, &json!({"case": "write"})).await else {
        return 0;
    };
    let expected = json!({
        // Text and bytes, written and read back whole, through a line iterator and through a
        // descriptor the operating-system calls hand out.
        "text": "first\nsecond\n",
        "lines": ["first\n", "second\n"],
        "bytes": "0001027f",
        "appended": "head-tail",
        "through_a_descriptor": "by descriptor",
        "read_by_descriptor": "by descriptor",
        // One directory deeper, made by the handler itself.
        "nested": "deep",
        "nested_is_directory": true,
        // What the filesystem reports about them.
        "size": "first\nsecond\n".len(),
        "listed": ["appended.txt", "by-fd.txt", "deep", "temporary.txt", "text.txt", "written.bin"],
        "scanned": ["appended.txt:file", "by-fd.txt:file", "deep:dir", "temporary.txt:file",
                    "text.txt:file", "written.bin:file"],
        "walked": ["appended.txt", "by-fd.txt", "deep/nested.txt", "temporary.txt", "text.txt",
                   "written.bin"],
        "exists": true,
        "access_write": true,
        // A file the handler made through the standard library's own temporary-file interface,
        // which is the way most code reaches `/tmp` at all.
        "temporary": "made by tempfile",
        "temporary_under_tmp": true,
        // Removed, renamed and gone: each reported by the filesystem afterwards.
        "after_remove": "[Errno 2] No such file or directory: '/tmp/text.txt'",
        "renamed": "0001027f",
        "after_rename": false,
        "after_rmdir": false,
    });
    assert_eq!(
        document, expected,
        "case {case}: expected the bytes the handler wrote, got {document}"
    );
    1
}

/// The configured size is a LIMIT and not only a number: what is written is taken off what the
/// storage has left, and a write past it is refused the way a full device refuses one.
///
/// Answers how many cases a run answered.
async fn the_size_is_enforced_and_not_only_reported(router: &MultiServiceRouter) -> usize {
    let probe = "probe-ephemeral-limit";
    create_function(router, probe, SIZES_MB[0]).await;
    let case = "the size as a limit";
    let Some((document, _)) = answered(router, probe, case, &json!({"case": "limit"})).await else {
        return 0;
    };
    let written = 1024 * 1024;
    let reported = json!({
        "wrote": document["wrote"].clone(),
        "grown_past_the_limit": document["grown_past_the_limit"].clone(),
        "written_past_the_limit": document["written_past_the_limit"].clone(),
    });
    assert_eq!(
        reported,
        json!({
            "wrote": written,
            // A write past what is left is refused as a full device refuses one, whether it is
            // the file being grown or a write made past its end.
            "grown_past_the_limit": "[Errno 28] No space left on device: '/tmp/grown'",
            "written_past_the_limit": "[Errno 28] No space left on device: '/tmp/grown'",
        }),
        "case {case}: expected the configured size to be enforced, got {document}"
    );
    // What one file took is what the storage has less of: its bytes, and the record a
    // filesystem keeps for its name.
    let fell = number(&document, "free_fell_by", case);
    assert!(
        fell >= written && fell <= written + ENTRY_ALLOWANCE,
        "case {case}: expected the free space to fall by the {written} bytes written and the \
         record of one file (at most {ENTRY_ALLOWANCE} bytes more), got {fell}: {document}"
    );
    // And what it held comes back when the file is removed: only the record of the file the
    // row left behind is still held.
    let before = number(&document, "free_before", case);
    let after = number(&document, "free_after_removing", case);
    assert!(
        after <= before && before - after <= ENTRY_ALLOWANCE,
        "case {case}: expected removing the file to give its bytes back, leaving no more than \
         the record of one entry held ({ENTRY_ALLOWANCE} bytes), got {after} against {before}: \
         {document}"
    );
    1
}

/// One member of `document` as a number, or a panic naming what the run reported instead.
fn number(document: &Value, member: &str, case: &str) -> u64 {
    document[member]
        .as_u64()
        .unwrap_or_else(|| panic!("case {case}: expected a number at {member:?}, got {document}"))
}

/// A fill of many writes holds every write that FITS the configured size and is refused at the
/// one that would pass it: what the storage holds is the one limit, whether a handler reaches
/// it in one write or in many.
///
/// Answers how many cases a run answered.
async fn a_fill_completes_while_it_fits_and_is_refused_where_it_does_not(
    router: &MultiServiceRouter,
) -> usize {
    // The smallest storage a function may carry, filled past its size: the fill is driven in
    // the widest chunks that still leave the refusal inside the storage, so the row holds no
    // more of this host than the one storage it is about.
    let size_mb = SIZES_MB[0];
    let probe = "probe-ephemeral-fill";
    let case = format!("a fill past a {size_mb} MB storage");
    create_function(router, probe, size_mb).await;
    let fill_mb = u64::try_from(size_mb).expect("a configured size in MB") + CHUNK_MB;
    let Some((document, _)) = answered(
        router,
        probe,
        &case,
        &json!({"case": "fill", "mb": fill_mb, "chunk_mb": CHUNK_MB}),
    )
    .await
    else {
        return 0;
    };
    let size = u64::try_from(size_mb).expect("a configured size in MB") * BYTES_PER_MB;
    let chunk = CHUNK_MB * BYTES_PER_MB;
    let total = number(&document, "total", &case);
    let held = number(&document, "written", &case);
    let free = number(&document, "free", &case);
    assert_eq!(
        total, size,
        "case {case}: expected a /tmp of {size} bytes, got {document}"
    );
    // Every write that had room completed, so the file holds all of them: it is filled to
    // within the one chunk that did not fit.
    assert!(
        held > 0
            && held.is_multiple_of(chunk)
            && held < size
            && size - held <= chunk + ENTRY_ALLOWANCE,
        "case {case}: expected the file to hold whole chunks of {chunk} bytes up to within one \
         chunk of the {size} bytes the storage has, got {held}: {document}"
    );
    // And the write that would have passed the size is refused as a full device refuses one.
    assert_eq!(
        document["error"],
        json!("[Errno 28] No space left on device: '/tmp/fill'"),
        "case {case}: expected the fill to be refused at the limit, got {document}"
    );
    assert!(
        free <= chunk + ENTRY_ALLOWANCE,
        "case {case}: expected the storage to be full to within one written chunk, got {free} \
         free: {document}"
    );
    1
}

/// The ENTRIES of the storage cost it what a filesystem charges for a name, and the limits a
/// filesystem states about names and open files are the ones it answers: a run cannot make this
/// process hold files, names or descriptors without end.
///
/// Answers how many cases a run answered.
async fn the_entries_of_the_storage_cost_it_what_a_filesystem_charges(
    router: &MultiServiceRouter,
) -> usize {
    let probe = "probe-ephemeral-entries";
    create_function(router, probe, SIZES_MB[0]).await;
    let case = "the entries of the storage";
    let Some((document, _)) = answered(router, probe, case, &json!({"case": "crowded"})).await
    else {
        return 0;
    };
    let made = number(&document, "made", case);
    assert_eq!(
        json!({
            "inodes_taken": document["inodes_taken"].clone(),
            // A path past the lengths a filesystem states, and more open files than an
            // environment has, are both refused the way an operating system refuses them.
            "too_long": document["too_long"].clone(),
            "too_many_open": document["too_many_open"].clone(),
            "opened_more_than_a_handful": document["opened_more_than_a_handful"].clone(),
        }),
        json!({
            "inodes_taken": made,
            "too_long": "[Errno 36] File name too long",
            "too_many_open": "[Errno 24] Too many open files",
            "opened_more_than_a_handful": true,
        }),
        "case {case}: expected each entry to take one inode and the two limits to be answered, \
         got {document}"
    );
    // Every one of those files is empty, so what the storage has less of is the RECORD of each
    // of them and nothing else.
    let charged = number(&document, "records_charged", case);
    assert!(
        charged >= made * 4096 && charged <= made * ENTRY_ALLOWANCE,
        "case {case}: expected {made} empty files to cost the storage one record each, got \
         {charged} bytes: {document}"
    );
    1
}

/// `/tmp` is the ONE writable directory: a write under the task root or the layer root is still
/// refused for the mount being read-only, and a path on no mount of the function reaches no
/// filesystem at all.
///
/// Answers how many cases a run answered.
async fn a_write_outside_tmp_is_still_refused(router: &MultiServiceRouter) -> usize {
    let probe = "probe-ephemeral-read-only";
    create_function(router, probe, SIZES_MB[0]).await;
    let case = "a write outside the storage";
    let Some((document, _)) = answered(router, probe, case, &json!({"case": "elsewhere"})).await
    else {
        return 0;
    };
    let expected = json!({
        "wrote_under_the_task_root": "[Errno 30] Read-only file system: '/var/task/written.txt'",
        "made_a_directory_under_the_task_root":
            "[Errno 30] Read-only file system: '/var/task/made'",
        "removed_a_file_of_the_package": "[Errno 30] Read-only file system: '/var/task/notes.txt'",
        // The package's own file is still readable, and its filesystem reports itself
        // read-only.
        "read_the_package": NOTES,
        "the_package_is_read_only": true,
        // A path on no mount of this function is on no filesystem of this environment.
        "wrote_on_no_mount": true,
    });
    assert_eq!(
        document, expected,
        "case {case}: expected the read-only mounts to stay read-only, got {document}"
    );
    1
}

/// The calls a program makes on an OPEN FILE of the storage are answered from the storage: the
/// descriptor a file object carries is one the operating-system calls take, and they read,
/// write, position, flush, duplicate and reserve space on the one open file.
///
/// Answers how many cases a run answered.
async fn the_calls_a_program_makes_on_an_open_file_are_answered(
    router: &MultiServiceRouter,
) -> usize {
    let probe = "probe-ephemeral-descriptors";
    create_function(router, probe, SIZES_MB[0]).await;
    let case = "the calls on an open file";
    let Some((document, _)) = answered(router, probe, case, &json!({"case": "descriptors"})).await
    else {
        return 0;
    };
    let expected = json!({
        // The descriptor a file object carries is a descriptor of this environment, so the
        // calls a program makes by number reach the file it opened.
        "has_a_descriptor": true,
        "flushed": true,
        "stat_by_descriptor": 5,
        "read_at_an_offset": "ead",
        "wrote_at_an_offset": 1,
        "after_the_offset_write": "heZd!",
        // A duplicated descriptor is the same open file, so what one reads moves the other.
        "read_through_the_duplicate": "he",
        "position_after_the_duplicate_read": 2,
        // Space reserved on it is space the file has, and the gathered forms write and read
        // the buffers in order.
        "after_reserving": 16,
        "gathered_write": 6,
        "gathered_read": ["abc", "def"],
        // Nothing of this environment is a terminal, a symbolic link, or anything a flush of
        // the whole filesystem has to write out.
        "is_a_terminal": false,
        "blocking": true,
        "inheritable": false,
        "synced": true,
        "not_a_link": "[Errno 22] Invalid argument: '/tmp/open.txt'",
        "locked": true,
    });
    assert_eq!(
        document, expected,
        "case {case}: expected the open file to answer the calls made on it, got {document}"
    );
    1
}

/// The two output DESCRIPTORS are the invocation's own streams: a handler writing by number
/// reaches the same log `print()` reaches.
///
/// Answers how many cases a run answered.
async fn the_output_descriptors_reach_the_invocation_log(router: &MultiServiceRouter) -> usize {
    let probe = "probe-ephemeral-streams";
    create_function(router, probe, SIZES_MB[0]).await;
    let case = "the output descriptors";
    let Some((document, log)) = answered(router, probe, case, &json!({"case": "streams"})).await
    else {
        return 0;
    };
    let expected = json!({
        "stdout_descriptor": 1,
        "stderr_descriptor": 2,
        "wrote_to_stdout": "by fd one\n".len(),
        "wrote_to_stderr": "by fd two\n".len(),
        "stdin_at_its_end": "",
        "is_a_terminal": false,
    });
    assert_eq!(
        document, expected,
        "case {case}: expected the output descriptors to be the invocation's streams, got \
         {document}"
    );
    for line in ["by fd one", "by fd two"] {
        assert!(
            log.contains(line),
            "case {case}: expected the invocation log to carry {line:?}, got {log:?}"
        );
    }
    1
}

/// `/tmp` belongs to the EXECUTION ENVIRONMENT: a file one invocation wrote is there for the
/// next invocation served in the same environment, a fresh environment's `/tmp` is empty, and a
/// function whose ephemeral storage was resized gets a `/tmp` of the new size.
///
/// Answers how many cases a run answered.
async fn tmp_belongs_to_the_execution_environment(router: &MultiServiceRouter) -> usize {
    let probe = "probe-ephemeral-environment";
    create_function(router, probe, SIZES_MB[0]).await;
    let case = "the storage of one environment";
    let event = json!({"case": "marker"});
    let Some((first, _)) = answered(router, probe, case, &event).await else {
        return 0;
    };
    assert_eq!(
        first["found"],
        json!(false),
        "case {case}: the first invocation runs in a fresh environment, whose /tmp holds \
         nothing, got {first}"
    );
    let Some((second, _)) = answered(router, probe, case, &event).await else {
        return 1;
    };
    // The invocations are serialised, so the second is served in the environment the first
    // left standing — and it then reads what that invocation wrote. An environment this host
    // did not keep is a fresh one, whose /tmp is empty for the same reason the first one's was.
    if second["environment"] == first["environment"] {
        assert_eq!(
            second["found"],
            json!(true),
            "case {case}: an invocation served in the environment the one before it ran in reads \
             the file that one wrote, got {second}"
        );
        assert_eq!(
            second["marker"],
            json!("written by the invocation before"),
            "case {case}: expected the bytes the first invocation wrote, got {second}"
        );
    } else {
        assert_eq!(
            second["found"],
            json!(false),
            "case {case}: a fresh execution environment's /tmp holds nothing, got {second}"
        );
    }
    // Resizing the function's ephemeral storage mints a new revision, so the next invocation is
    // served in a new environment: its /tmp is the new size and holds nothing.
    let resized_mb = SIZES_MB[1];
    resize(router, probe, resized_mb).await;
    let Some((after, _)) = answered(router, probe, case, &event).await else {
        return 2;
    };
    let total = u64::try_from(resized_mb).expect("a configured size in MB") * BYTES_PER_MB;
    assert_eq!(
        after["total"],
        json!(total),
        "case {case}: after the resize the environment's /tmp is {total} bytes, got {after}"
    );
    assert_eq!(
        after["found"],
        json!(false),
        "case {case}: the environment serving an invocation after the resize is a new one, whose \
         /tmp holds nothing, got {after}"
    );
    3
}

/// The text of the file the package ships beside its handler, which stays read-only.
const NOTES: &str = "beside the handler\n";

/// The probe every table drives: its handler reads and writes the ephemeral storage of its own
/// execution environment the ways a Python program reaches a directory, and reports what each
/// one answered.
const PROBE_SOURCE: &str = r#"
import errno
import os
import tempfile

_TMP = "/tmp"
_TASK_ROOT = os.environ["LAMBDA_TASK_ROOT"]
_MARKER = _TMP + "/marker.txt"
_MARKER_TEXT = "written by the invocation before"


def _message(call):
    """The MESSAGE of the error `call` raised, or an empty string when it raised none."""
    try:
        call()
    except BaseException as error:
        return str(error)
    return ""


def _reason(call):
    """The ERROR NUMBER and the sentence of the error `call` raised, without the path it names,
    or an empty string when it raised none."""
    try:
        call()
    except OSError as error:
        return "[Errno %s] %s" % (error.errno, error.strerror)
    except BaseException as error:
        return type(error).__name__
    return ""


def _free():
    held = os.statvfs(_TMP)
    return held.f_bavail * held.f_frsize


def _report(event):
    held = os.statvfs(_TMP)
    return {
        "total": held.f_blocks * held.f_frsize,
        "free": held.f_bavail * held.f_frsize,
        "block_size": held.f_frsize,
        "is_directory": os.path.isdir(_TMP),
        "writable": os.access(_TMP, os.W_OK),
        "entries": sorted(os.listdir(_TMP)),
        "read_only_flag": held.f_flag & os.ST_RDONLY,
    }


def _write(event):
    with open(_TMP + "/text.txt", "w") as writing:
        writing.write("first\n")
        writing.write("second\n")
    with open(_TMP + "/text.txt") as reading:
        text = reading.read()
    with open(_TMP + "/text.txt") as reading:
        lines = list(reading)
    with open(_TMP + "/written.bin", "wb") as writing:
        writing.write(bytes([0, 1, 2, 127]))
    with open(_TMP + "/written.bin", "rb") as reading:
        raw = reading.read()
    with open(_TMP + "/appended.txt", "w") as writing:
        writing.write("head")
    with open(_TMP + "/appended.txt", "a") as writing:
        writing.write("-tail")
    with open(_TMP + "/appended.txt") as reading:
        appended = reading.read()
    descriptor = os.open(_TMP + "/by-fd.txt", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
    os.write(descriptor, b"by descriptor")
    os.close(descriptor)
    with open(_TMP + "/by-fd.txt") as reading:
        by_descriptor = reading.read()
    reader = os.open(_TMP + "/by-fd.txt", os.O_RDONLY)
    by_read = os.read(reader, 64)
    os.close(reader)
    os.mkdir(_TMP + "/deep")
    with open(_TMP + "/deep/nested.txt", "w") as writing:
        writing.write("deep")
    with open(_TMP + "/deep/nested.txt") as reading:
        nested = reading.read()
    handle, named = tempfile.mkstemp(dir=_TMP, prefix="temporary", suffix=".txt")
    with os.fdopen(handle, "w") as writing:
        writing.write("made by tempfile")
    with open(named) as reading:
        temporary = reading.read()
    os.rename(named, _TMP + "/temporary.txt")
    listed = sorted(os.listdir(_TMP))
    scanned = sorted(
        "%s:%s" % (entry.name, "dir" if entry.is_dir() else "file")
        for entry in os.scandir(_TMP)
    )
    walked = sorted(
        os.path.relpath(os.path.join(base, name), _TMP)
        for base, directories, files in os.walk(_TMP)
        for name in files
    )
    answer = {
        "text": text,
        "lines": lines,
        "bytes": raw.hex(),
        "appended": appended,
        "through_a_descriptor": by_descriptor,
        "read_by_descriptor": by_read.decode("utf-8"),
        "nested": nested,
        "nested_is_directory": os.path.isdir(_TMP + "/deep"),
        "size": os.path.getsize(_TMP + "/text.txt"),
        "listed": listed,
        "scanned": scanned,
        "walked": walked,
        "exists": os.path.exists(_TMP + "/written.bin"),
        "access_write": os.access(_TMP + "/text.txt", os.W_OK),
        "temporary": temporary,
        "temporary_under_tmp": named.startswith(_TMP + "/"),
    }
    os.remove(_TMP + "/text.txt")
    answer["after_remove"] = _message(lambda: open(_TMP + "/text.txt"))
    os.rename(_TMP + "/written.bin", _TMP + "/moved.bin")
    with open(_TMP + "/moved.bin", "rb") as reading:
        answer["renamed"] = reading.read().hex()
    answer["after_rename"] = os.path.exists(_TMP + "/written.bin")
    os.remove(_TMP + "/deep/nested.txt")
    os.rmdir(_TMP + "/deep")
    answer["after_rmdir"] = os.path.exists(_TMP + "/deep")
    return answer


def _limit(event):
    before = _free()
    wrote = 1024 * 1024
    with open(_TMP + "/held", "wb") as writing:
        writing.write(b"\x00" * wrote)
    after = _free()
    grown = _TMP + "/grown"
    with open(grown, "wb") as writing:
        pass
    room = _free()
    answer = {
        "wrote": wrote,
        "free_before": before,
        "free_fell_by": before - after,
        # Growing the file past what is left, and writing past its end, are both the device
        # having no room for it.
        "grown_past_the_limit": _message(lambda: os.truncate(grown, room + 1)),
    }

    def _past_the_end():
        with open(grown, "r+b") as writing:
            writing.seek(room + 1)
            writing.write(b"x")

    answer["written_past_the_limit"] = _message(_past_the_end)
    os.remove(_TMP + "/held")
    answer["free_after_removing"] = _free()
    return answer


def _fill(event):
    """Write `mb` MB to one file, a chunk at a time, and report what the storage came to."""
    chunk_mb = int(event["chunk_mb"])
    chunk = b"\x00" * (chunk_mb * 1024 * 1024)
    path = _TMP + "/fill"
    error = None
    try:
        with open(path, "wb") as writing:
            for _ in range(int(event["mb"]) // chunk_mb):
                writing.write(chunk)
    except BaseException as exc:
        error = str(exc)
    held = os.statvfs(_TMP)
    answer = {
        "total": held.f_blocks * held.f_frsize,
        "written": os.path.getsize(path),
        "error": error,
        "free": held.f_bavail * held.f_frsize,
    }
    os.remove(path)
    return answer


def _descriptors(event):
    """The calls a program makes on one open file, by the descriptor it carries."""
    path = _TMP + "/open.txt"
    with open(path, "w+b") as held:
        held.write(b"head!")
        held.flush()
        descriptor = held.fileno()
        answer = {
            "has_a_descriptor": isinstance(descriptor, int) and descriptor > 2,
            "flushed": os.fsync(descriptor) is None,
            "stat_by_descriptor": os.fstat(descriptor).st_size,
            "read_at_an_offset": os.pread(descriptor, 3, 1).decode("utf-8"),
            "wrote_at_an_offset": os.pwrite(descriptor, b"Z", 2),
        }
        held.seek(0)
        answer["after_the_offset_write"] = held.read().decode("utf-8")
        held.seek(0)
        duplicate = os.dup(descriptor)
        answer["read_through_the_duplicate"] = os.read(duplicate, 2).decode("utf-8")
        answer["position_after_the_duplicate_read"] = held.tell()
        os.close(duplicate)
        os.posix_fallocate(descriptor, 0, 16)
        answer["after_reserving"] = os.fstat(descriptor).st_size
        os.ftruncate(descriptor, 0)
        os.lseek(descriptor, 0, 0)
        answer["gathered_write"] = os.writev(descriptor, [b"abc", b"def"])
        os.lseek(descriptor, 0, 0)
        first = bytearray(3)
        second = bytearray(3)
        os.readv(descriptor, [first, second])
        answer["gathered_read"] = [
            bytes(first).decode("utf-8"),
            bytes(second).decode("utf-8"),
        ]
        answer["is_a_terminal"] = os.isatty(descriptor)
        answer["blocking"] = os.get_blocking(descriptor)
        answer["inheritable"] = os.get_inheritable(descriptor)
        answer["synced"] = os.sync() is None
        answer["not_a_link"] = _message(lambda: os.readlink(path))
        answer["locked"] = os.lockf(descriptor, 1, 0) is None
    os.remove(path)
    return answer


def _crowded(event):
    """What the storage answers when its ENTRIES rather than its bytes are what runs out: a name
    of its own costs it, an inode of its own is taken for it, a path longer than it takes is
    refused, and an environment has only so many files open at once."""
    before = _free()
    opening = os.statvfs(_TMP)
    made = 64
    for index in range(made):
        with open(_TMP + "/empty-%d" % index, "w"):
            pass
    after = os.statvfs(_TMP)
    answer = {
        "made": made,
        "records_charged": before - after.f_bavail * after.f_frsize,
        "inodes_taken": opening.f_ffree - after.f_ffree,
        "too_long": _reason(lambda: open(_TMP + "/" + "n" * 300, "w")),
    }
    streams = []
    try:
        for index in range(4096):
            streams.append(open(_TMP + "/open-%d" % index, "w"))
        answer["too_many_open"] = ""
    except OSError as error:
        answer["too_many_open"] = "[Errno %s] %s" % (error.errno, error.strerror)
    answer["opened_more_than_a_handful"] = len(streams) > 64
    for stream in streams:
        stream.close()
    return answer


def _elsewhere(event):
    written = _TASK_ROOT + "/written.txt"
    made = _TASK_ROOT + "/made"
    notes = _TASK_ROOT + "/notes.txt"
    answer = {
        "wrote_under_the_task_root": _message(lambda: open(written, "w")),
        "made_a_directory_under_the_task_root": _message(lambda: os.mkdir(made)),
        "removed_a_file_of_the_package": _message(lambda: os.remove(notes)),
    }
    with open(notes) as reading:
        answer["read_the_package"] = reading.read()
    held = os.statvfs(notes)
    answer["the_package_is_read_only"] = bool(held.f_flag & os.ST_RDONLY)
    answer["wrote_on_no_mount"] = _message(lambda: open("/etc/passwd", "w")) != ""
    return answer


def _streams(event):
    import sys

    return {
        "stdout_descriptor": sys.stdout.fileno(),
        "stderr_descriptor": sys.stderr.fileno(),
        "wrote_to_stdout": os.write(1, b"by fd one\n"),
        "wrote_to_stderr": os.write(2, b"by fd two\n"),
        "stdin_at_its_end": os.read(0, 16).decode("utf-8"),
        "is_a_terminal": os.isatty(1),
    }


def _marker(event):
    held = os.statvfs(_TMP)
    found = os.path.exists(_MARKER)
    marker = ""
    if found:
        with open(_MARKER) as reading:
            marker = reading.read()
    else:
        with open(_MARKER, "w") as writing:
            writing.write(_MARKER_TEXT)
    return {
        "environment": os.environ.get("AWS_LAMBDA_LOG_STREAM_NAME", ""),
        "found": found,
        "marker": marker,
        "total": held.f_blocks * held.f_frsize,
    }


_CASES = {
    "report": _report,
    "write": _write,
    "limit": _limit,
    "fill": _fill,
    "crowded": _crowded,
    "descriptors": _descriptors,
    "elsewhere": _elsewhere,
    "streams": _streams,
    "marker": _marker,
}


def handler(event, context):
    return _CASES[event["case"]](event)
"#;
