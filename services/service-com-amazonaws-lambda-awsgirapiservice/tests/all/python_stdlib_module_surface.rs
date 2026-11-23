// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the MODULES of the standard library a `python3.x` function's run is
//! given: `CreateFunction` then `Invoke` (`POST
//! /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: a `python3.x` runtime presents the standard library of the runtime it
//! advertises, so every module of that library is ON THE IMPORT PATH whether or not this
//! emulator registers an extension for it — a handler module, or a package the deployment
//! package vendors, whose body imports one of them imports cleanly and the invocation answers
//! the handler's own value with no `X-Amz-Function-Error`. A capability the runtime cannot
//! serve is refused by the CALL that needs it, with the error the system call it stands for
//! gives, and never by the import; what a module can answer out of data or computation it
//! answers. An import that fails blames a deployment package that is not at fault.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what the
//! rule generalises over: which module of the standard library is imported, whether it is
//! imported by the handler's own module body or through the import system at run time, which
//! handler module name the package carries, which surface of the module is read (the names, a
//! registration, a computation), and which call is the one that needs the absent capability.

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

/// How long past a due instant this suite waits before calling. The transition is owed by the
/// CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` every function this suite creates carries, in seconds. Nothing here is about
/// a deadline, so it is given generous room — the standard library's own import work is the
/// only thing a run of this suite does, and how long that takes on a loaded host is not what
/// any row asserts.
const FUNCTION_TIMEOUT_SECONDS: i32 = 600;

/// How many times a case re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 6;

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

/// Dispatch one request.
async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.us-east-1.amazonaws.com{uri}"))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
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

/// Create a `python3.12` function whose package holds `source` as `<module>.py`, configured
/// with `<module>.handler`, and wait out the provisioning window the create documents.
async fn create_function(router: &MultiServiceRouter, name: &str, module: &str, source: &str) {
    let entry = format!("{module}.py");
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": format!("{module}.handler"),
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[(entry.as_str(), source)])),
            },
        }))
        .expect("the create body serializes"),
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

/// `Invoke` on `name` with `event`, re-driven while the answer reports a bound this whole
/// test binary shares rather than a run.
async fn invoke(router: &MultiServiceRouter, name: &str, event: &Value) -> Answer {
    let mut answered = invoke_once(router, name, event).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answered.status, &answered.text()) {
            break;
        }
        answered = invoke_once(router, name, event).await;
    }
    answered
}

/// One `Invoke` request on the function `name` names.
async fn invoke_once(router: &MultiServiceRouter, name: &str, event: &Value) -> Answer {
    call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &serde_json::to_vec(event).expect("the event serializes"),
    )
    .await
}

/// What one case of the probe answered, or `None` when the run was ended by a bound this whole
/// test binary shares rather than by anything the case asked for.
async fn answered(
    router: &MultiServiceRouter,
    name: &str,
    case: &str,
    event: &Value,
) -> Option<Value> {
    let answer = invoke(router, name, event).await;
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
        "case {case}: the handler's module imports cleanly and the handler catches each \
         refusal itself, so the invocation carries no function error, got {failure:?} with {}",
        answer.text()
    );
    Some(answer.json())
}

/// The whole rule, driven END TO END as one test: ONE real `CreateFunction` whose handler
/// module BODY imports the modules under test — which is where a vendored client library's own
/// imports run, and where an absent module fails the whole package's import, the recorded
/// symptom — and real `Invoke` requests over the router, which the tables below read.
///
/// One test, one function and one stream of invocations, the way the sibling runtime suites
/// are built: the memory bound a run is stopped on is read from the WHOLE process
/// (`state::execution::python`) and this binary runs its suites in parallel, so fewer
/// interpreters in flight at once is fewer runs stopped for memory belonging to another suite.
/// A case whose run was stopped by one of those shared bounds says nothing about the rule and
/// is left out of the reading — but not every one of them can be: each table answers how many
/// of its cases a RUN answered, and a test that read none of them at all is a test that
/// asserted nothing, so it fails rather than passing silently.
#[tokio::test]
async fn the_python_runtime_presents_the_modules_of_its_standard_library() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let probe = "probe-stdlib-modules";
    create_function(&router, probe, "lambda_function", PROBE_SOURCE).await;
    // The module body imported every module under test on the way up, so a `surface` answer is
    // itself the evidence that each import was clean; the tables read what the modules then
    // answer, in ONE run each.
    let mut read = the_modules_this_runtime_supplies_answer_their_own_surface(&router, probe).await;
    read += every_module_of_the_standard_library_is_on_the_import_path(&router, probe).await;
    assert!(
        read > 0,
        "expected at least one case of the tables above to be answered by a run, got {read}: \
         every invocation reported a bound this whole test binary shares, so nothing of the \
         rule was read"
    );
}

