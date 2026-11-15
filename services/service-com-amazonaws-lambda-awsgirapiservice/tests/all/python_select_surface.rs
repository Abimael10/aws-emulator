// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WAITING interface of the standard library a `python3.x`
//! function's run is given: `CreateFunction` then `Invoke` (`POST
//! /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: the Python a function runs on presents the standard library of the
//! runtime it advertises, so a handler module — or a client library the deployment package
//! vendors — whose body imports one of those modules IMPORTS CLEANLY and the invocation
//! answers the handler's own value with no `X-Amz-Function-Error`; a capability the runtime
//! cannot serve is refused by the CALL that needs it, with the error the system call it
//! stands for gives, and never by the import. For `select` that means the event bits, the
//! two multiplexers this platform has and the module's own error are the module's own, a
//! wait with nothing to wait for is served as the time it is, and a wait on a DESCRIPTOR —
//! which a function of this emulator holds none of — is refused where it is asked for:
//! `EBADF` from `select`, `POLLNVAL` from `poll`, `EBADF` from an `epoll` registration, and
//! the language's own `ValueError` for a negative descriptor.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what
//! the rule generalises over: which module of the standard library is imported, which
//! handler module name the package carries, which surface of `select` is read (names,
//! multiplexers, the default selector), which call waits and which waits on a descriptor,
//! how the descriptor is named (an integer, a socket of the runtime, a negative number, a
//! value that is no descriptor at all), and what the timeout is.

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

/// How long past a due instant this suite waits before calling. The transition is owed by
/// the CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` every function this suite creates carries, in seconds. Sized the way the
/// sibling suites that RUN handlers size theirs: it has to cover this test binary's own
/// scheduling, not the handlers' work, which is immediate.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// How many times a row re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 4;

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

/// What one case of the probe answered, or `None` when the run was ended by a bound this
/// whole test binary shares rather than by anything the case asked for.
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

/// The whole rule, driven END TO END as one test: every table below is a real
/// `CreateFunction` and real `Invoke` requests over the router.
///
/// One test and one stream of invocations, the way the sibling runtime suites are built: the
/// memory bound a run is stopped on is read from the WHOLE process
/// (`state::execution::python`) and this binary runs its suites in parallel, so fewer
/// interpreters in flight at once is fewer runs stopped for memory belonging to another
/// suite. A case whose run was stopped by one of those shared bounds says nothing about the
/// rule and is left out of the reading — but not every one of them can be: each table
/// answers how many of its cases a RUN answered, and a test that read none of them at all
/// is a test that asserted nothing, so it fails rather than passing silently.
#[tokio::test]
async fn the_python_runtime_presents_the_waiting_interface_of_its_standard_library() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let mut read = a_module_that_imports_the_waiting_interface_imports_cleanly(&router).await;
    let probe = "probe-select-surface";
    create_function(&router, probe, "lambda_function", PROBE_SOURCE).await;
    read += the_modules_names_are_the_runtimes_own(&router, probe).await;
    read += a_wait_with_nothing_to_wait_for_is_served(&router, probe).await;
    read += a_wait_on_a_descriptor_is_refused_where_it_is_asked_for(&router, probe).await;
    read += the_default_selector_is_the_platforms_own(&router, probe).await;
    read += every_standard_library_module_built_on_the_waiting_interface_imports(&router, probe)
        .await;
    assert!(
        read > 0,
        "expected at least one case of the tables above to be answered by a run, got \
         {read}: every invocation reported a bound this whole test binary shares, so nothing \
         of the rule was read"
    );
}

