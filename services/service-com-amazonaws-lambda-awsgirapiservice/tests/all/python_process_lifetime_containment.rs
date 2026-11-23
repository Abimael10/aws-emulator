// SPDX-License-Identifier: CC0-1.0

//! What a `python3.x` run can do to the PROCESS that serves it.
//!
//! The rule under test: no call a handler can make ends, replaces or detaches the process the
//! emulator runs in — every operating-system name that would do so is either absent from the
//! surface a run is given or raises when it is called, and the interpreter's own exit raises
//! `SystemExit` for the runtime to report as that invocation's failure. The process keeps
//! serving: an invocation that tried every one of them is answered, and so is the next
//! invocation after it.
//!
//! That matters because every function of every tenant is served in this one process: a guest
//! call that ended it would discard every other invocation's run and all of the emulator's
//! state, and it would do so past every bound a run is held to — nothing is traced, charged or
//! latched on the way out.
//!
//! The rows differ along what the rule generalises over: the KIND of name tried — the
//! interpreter's immediate exit, the abort of a process, a signal to this very process,
//! starting a second one by forking, replacing this one by exec, running a shell, spawning a
//! child, and the language's own `sys.exit` — and whether the name is reached through `os` or
//! through the module `os` is built on.
//!
//! Every row drives real HTTP requests through the router. One invocation tries them all and
//! reports what each one did, so a name that really ended the process would take this test
//! binary with it rather than report a passing row.

use std::sync::Arc;

use serde_json::Value;

// The harness of the sibling Python suites, reused rather than copied.
use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::python_frame_source_lines::{
    create_function, invoke, poll_until_active, router_on, Answer,
};

/// The function this suite creates.
const FUNCTION: &str = "python-process-lifetime";

/// The handler module: one attempt per name, each made inside the run and reported by the
/// class of what it raised.
///
/// A name the surface does not carry at all raises `AttributeError`, and one it carries as a
/// refusal raises the error a host answers for it; either way the run reports it and goes on
/// to the next. `os.P_NOWAIT` and `signal.SIGKILL` are read inside the attempt, so a platform
/// that carries neither is reported the same way.
const MODULE: &str = "\
import os
import signal
import sys


ATTEMPTS = {
    \"os._exit\": lambda: os._exit(0),
    \"os.abort\": lambda: os.abort(),
    \"os.kill\": lambda: os.kill(os.getpid(), signal.SIGKILL),
    \"os.fork\": lambda: os.fork(),
    \"os.execv\": lambda: os.execv(\"/bin/sh\", [\"sh\", \"-c\", \"true\"]),
    \"os.system\": lambda: os.system(\"true\"),
    \"os.popen\": lambda: os.popen(\"true\"),
    \"os.spawnv\": lambda: os.spawnv(os.P_NOWAIT, \"/bin/sh\", [\"sh\", \"-c\", \"true\"]),
    \"posix._exit\": lambda: __import__(\"posix\")._exit(0),
    \"posix.abort\": lambda: __import__(\"posix\").abort(),
    \"sys.exit\": lambda: sys.exit(1),
}


def handler(event, context):
    outcome = {}
    for name in sorted(ATTEMPTS):
        try:
            ATTEMPTS[name]()
            outcome[name] = \"returned\"
        except BaseException as error:
            outcome[name] = type(error).__name__
    return outcome
";

/// The names the rows cover, each with the reason it may not take the process.
const ATTEMPTED: &[&str] = &[
    "os._exit",
    "os.abort",
    "os.kill",
    "os.fork",
    "os.execv",
    "os.system",
    "os.popen",
    "os.spawnv",
    "posix._exit",
    "posix.abort",
    "sys.exit",
];

/// What an attempt may come to: the name is not on the surface at all, or it is there and
/// refuses.
///
/// `AttributeError` is the absent name; `OSError` and its own subclasses are the refusal a
/// host-reaching call answers with (`state::execution::python`'s `REFUSED_POSIX_CALLS`);
/// `SystemExit` is the language's own exit, which is an exception a run reports rather than a
/// process ending; `NotImplementedError` is a platform that carries the name and implements
/// nothing behind it.
const ALLOWED: &[&str] = &[
    "AttributeError",
    "NotImplementedError",
    "OSError",
    "PermissionError",
    "SystemExit",
    "TypeError",
];

/// What the run reported for each attempted name.
fn outcomes(answered: &Answer) -> serde_json::Map<String, Value> {
    answered
        .json()
        .as_object()
        .cloned()
        .unwrap_or_else(serde_json::Map::new)
}

#[tokio::test]
async fn no_guest_call_can_end_the_process_that_serves_it() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_function(&router, FUNCTION, &[("index.py", MODULE)], "index.handler").await;
    poll_until_active(&router, FUNCTION).await;

    let answered = invoke(&router, FUNCTION, b"{}").await;
    assert_eq!(
        answered.status,
        200,
        "the invocation that tries every process-ending name must be answered, got {} {}",
        answered.status,
        answered.text()
    );
    let reported = outcomes(&answered);
    for name in ATTEMPTED {
        let outcome = reported
            .get(*name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            ALLOWED.contains(&outcome.as_str()),
            "case {name:?}: the name must be absent from the run's surface or refuse it, \
             expected one of {ALLOWED:?}, got {outcome:?}"
        );
    }

    // The process is still serving: the next invocation is answered the same way, which is
    // what an attempt that had really ended the process could not leave behind.
    let again = invoke(&router, FUNCTION, b"{}").await;
    assert_eq!(
        again.status,
        200,
        "the invocation after it must be served too, got {} {}",
        again.status,
        again.text()
    );
    assert_eq!(
        outcomes(&again),
        reported,
        "the run after it must report the same surface, expected {reported:?}, got {:?}",
        outcomes(&again)
    );
}