/// The recorded shape and its generalisation, in ONE run: the handler module's BODY imported
/// the signal, child-process, waiting, socket and TLS interfaces and the standard library's own
/// modules built on them, so an invocation that answers at all is an import that was clean —
/// and what it answers is the surface each of those modules states.
///
/// The cases below differ along which module is read, which surface of it is read (its names, a
/// registration, a computation) and which call is the one that needs a capability this runtime
/// has none of.
///
/// Answers how many cases a run answered.
async fn the_modules_this_runtime_supplies_answer_their_own_surface(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    let case = "the surface of the modules this runtime supplies";
    let Some(document) = answered(router, probe, case, &json!({"case": "surface"})).await else {
        return 0;
    };
    assert_eq!(
        document.get("imported").and_then(Value::as_bool),
        Some(true),
        "case {case}: the handler module's body must have imported every module under test, got \
         {document}"
    );
    the_signal_interfaces_names_are_the_runtimes_own(&document, case);
    a_signal_registration_is_remembered_and_nothing_delivers_one(&document, case);
    a_call_that_needs_a_delivery_is_refused_where_it_is_asked_for(&document, case);
    the_child_process_interface_answers_its_names_and_runs_its_programs(&document, case);
    the_socket_interface_names_the_options_a_client_library_sets(&document, case);
    1
}

/// The names a program reads off the signal interface are the ones the standard library
/// states.
///
/// Source of each expected value: the signal numbers this platform's `<signal.h>` defines, as
/// Python's own `signal` module publishes them, together with that module's own
/// enumerations (`Signals`, `Handlers`), its handler sentinels, its interval-timer selectors,
/// its mask operations, `NSIG`, and the text the operating system gives for a signal
/// (`strsignal`). The table names signals the report this rule came from does not cite.
///
fn the_signal_interfaces_names_are_the_runtimes_own(document: &Value, case: &str) {
    let document = &document["signal_names"];
    let expected = json!({
        "sighup": 1,
        "sigint": 2,
        "sigquit": 3,
        "sigkill": 9,
        "sigusr1": 10,
        "sigsegv": 11,
        "sigpipe": 13,
        "sigalrm": 14,
        "sigterm": 15,
        "sigchld": 17,
        "sigwinch": 28,
        "sigsys": 31,
        "nsig": 65,
        "sig_dfl": 0,
        "sig_ign": 1,
        "itimer_real": 0,
        "itimer_virtual": 1,
        "itimer_prof": 2,
        "sig_block": 0,
        "sig_unblock": 1,
        "sig_setmask": 2,
        "enum_name": "SIGTERM",
        "enum_value": 15,
        "handler_name": "SIG_IGN",
        "strsignal_kill": "Killed",
        "strsignal_terminated": "Terminated",
        "strsignal_unknown": null,
        "valid_signals_hold_them": true,
        "itimer_error_is_os_error": true,
    });
    assert_eq!(
        *document, expected,
        "case {case}: expected the standard library's own names, got {document}"
    );
}

/// A REGISTRATION is the module's own: installing a handler answers the handler that was in
/// place before it and the installed one is reported back, while the signal the interpreter
/// installs its own handler for on the way up reports that handler and an untouched signal
/// reports the platform's default. Nothing delivers one, so no handler is ever called.
///
/// The rows differ along which signal is registered, along what is installed (a callable of
/// the program's own, the ignore sentinel) and along whether the signal was touched at all.
///
/// Source: `signal.signal` answers the previous handler, `signal.getsignal` answers the
/// current one, CPython installs `default_int_handler` for the interrupt signal, and a signal
/// nothing has registered reports `SIG_DFL`.
///
fn a_signal_registration_is_remembered_and_nothing_delivers_one(document: &Value, case: &str) {
    let document = &document["signal_registration"];
    let expected = json!({
        // Installing over a handler of the program's own answers that handler, and the one
        // just installed is what the module reports from then on.
        "answered_previous": "first_handler",
        "installed": "second_handler",
        // The ignore sentinel is a registration too, and it is reported as itself.
        "ignored": "SIG_IGN",
        // A signal nothing registered is the platform's default, and the interrupt signal is
        // the handler the interpreter installs on its way up.
        "untouched": "SIG_DFL",
        "interrupt": "default_int_handler",
        // No handler the module holds is ever called, because nothing signals the run.
        "handlers_called": 0,
    });
    assert_eq!(
        *document, expected,
        "case {case}: expected the registration to be remembered and nothing delivered, got \
         {document}"
    );
}