/// The recorded shape, and the generalisation of it: a handler module whose body imports the
/// waiting interface — or the standard library's own selector module built on it, which is
/// what a vendored client library imports — imports cleanly and its own value is the answer,
/// whatever the module is named and however it names the import.
///
/// The rows differ along the handler module's name, along which module the body imports, and
/// along the form of the import (the module, an alias, the selector module built on it).
///
/// Answers how many rows a run answered.
async fn a_module_that_imports_the_waiting_interface_imports_cleanly(
    router: &MultiServiceRouter,
) -> usize {
    // The case, the handler module's name, and the module body.
    let rows: &[(&str, &str, &str)] = &[
        (
            "the recorded probe's own shape: a vendored client library's imports",
            "index",
            "import select, selectors, socket, struct, threading\n\n\n\
             def handler(event, context):\n    return {'ok': True}\n",
        ),
        (
            "a module that imports it under a name of its own and waits in its own body",
            "kafka_probe",
            "import select as waiting\n\n\n_READY = waiting.select([], [], [], 0)\n\n\n\
             def handler(event, context):\n    \
                 return {'ok': _READY == ([], [], [])}\n",
        ),
        (
            "a module that builds the selector the standard library picks for the platform",
            "selector_probe",
            "import selectors\n\n\ndef handler(event, context):\n    \
                 with selectors.DefaultSelector() as selector:\n        \
                     return {'ok': selector.select(0) == []}\n",
        ),
    ];
    let mut read = 0_usize;
    for (case, module, source) in rows {
        let name = format!("probe-select-import-{module}");
        create_function(router, &name, module, source).await;
        let Some(document) = answered(router, &name, case, &json!({})).await else {
            continue;
        };
        read += 1;
        assert_eq!(
            document,
            json!({"ok": true}),
            "case {case}: expected the handler's own value, got {document}"
        );
    }
    read
}

/// The names a program reads off the module are the ones the standard library states.
///
/// Source of each expected value: the event bits `<poll.h>` and `<sys/epoll.h>` define on
/// this platform, as Python's own `select` module documents them, and `select.error`, which
/// is `OSError` in Python 3. The multiplexers are the ones this platform has — `poll` and
/// `epoll` — and the two it does not: `devpoll` is Solaris's and `kqueue` is BSD's, so a
/// runtime advertising a Linux `python3.x` must not carry them.
///
/// Answers how many cases a run answered.
async fn the_modules_names_are_the_runtimes_own(router: &MultiServiceRouter, probe: &str) -> usize {
    let case = "the module's names";
    let Some(document) = answered(router, probe, case, &json!({"case": "names"})).await else {
        return 0;
    };
    let expected = json!({
        "pollin": 1,
        "pollpri": 2,
        "pollout": 4,
        "pollerr": 8,
        "pollhup": 16,
        "pollnval": 32,
        "pollrdhup": 8192,
        "epollin": 1,
        "epollout": 4,
        "epollet": 2147483648_u32,
        "epolloneshot": 1073741824,
        "epollexclusive": 268435456,
        "pipe_buf": 4096,
        "error_is_os_error": true,
        "has": ["epoll", "poll", "select"],
        "has_not": ["devpoll", "kqueue"],
    });
    assert_eq!(
        document, expected,
        "case {case}: expected the standard library's own names, got {document}"
    );
    1
}

/// A wait with nothing to wait for is the time it is, so it is SERVED: every call answers
/// that nothing became ready.
///
/// The rows differ along which call waits (`select`, a `poll` object, an `epoll` object),
/// along the unit its timeout is stated in (seconds for `select` and `epoll`, milliseconds
/// for `poll`) and along the timeout itself. Nothing here reads a clock: what is asserted is
/// the ANSWER, not how long it took.
///
/// Answers how many cases a run answered.
async fn a_wait_with_nothing_to_wait_for_is_served(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    let case = "a wait with nothing to wait for";
    let Some(document) = answered(router, probe, case, &json!({"case": "waits"})).await else {
        return 0;
    };
    let expected = json!({
        "select_at_once": [[], [], []],
        "select_timed": [[], [], []],
        "select_over_tuples": [[], [], []],
        "poll_at_once": [],
        "poll_timed": [],
        "epoll_at_once": [],
        "epoll_timed": [],
        "epoll_in_a_context": [],
    });
    assert_eq!(
        document, expected,
        "case {case}: expected every wait to answer that nothing became ready, got {document}"
    );
    1
}

/// A wait on a DESCRIPTOR is refused where it is asked for, with the error the system call
/// it stands for gives — and never by the import.
///
/// The rows differ along which call is made, along how the descriptor is named (an integer,
/// a socket of this runtime, a negative number, a value that is no descriptor at all), along
/// which of the three sets of `select` holds it, and along what the timeout is.
///
/// Source of each expected refusal: the errors these calls raise in CPython — `OSError` with
/// `EBADF` for a `select` or an `epoll_ctl` naming a descriptor that is not open, `POLLNVAL`
/// in a `poll` answer for the same, `ValueError` for a negative descriptor and for a negative
/// `select` timeout, `TypeError` for a value that is no descriptor and for a set that is no
/// sequence, and `KeyError` from `poll.unregister` for a descriptor it does not hold.
///
/// Answers how many cases a run answered.
async fn a_wait_on_a_descriptor_is_refused_where_it_is_asked_for(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    let case = "a wait on a descriptor";
    let Some(document) = answered(router, probe, case, &json!({"case": "descriptors"})).await
    else {
        return 0;
    };
    let expected = json!({
        // `select` on a descriptor, whichever of the three sets names it: the descriptor is
        // not open, which is `EBADF`.
        "read_set": ["OSError", "EBADF"],
        "write_set": ["OSError", "EBADF"],
        "exception_set": ["OSError", "EBADF"],
        // A socket of this runtime holds no descriptor at all, and a negative descriptor is
        // the language's own error.
        "runtime_socket": ["ValueError", null],
        "negative": ["ValueError", null],
        // What is no descriptor, and what is no sequence of them.
        "not_a_descriptor": ["TypeError", null],
        "not_a_sequence": ["TypeError", null],
        // The timeout of a `select` is seconds, and a negative one is refused.
        "negative_timeout": ["ValueError", null],
        "unreadable_timeout": ["TypeError", null],
        // `poll` is told about a descriptor without being told to look at it, so it reports
        // the one that is not open as invalid, every registration of it, in order.
        "poll_one": [[3, 32]],
        "poll_two": [[3, 32], [4, 32]],
        "poll_modified": [[3, 32]],
        "poll_after_unregister": [],
        // `poll.modify` of a descriptor it does not hold is the `ENOENT` the system call
        // answers, which Python raises as that error number's own class.
        "poll_modify_unknown": ["FileNotFoundError", "ENOENT"],
        "poll_unregister_unknown": ["KeyError", null],
        "poll_negative": ["ValueError", null],
        // `epoll` is told which descriptor to watch while it registers one, so it refuses
        // there — and a closed one refuses everything.
        "epoll_register": ["OSError", "EBADF"],
        "epoll_modify": ["OSError", "EBADF"],
        "epoll_negative": ["ValueError", null],
        "epoll_closed": ["ValueError", null],
        "epoll_was_closed": true,
    });
    assert_eq!(
        document, expected,
        "case {case}: expected each call to answer for a descriptor that is not open, got \
         {document}"
    );
    1
}

/// The generalisation past `select` itself: the selector the standard library picks for this
/// platform is built out of the module, so a client library that multiplexes its connections
/// gets the platform's own selector — and that selector's own answers are the module's.
///
/// Source: `selectors.DefaultSelector`, which CPython's `selectors` resolves by asking which
/// multiplexer the platform has, in the order `kqueue`, `epoll`, `devpoll`, `poll`, `select`.
///
/// Answers how many cases a run answered.
async fn the_default_selector_is_the_platforms_own(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    let case = "the default selector";
    let Some(document) = answered(router, probe, case, &json!({"case": "selectors"})).await else {
        return 0;
    };
    let expected = json!({
        "default": "EpollSelector",
        "empty_select": [],
        "no_registration": {},
        // A socket of this runtime holds no descriptor for a selector to watch, and a
        // descriptor that is not open is refused by the multiplexer underneath.
        "register_socket": "ValueError",
        "register_descriptor": "OSError",
        // The names a library reads off the selector module.
        "event_read": 1,
        "event_write": 2,
    });
    assert_eq!(
        document, expected,
        "case {case}: expected the platform's own default selector, got {document}"
    );
    1
}