/// A call that needs the operating system to DELIVER or to TIME a signal is refused where it
/// is asked for — with the error a system call that is not there gives — and never by the
/// import. The language's own refusals of a bad argument are kept.
///
/// The rows differ along which call is made (arming the real-time timer, arming and reading an
/// interval timer, raising a signal, signalling a thread, changing the mask, waiting for a
/// delivery), along what is asked of a registration that cannot be one (a handler that is
/// neither callable nor a sentinel, a number that is no signal) and along the wakeup
/// descriptor asked for.
///
/// Source of each expected refusal: `OSError` carrying `ENOSYS` is what a system call that is
/// not implemented answers with; `signal.setitimer`/`getitimer` raise the module's own
/// `ItimerError`, which is an `OSError`; `signal.signal` raises `TypeError` for a handler that
/// is neither callable nor a sentinel and `ValueError` for a number that is no signal of this
/// platform; `signal.set_wakeup_fd(-1)` answers the descriptor that was in place and any other
/// descriptor is refused.
///
fn a_call_that_needs_a_delivery_is_refused_where_it_is_asked_for(document: &Value, case: &str) {
    let document = &document["signal_refusals"];
    let expected = json!({
        "alarm": ["OSError", "ENOSYS"],
        "setitimer": ["ItimerError", "ENOSYS"],
        "getitimer": ["ItimerError", "ENOSYS"],
        "raise_signal": ["OSError", "ENOSYS"],
        "pthread_kill": ["OSError", "ENOSYS"],
        "pthread_sigmask": ["OSError", "ENOSYS"],
        "sigpending": ["OSError", "ENOSYS"],
        "sigwait": ["OSError", "ENOSYS"],
        "sigtimedwait": ["OSError", "ENOSYS"],
        "siginterrupt": ["OSError", "ENOSYS"],
        // The language's own refusals, which this module keeps.
        "handler_is_no_handler": ["TypeError", null],
        "number_is_no_signal": ["ValueError", null],
        "read_of_no_signal": null,
        // No descriptor is in place, which is what taking one away answers, and there is
        // none to put there.
        "wakeup_cleared": -1,
        "wakeup_descriptor": ["ValueError", null],
    });
    assert_eq!(
        *document, expected,
        "case {case}: expected each call to answer for the capability it needs, got {document}"
    );
}

/// The child-process interface answers the names a program reads off it and the pieces of it
/// that are computation, and RUNS the program every entry point is given: a child is one
/// program of this execution environment, performed over the utilities that environment has.
///
/// The rows differ along which name is read, along which computation is performed (an exit
/// status checked, an error rendered, an argument sequence quoted) and along which entry point
/// starts the program (`run`, `call`, `check_call`, `check_output`, a `Popen`, a shell), plus a
/// program the environment does not have at all.
///
/// Source of each expected value: `subprocess.PIPE`/`STDOUT`/`DEVNULL` are `-1`, `-2` and
/// `-3`; `CompletedProcess.check_returncode` raises `CalledProcessError` whose message is
/// "Command '<args>' returned non-zero exit status <status>."; `TimeoutExpired`'s message is
/// "Command '<cmd>' timed out after <timeout> seconds"; `list2cmdline` quotes an argument
/// holding a space; `true` and `echo` are utilities of the sandbox, which exit with success and
/// write their operands; and `ENOENT` is what a command search that found nothing answers with.
///
fn the_child_process_interface_answers_its_names_and_runs_its_programs(
    document: &Value,
    case: &str,
) {
    let document = &document["process"];
    let expected = json!({
        "pipe": -1,
        "stdout": -2,
        "devnull": -3,
        "errors_are_subprocess_errors": true,
        "success_is_not_raised": true,
        "checked_status": [
            "CalledProcessError",
            "Command '['a', 'b']' returned non-zero exit status 2."
        ],
        "signalled_message": "Command 'x' died with signal 9.",
        "timeout_message": "Command 'x' timed out after 3 seconds",
        "output_is_stdout": "written",
        "completed_reports_what_it_was_built_with": ["['a']", 0, "out", "err"],
        "command_line": "a \"b c\" \"\"",
        "run": 0,
        "call": 0,
        "check_call": 0,
        "check_output": "out\n",
        "popen": 0,
        "shell": [0, ""],
        "shell_output": "out",
        "absent": ["FileNotFoundError", "ENOENT"],
    });
    assert_eq!(
        *document, expected,
        "case {case}: expected the module's own names and the programs it ran, got {document}"
    );
}