/// Every module of the standard library that is built on the waiting interface imports
/// cleanly too, because the interface is on the import path.
///
/// Source: the modules of the CPython standard library whose own body imports `select` or
/// the selector module built on it, and whose other imports this runtime registers — the
/// selector module itself, the socket interface it is used beside, and the server module
/// built on both.
///
/// Answers how many cases a run answered.
async fn every_standard_library_module_built_on_the_waiting_interface_imports(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    let case = "the modules built on the waiting interface";
    let modules = ["select", "selectors", "socket", "socketserver"];
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

/// The probe every table above drives: its module body imports the waiting interface exactly
/// as a vendored client library's does, and its handler answers the surface the event names.
const PROBE_SOURCE: &str = r#"
import errno
import importlib
import select
import selectors
import socket


def _refused(call):
    """The error `call` raised: its name and the NAME of its `errno`, or no error."""
    try:
        call()
    except BaseException as error:
        number = getattr(error, "errno", None)
        return [type(error).__name__, errno.errorcode.get(number)]
    return ["", None]


def _named(call):
    """The NAME of the error `call` raised, or an empty string when it raised none."""
    return _refused(call)[0]


def _names(event):
    multiplexers = ("devpoll", "epoll", "kqueue", "poll", "select")
    return {
        "pollin": int(select.POLLIN),
        "pollpri": int(select.POLLPRI),
        "pollout": int(select.POLLOUT),
        "pollerr": int(select.POLLERR),
        "pollhup": int(select.POLLHUP),
        "pollnval": int(select.POLLNVAL),
        "pollrdhup": int(select.POLLRDHUP),
        "epollin": int(select.EPOLLIN),
        "epollout": int(select.EPOLLOUT),
        "epollet": int(select.EPOLLET),
        "epolloneshot": int(select.EPOLLONESHOT),
        "epollexclusive": int(select.EPOLLEXCLUSIVE),
        "pipe_buf": int(select.PIPE_BUF),
        "error_is_os_error": select.error is OSError,
        "has": [name for name in multiplexers if hasattr(select, name)],
        "has_not": [name for name in multiplexers if not hasattr(select, name)],
    }


def _waits(event):
    poller = select.poll()
    epoller = select.epoll()
    with select.epoll() as scoped:
        in_a_context = scoped.poll(0)
    return {
        "select_at_once": select.select([], [], [], 0),
        "select_timed": select.select([], [], [], 0.001),
        "select_over_tuples": select.select((), (), (), 0.0),
        "poll_at_once": poller.poll(0),
        "poll_timed": poller.poll(1),
        "epoll_at_once": epoller.poll(0),
        "epoll_timed": epoller.poll(0.001),
        "epoll_in_a_context": in_a_context,
    }


def _descriptors(event):
    poller = select.poll()
    poller.register(3)
    one = poller.poll(0)
    poller.register(4, select.POLLOUT)
    two = poller.poll(0)
    poller.unregister(4)
    poller.modify(3, select.POLLIN | select.POLLOUT)
    modified = poller.poll(0)
    poller.unregister(3)
    emptied = poller.poll(0)
    epoller = select.epoll()
    register = _refused(lambda: epoller.register(3, select.EPOLLIN))
    modify = _refused(lambda: epoller.modify(3, select.EPOLLOUT))
    negative = _refused(lambda: epoller.register(-1))
    epoller.close()
    closed = _refused(lambda: epoller.poll(0))
    return {
        "read_set": _refused(lambda: select.select([3], [], [], 0)),
        "write_set": _refused(lambda: select.select([], [3], [], 0)),
        "exception_set": _refused(lambda: select.select([], [], [3], 0)),
        "runtime_socket": _refused(lambda: select.select([socket.socket()], [], [], 0)),
        "negative": _refused(lambda: select.select([-1], [], [], 0)),
        "not_a_descriptor": _refused(lambda: select.select([object()], [], [], 0)),
        "not_a_sequence": _refused(lambda: select.select(3, [], [], 0)),
        "negative_timeout": _refused(lambda: select.select([], [], [], -1)),
        "unreadable_timeout": _refused(lambda: select.select([], [], [], "soon")),
        "poll_one": one,
        "poll_two": two,
        "poll_modified": modified,
        "poll_after_unregister": emptied,
        "poll_modify_unknown": _refused(lambda: poller.modify(9, select.POLLIN)),
        "poll_unregister_unknown": _refused(lambda: poller.unregister(9)),
        "poll_negative": _refused(lambda: poller.register(-1)),
        "epoll_register": register,
        "epoll_modify": modify,
        "epoll_negative": negative,
        "epoll_closed": closed,
        "epoll_was_closed": epoller.closed,
    }


def _selectors(event):
    selector = selectors.DefaultSelector()
    empty = selector.select(0)
    registration = dict(selector.get_map())
    register_socket = _named(
        lambda: selector.register(socket.socket(), selectors.EVENT_READ)
    )
    register_descriptor = _named(lambda: selector.register(3, selectors.EVENT_READ))
    selector.close()
    return {
        "default": selectors.DefaultSelector.__name__,
        "empty_select": empty,
        "no_registration": registration,
        "register_socket": register_socket,
        "register_descriptor": register_descriptor,
        "event_read": int(selectors.EVENT_READ),
        "event_write": int(selectors.EVENT_WRITE),
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


_CASES = {
    "names": _names,
    "waits": _waits,
    "descriptors": _descriptors,
    "selectors": _selectors,
    "modules": _modules,
}


def handler(event, context):
    return _CASES[event["case"]](event)
"#;