/// The socket interface names the options a client library sets on a connection it opened, so
/// a module that reads one of them at import time imports and runs.
///
/// Source of each expected value: the numbers this platform's own headers define, as Python's
/// `socket` module publishes them — `<asm-generic/socket.h>` for the socket-level options,
/// `<netinet/tcp.h>` for the TCP-level ones, `<sys/socket.h>` for the families, the socket
/// types and the message flags, `<netdb.h>` for the lookup flags and errors, and
/// `<netinet/in.h>` for the special addresses. The table names options the report this rule
/// came from does not cite.
///
fn the_socket_interface_names_the_options_a_client_library_sets(document: &Value, case: &str) {
    let document = &document["socket_names"];
    let expected = json!({
        "so_sndbuf": 7,
        "so_rcvbuf": 8,
        "so_linger": 13,
        "so_rcvtimeo": 20,
        "so_sndtimeo": 21,
        "so_type": 3,
        "so_reuseport": 15,
        "tcp_keepidle": 4,
        "tcp_keepintvl": 5,
        "tcp_keepcnt": 6,
        "tcp_user_timeout": 18,
        "tcp_maxseg": 2,
        "msg_peek": 2,
        "msg_dontwait": 64,
        "msg_waitall": 256,
        "ai_numerichost": 4,
        "ai_addrconfig": 32,
        "eai_noname": -2,
        "eai_again": -3,
        "ipproto_ipv6": 41,
        "ipv6_v6only": 26,
        "af_unix": 1,
        "sock_raw": 3,
        "inaddr_loopback": 2130706433,
        "somaxconn": 4096,
    });
    assert_eq!(
        *document, expected,
        "case {case}: expected the platform's own option numbers, got {document}"
    );
}

/// The generalisation past the modules this change is about: every module of the standard
/// library a handler or a vendored client library reaches is on the import path, so importing
/// one through the import system at run time succeeds too.
///
/// Source: the modules of the CPython standard library a client library's own import chain
/// touches — the ones this runtime supplies itself, the ones the frozen library builds on
/// them, and the ones that have nothing to do with the host — including modules the report
/// this rule came from does not cite. The table is deliberately the CHEAP half of that chain:
/// the modules of the standard library that pull in dozens of others (`asyncio`,
/// `multiprocessing`, `email`, `urllib.request`) are on the import path too, but importing one
/// costs memory every suite of this test binary shares (the bound a run is stopped on is read
/// from the whole process), so they are left to the module whose body imports them above
/// rather than driven here as well.
///
/// Answers how many cases a run answered.
async fn every_module_of_the_standard_library_is_on_the_import_path(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    let case = "the modules on the import path";
    let modules = [
        "signal",
        "subprocess",
        "select",
        "selectors",
        "socket",
        "socketserver",
        "ssl",
        "queue",
        "uuid",
        "platform",
        "gzip",
        "hashlib",
        "hmac",
        "threading",
        "logging",
        "weakref",
        "json",
        "decimal",
        "struct",
        "binascii",
        "random",
        "re",
    ];
    let event = json!({"case": "modules", "modules": modules.as_slice()});
    let Some(document) = answered(router, probe, case, &event).await else {
        return 0;
    };
    for module in modules {
        let outcome = document
            .get(module)
            .and_then(Value::as_str)
            .unwrap_or("<not reported>");
        assert_eq!(
            outcome, "",
            "case {case} {module}: expected the module to import cleanly, got {outcome:?}"
        );
    }
    1
}

/// The probe both tables drive: its module BODY imports the modules a vendored client library
/// imports — the recorded symptom is an absent one failing that import — and its handler
/// answers the surface the event names.
const PROBE_SOURCE: &str = r#"
import errno
import importlib
import multiprocessing
import select
import selectors
import signal
import socket
import ssl
import struct
import subprocess
import threading

# Every module above was imported by this module's BODY, so a handler that runs at all is a
# clean import of each.
_IMPORTED = True

# How many times a handler this run registered was called. Nothing signals an invocation, so
# nothing here ever increments it.
_CALLED = []


def _refused(call):
    """The error `call` raised: its name and the NAME of its `errno`, or no error."""
    try:
        call()
    except BaseException as error:
        number = getattr(error, "errno", None)
        return [type(error).__name__, errno.errorcode.get(number)]
    return ["", None]


def _failed(call):
    """The error `call` raised: its name and the message it rendered."""
    try:
        call()
    except BaseException as error:
        return [type(error).__name__, str(error)]
    return ["", ""]


def _signal_names(event):
    return {
        "sighup": int(signal.SIGHUP),
        "sigint": int(signal.SIGINT),
        "sigquit": int(signal.SIGQUIT),
        "sigkill": int(signal.SIGKILL),
        "sigusr1": int(signal.SIGUSR1),
        "sigsegv": int(signal.SIGSEGV),
        "sigpipe": int(signal.SIGPIPE),
        "sigalrm": int(signal.SIGALRM),
        "sigterm": int(signal.SIGTERM),
        "sigchld": int(signal.SIGCHLD),
        "sigwinch": int(signal.SIGWINCH),
        "sigsys": int(signal.SIGSYS),
        "nsig": int(signal.NSIG),
        "sig_dfl": int(signal.SIG_DFL),
        "sig_ign": int(signal.SIG_IGN),
        "itimer_real": int(signal.ITIMER_REAL),
        "itimer_virtual": int(signal.ITIMER_VIRTUAL),
        "itimer_prof": int(signal.ITIMER_PROF),
        "sig_block": int(signal.SIG_BLOCK),
        "sig_unblock": int(signal.SIG_UNBLOCK),
        "sig_setmask": int(signal.SIG_SETMASK),
        "enum_name": signal.Signals(15).name,
        "enum_value": int(signal.Signals.SIGTERM),
        "handler_name": signal.Handlers(1).name,
        "strsignal_kill": signal.strsignal(signal.SIGKILL),
        "strsignal_terminated": signal.strsignal(signal.SIGTERM),
        "strsignal_unknown": signal.strsignal(9999),
        "valid_signals_hold_them": set(
            [signal.SIGTERM, signal.SIGUSR2]
        ).issubset(signal.valid_signals()),
        "itimer_error_is_os_error": issubclass(signal.ItimerError, OSError),
    }


def first_handler(signalnum, frame):
    _CALLED.append(signalnum)


def second_handler(signalnum, frame):
    _CALLED.append(signalnum)


def _named_handler(handler):
    """What `getsignal` answered, as a name: a sentinel by its own name and a handler by its
    function name."""
    if handler in (signal.SIG_DFL, signal.SIG_IGN):
        return signal.Handlers(int(handler)).name
    return getattr(handler, "__name__", repr(handler))


def _signal_registration(event):
    signal.signal(signal.SIGUSR1, first_handler)
    previous = signal.signal(signal.SIGUSR1, second_handler)
    signal.signal(signal.SIGUSR2, signal.SIG_IGN)
    return {
        "answered_previous": _named_handler(previous),
        "installed": _named_handler(signal.getsignal(signal.SIGUSR1)),
        "ignored": _named_handler(signal.getsignal(signal.SIGUSR2)),
        "untouched": _named_handler(signal.getsignal(signal.SIGHUP)),
        "interrupt": _named_handler(signal.getsignal(signal.SIGINT)),
        "handlers_called": len(_CALLED),
    }


def _signal_refusals(event):
    return {
        "alarm": _refused(lambda: signal.alarm(1)),
        "setitimer": _refused(lambda: signal.setitimer(signal.ITIMER_REAL, 0.5)),
        "getitimer": _refused(lambda: signal.getitimer(signal.ITIMER_REAL)),
        "raise_signal": _refused(lambda: signal.raise_signal(signal.SIGUSR1)),
        "pthread_kill": _refused(lambda: signal.pthread_kill(0, signal.SIGUSR1)),
        "pthread_sigmask": _refused(
            lambda: signal.pthread_sigmask(signal.SIG_BLOCK, [signal.SIGUSR1])
        ),
        "sigpending": _refused(lambda: signal.sigpending()),
        "sigwait": _refused(lambda: signal.sigwait([signal.SIGUSR1])),
        "sigtimedwait": _refused(lambda: signal.sigtimedwait([signal.SIGUSR1], 0)),
        "siginterrupt": _refused(lambda: signal.siginterrupt(signal.SIGUSR1, True)),
        "handler_is_no_handler": _refused(lambda: signal.signal(signal.SIGUSR1, 7)),
        "number_is_no_signal": _refused(lambda: signal.signal(9999, signal.SIG_DFL)),
        "read_of_no_signal": signal.strsignal(9999),
        "wakeup_cleared": signal.set_wakeup_fd(-1),
        "wakeup_descriptor": _refused(lambda: signal.set_wakeup_fd(3)),
    }


def _process(event):
    finished = subprocess.CompletedProcess(["a"], 0, "out", "err")
    written = subprocess.CalledProcessError(1, ["a"], "written")
    return {
        "pipe": int(subprocess.PIPE),
        "stdout": int(subprocess.STDOUT),
        "devnull": int(subprocess.DEVNULL),
        "errors_are_subprocess_errors": (
            issubclass(subprocess.CalledProcessError, subprocess.SubprocessError)
            and issubclass(subprocess.TimeoutExpired, subprocess.SubprocessError)
        ),
        "success_is_not_raised": finished.check_returncode() is None,
        "checked_status": _failed(
            lambda: subprocess.CompletedProcess(["a", "b"], 2).check_returncode()
        ),
        "signalled_message": str(subprocess.CalledProcessError(-9, "x")),
        "timeout_message": str(subprocess.TimeoutExpired("x", 3)),
        "output_is_stdout": written.stdout,
        "completed_reports_what_it_was_built_with": [
            str(finished.args),
            finished.returncode,
            finished.stdout,
            finished.stderr,
        ],
        "command_line": subprocess.list2cmdline(["a", "b c", ""]),
        # Every entry point RUNS the program it was given, over the utilities the execution
        # environment has, and reports what that program came to.
        "run": subprocess.run(["true"]).returncode,
        "call": subprocess.call(["true"]),
        "check_call": subprocess.check_call(["true"]),
        "check_output": subprocess.check_output(["echo", "out"], text=True),
        "popen": subprocess.Popen(["true"]).wait(),
        "shell": list(subprocess.getstatusoutput("true")),
        "shell_output": subprocess.getoutput("echo out"),
        # A program the environment does not have is where the command search ends with
        # nothing.
        "absent": _refused(lambda: subprocess.run(["absent-program"])),
    }


def _socket_names(event):
    return {
        "so_sndbuf": int(socket.SO_SNDBUF),
        "so_rcvbuf": int(socket.SO_RCVBUF),
        "so_linger": int(socket.SO_LINGER),
        "so_rcvtimeo": int(socket.SO_RCVTIMEO),
        "so_sndtimeo": int(socket.SO_SNDTIMEO),
        "so_type": int(socket.SO_TYPE),
        "so_reuseport": int(socket.SO_REUSEPORT),
        "tcp_keepidle": int(socket.TCP_KEEPIDLE),
        "tcp_keepintvl": int(socket.TCP_KEEPINTVL),
        "tcp_keepcnt": int(socket.TCP_KEEPCNT),
        "tcp_user_timeout": int(socket.TCP_USER_TIMEOUT),
        "tcp_maxseg": int(socket.TCP_MAXSEG),
        "msg_peek": int(socket.MSG_PEEK),
        "msg_dontwait": int(socket.MSG_DONTWAIT),
        "msg_waitall": int(socket.MSG_WAITALL),
        "ai_numerichost": int(socket.AI_NUMERICHOST),
        "ai_addrconfig": int(socket.AI_ADDRCONFIG),
        "eai_noname": int(socket.EAI_NONAME),
        "eai_again": int(socket.EAI_AGAIN),
        "ipproto_ipv6": int(socket.IPPROTO_IPV6),
        "ipv6_v6only": int(socket.IPV6_V6ONLY),
        "af_unix": int(socket.AF_UNIX),
        "sock_raw": int(socket.SOCK_RAW),
        "inaddr_loopback": int(socket.INADDR_LOOPBACK),
        "somaxconn": int(socket.SOMAXCONN),
    }


def _modules(event):
    outcome = {}
    for name in event["modules"]:
        try:
            importlib.import_module(name)
            outcome[name] = ""
        except BaseException as error:
            outcome[name] = "%s: %s" % (type(error).__name__, error)
    return outcome


def _surface(event):
    """Everything the modules this runtime supplies answer, in ONE run."""
    return {
        "imported": _IMPORTED,
        "signal_names": _signal_names(event),
        "signal_registration": _signal_registration(event),
        "signal_refusals": _signal_refusals(event),
        "process": _process(event),
        "socket_names": _socket_names(event),
    }


_CASES = {
    "surface": _surface,
    "modules": _modules,
}


def handler(event, context):
    return _CASES[event["case"]](event)
"#;
