// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the EXECUTION ENVIRONMENT a function's provisioning stands up:
//! `CreateFunction`, `GetFunctionConfiguration` polled until the function reports `Active`,
//! then `Invoke` (`POST /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: building an execution environment is two kinds of work, and only one
//! of them has to wait for a function. The first reads no function's configuration at all —
//! the interpreter, the host surface it is narrowed to and this emulator's own runtime — and
//! it is stood up while the function is being PROVISIONED, which is the window
//! `CreateFunction` documents ("When you create a function, Lambda provisions an instance of
//! the function and its supporting resources") and the function reports a non-terminal
//! `State` in. The second is one function's own: what the environment IS, the variables its
//! invocations run with, how deep their calls may nest, which release of the language its
//! runtime names, and the import of the handler's own module — the INIT phase, which the
//! first invocation ADOPTS the standing environment to run. So a function's first invocation
//! pays its own initialization and not the build of an interpreter, and what a run observes
//! is the same either way.
//!
//! The rows differ along what the rule generalises over: the `Runtime` identifier the release
//! is read from (three of the `python3.x` identifiers the model carries), the
//! `Architectures` entry the machine is read from (both the model's values), the environment
//! variables the invocation runs with (including one whose value is empty), and whether the
//! handler's own module does work while it is being read — which is the part of the
//! initialization that belongs to the function.
//!
//! What is NOT asserted here, because it is asserted elsewhere:
//! `crate::environment_setup_span` pins that the span spent standing an environment up is
//! charged to neither of a run's own spans, and `crate::invocation_init_phase` what the INIT
//! phase bounds.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::warm_pool;
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

/// The `State` a function settles in, from the model's `State` enum — the state a client
/// waits for before invoking.
const STATE_ACTIVE: &str = "Active";

/// How long a poll may look for its condition before the case gives up — generous, so a
/// loaded host cannot turn a condition that holds into a failure. Every poll here waits for
/// the CONDITION and never for a fixed span.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How many times a row re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 4;

/// The field the REPORT of a run that ran its own INIT phase states that phase's span in.
const INIT_DURATION_FIELD: &str = "Init Duration:";

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

    /// The log tail this invocation was answered with, decoded.
    fn log_tail(&self) -> String {
        self.header("x-amz-log-result")
            .and_then(|tail| base64::engine::general_purpose::STANDARD.decode(tail).ok())
            .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
            .unwrap_or_default()
    }
}

/// One service mounted on its own engine, reached the way the production router reaches it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// Dispatch one request, with the headers `headers` names.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
    headers: &[(&str, &str)],
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.us-east-1.amazonaws.com{uri}"))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
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

/// One row: the case, the function's name, its `Runtime`, its `Architectures` entry, the
/// value of the variable its invocations read, and the module the handler lives in.
struct Row {
    case: &'static str,
    name: &'static str,
    runtime: &'static str,
    architecture: &'static str,
    greeting: &'static str,
    module: &'static str,
}

/// A handler whose module does its own work while it is being READ: the part of the
/// initialization that belongs to the function rather than to the environment.
const WORKS_WHILE_IT_IMPORTS: &str = "import json\n\
     import os\n\
     import sys\n\
     \n\
     READY = json.dumps({'imported': True})\n\
     \n\
     def lambda_handler(event, context):\n    \
         return {\n        \
             'greeting': os.environ.get('GREETING'),\n        \
             'machine': os.uname().machine,\n        \
             'release': list(sys.version_info[:2]),\n        \
             'implementation': sys.implementation.name,\n        \
             'recursion': sys.getrecursionlimit(),\n        \
             'ready': READY,\n        \
             'stream': bool(os.environ.get('AWS_LAMBDA_LOG_STREAM_NAME')),\n        \
         }\n";

/// The same handler behind a module that does nothing at all while it is read.
const IMPORTS_WITHOUT_WORK: &str = "import os\n\
     import sys\n\
     \n\
     def lambda_handler(event, context):\n    \
         return {\n        \
             'greeting': os.environ.get('GREETING'),\n        \
             'machine': os.uname().machine,\n        \
             'release': list(sys.version_info[:2]),\n        \
             'implementation': sys.implementation.name,\n        \
             'recursion': sys.getrecursionlimit(),\n        \
             'ready': 'none',\n        \
             'stream': bool(os.environ.get('AWS_LAMBDA_LOG_STREAM_NAME')),\n        \
         }\n";

/// Whether an execution environment STANDS READY for the next invocation of this process:
/// one is waiting, or one has been adopted since `adopted_before` — which is what becomes of
/// a waiting one.
///
/// Readings of this module's own pool, like the count of runs it is holding past their budget
/// is ([`warm_pool::standing_blanks`], [`warm_pool::adopted_blanks`]). The pool is the whole
/// process's, and so is what every suite of this target provisions into it, which is why the
/// reading is "one is ready" rather than a count of its own.
fn stands_ready(adopted_before: u64) -> bool {
    warm_pool::standing_blanks() > 0 || warm_pool::adopted_blanks() > adopted_before
}

/// Create the function `row` describes.
async fn create_function(router: &MultiServiceRouter, row: &Row) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": row.name,
            "Role": ROLE,
            "Runtime": row.runtime,
            "Handler": "lambda_function.lambda_handler",
            "Architectures": [row.architecture],
            "Environment": { "Variables": { "GREETING": row.greeting } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", row.module)])),
            },
        }))
        .expect("the create body serializes"),
        &[],
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {:?}: CreateFunction must succeed, got {} {}",
        row.case,
        created.status,
        created.text()
    );
}

/// Read the configuration of `name` until it reports [`STATE_ACTIVE`] — the condition a client
/// waits for before invoking, polled rather than slept out.
async fn settled(router: &MultiServiceRouter, case: &str, name: &str) {
    let give_up = Instant::now() + POLL_DEADLINE;
    loop {
        let read = call(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &[],
            &[],
        )
        .await;
        let state = read
            .json()
            .get("State")
            .and_then(Value::as_str)
            .map(str::to_string);
        if state.as_deref() == Some(STATE_ACTIVE) {
            return;
        }
        assert!(
            Instant::now() < give_up,
            "case {case:?}: expected {name:?} to report State {STATE_ACTIVE:?}, got {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until an execution environment stands ready for the invocation this row is about to
/// make — the environment the provisioning of the function just created asked for.
async fn stands_ready_for(case: &str, adopted_before: u64) {
    let give_up = Instant::now() + POLL_DEADLINE;
    loop {
        if stands_ready(adopted_before) {
            return;
        }
        assert!(
            Instant::now() < give_up,
            "case {case:?}: expected the provisioning of the function to leave an execution \
             environment standing for its first invocation, got none waiting and none adopted \
             past {adopted_before}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One `Invoke` request on `name`, asking for the log tail.
async fn invoke_once(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        b"{}",
        &[("x-amz-log-type", "Tail")],
    )
    .await
}

/// `Invoke` on `name`, re-driven while the answer reports one of this process's own HOST
/// BOUNDS instead of a run of the handler ([`is_host_bound`]), neither of which says anything
/// about the rule.
async fn invoke(router: &MultiServiceRouter, name: &str) -> Answer {
    let mut answered = invoke_once(router, name).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answered.status, &answered.text()) {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        answered = invoke_once(router, name).await;
    }
    answered
}

/// Remove the function `name`, so nothing of one row is left standing for the next.
async fn delete_function(router: &MultiServiceRouter, case: &str, name: &str) {
    let deleted = call(router, "DELETE", &format!("{FUNCTIONS_URI}/{name}"), &[], &[]).await;
    assert_eq!(
        deleted.status, 204,
        "case {case:?}: the function must be removable between rows, got {} {}",
        deleted.status,
        deleted.text()
    );
}

/// The release a `python3.x` identifier names, as the two numbers a Python program reads off
/// `sys.version_info`.
fn release_of(case: &str, runtime: &str) -> Value {
    let (major, minor) = runtime
        .trim_start_matches("python")
        .split_once('.')
        .unwrap_or_else(|| panic!("case {case:?}: the row's runtime names a release"));
    json!([
        major.parse::<i64>().expect("the major version"),
        minor.parse::<i64>().expect("the minor version")
    ])
}

/// The machine name a POSIX `uname` gives an `Architectures` entry: `x86_64` by that same
/// name, and `arm64` the name the Linux kernel gives that instruction set.
fn machine_of(architecture: &str) -> &'static str {
    match architecture {
        "arm64" => "aarch64",
        _ => "x86_64",
    }
}

/// RULE: the half of an execution environment that reads no function's configuration is stood
/// up while the function is being provisioned, and the first invocation of that function
/// adopts it — applying its own function's half and running its own initialization in it, so
/// what the run observes is the function's own environment.
///
/// One test and one stream of invocations, the way the sibling suites that RUN handlers are
/// built: this emulator serves a bounded number of runs at once for the whole process. A row
/// whose answer reports a bound this binary shares says nothing about the rule and is left
/// out of the reading, and a test that read no row at all fails rather than passing silently.
#[tokio::test]
async fn a_functions_provisioning_stands_the_environment_its_first_invocation_adopts_up() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let rows: &[Row] = &[
        Row {
            case: "a python3.12 function on x86_64 whose module works while it is read",
            name: "provisioned-twelve",
            runtime: "python3.12",
            architecture: "x86_64",
            greeting: "hello from the module that works",
            module: WORKS_WHILE_IT_IMPORTS,
        },
        // Another release of the language, the other instruction set, and a module that does
        // nothing while it is read: everything the function's own half of the build states.
        Row {
            case: "a python3.13 function on arm64 whose module does no work while it is read",
            name: "provisioned-thirteen",
            runtime: "python3.13",
            architecture: "arm64",
            greeting: "hola",
            module: IMPORTS_WITHOUT_WORK,
        },
        // A variable whose value is EMPTY, which is a value like any other: the environment
        // the handler reads is the one its function states.
        Row {
            case: "a python3.11 function whose variable has an empty value",
            name: "provisioned-eleven",
            runtime: "python3.11",
            architecture: "x86_64",
            greeting: "",
            module: WORKS_WHILE_IT_IMPORTS,
        },
    ];
    let mut read = 0_usize;
    let mut limits: Vec<(&str, i64)> = Vec::new();
    for row in rows {
        let case = row.case;
        let adopted_before = warm_pool::adopted_blanks();
        create_function(&router, row).await;
        // The provisioning window the create opened is what the environment is stood up in,
        // so one stands ready before the function reports `Active`.
        stands_ready_for(case, adopted_before).await;
        settled(&router, case, row.name).await;

        let answered = invoke(&router, row.name).await;
        if is_host_bound(answered.status, &answered.text()) {
            delete_function(&router, case, row.name).await;
            continue;
        }
        read += 1;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: the first invocation of the function must be answered, got {} {}",
            answered.status,
            answered.text()
        );
        let failure = answered.header("x-amz-function-error");
        assert_eq!(
            failure, None,
            "case {case:?}: the handler returns its own document, so the invocation carries no \
             function error, got {failure:?} with {}",
            answered.text()
        );
        // The invocation ADOPTED an environment that was already standing rather than
        // building one on its own path: this process has adopted one more than it had.
        let adopted = warm_pool::adopted_blanks();
        assert!(
            adopted > adopted_before,
            "case {case:?}: expected the first invocation to adopt an execution environment \
             that was already standing, so more than {adopted_before} to have been adopted, \
             got {adopted}"
        );
        // And the environment it adopted is its own FUNCTION's: every member of the half of
        // the build that one function's configuration states is in place in it.
        let document = answered.json();
        for (member, expected) in [
            ("greeting", json!(row.greeting)),
            ("machine", json!(machine_of(row.architecture))),
            ("release", release_of(case, row.runtime)),
            // A managed `python3.x` runtime is a CPython release, which is the
            // implementation a handler of such a function observes.
            ("implementation", json!("cpython")),
            // The runtime's own variables are installed beside the function's own.
            ("stream", json!(true)),
        ] {
            let actual = document.get(member).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the run to report {member} as {expected}, got {actual} \
                 in {document}"
            );
        }
        // Its own module was imported in it, which is the INIT phase the invocation that
        // adopts an environment runs — and the report of that run states that phase.
        let ready = document.get("ready").and_then(Value::as_str);
        assert!(
            ready.is_some_and(|ready| !ready.is_empty()),
            "case {case:?}: expected the handler's own module to have been imported in the \
             environment, got {ready:?} in {document}"
        );
        let tail = answered.log_tail();
        assert!(
            tail.contains(INIT_DURATION_FIELD),
            "case {case:?}: expected the report of the first invocation of a function to state \
             {INIT_DURATION_FIELD:?}, got {tail:?}"
        );
        // How deep the calls of a run may nest is clamped by the HOST when the environment is
        // adopted, so every row reads the same ceiling — whatever the interpreter's own
        // default is.
        let recursion = document
            .get("recursion")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        assert!(
            recursion > 0,
            "case {case:?}: expected the run to report the recursion ceiling the host clamped \
             it to, got {recursion} in {document}"
        );
        limits.push((case, recursion));

        // The same environment serves the invocation after it, and answers it the same
        // document: nothing of the function's own half is lost by being applied to an
        // environment that was standing.
        let again = invoke(&router, row.name).await;
        if !is_host_bound(again.status, &again.text()) {
            assert_eq!(
                again.json(),
                document,
                "case {case:?}: expected the invocation after it to be answered the same \
                 document, got {} and {}",
                again.json(),
                document
            );
        }
        delete_function(&router, case, row.name).await;
    }
    assert!(
        read > 0,
        "no row of the table was answered by a run at all: every one was ended by this \
         process's own host bounds, so the rule was never observed"
    );
    let differing: Vec<&(&str, i64)> = limits
        .iter()
        .filter(|(_, limit)| Some(*limit) != limits.first().map(|(_, first)| *first))
        .collect();
    assert!(
        differing.is_empty(),
        "expected every row's run to report the one recursion ceiling the host clamps a run \
         to, got {differing:?} against {:?}",
        limits.first()
    );
}

/// What one update row changes about the function, as the request body it sends to the
/// operation at `suffix`.
struct Updated {
    case: &'static str,
    suffix: &'static str,
    body: Value,
    /// What the run answers as its `greeting` once the update has been applied.
    greeting: &'static str,
}

/// Read the configuration of `name` until no update is in progress on it — the condition a
/// client waits for before using an updated function ("The LastUpdateStatus […] fields in the
/// response from GetFunctionConfiguration indicate when the update is complete"), polled
/// rather than slept out.
async fn update_applied(router: &MultiServiceRouter, case: &str, name: &str) {
    let give_up = Instant::now() + POLL_DEADLINE;
    loop {
        let read = call(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &[],
            &[],
        )
        .await;
        let status = read
            .json()
            .get("LastUpdateStatus")
            .and_then(Value::as_str)
            .map(str::to_string);
        if status.as_deref() == Some(LAST_UPDATE_STATUS_SUCCESSFUL) {
            return;
        }
        assert!(
            Instant::now() < give_up,
            "case {case:?}: expected the update of {name:?} to be applied, so \
             LastUpdateStatus {LAST_UPDATE_STATUS_SUCCESSFUL:?}, got {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `LastUpdateStatus` of a function whose update has been applied, from the model's
/// `LastUpdateStatus` enum.
const LAST_UPDATE_STATUS_SUCCESSFUL: &str = "Successful";

/// RULE, read through the OTHER window a function is provisioned in: an accepted update to a
/// function's code or configuration opens one ("When you update a function, Lambda provisions
/// an instance of the function and its supporting resources") and mints a new revision, so no
/// environment built before it serves an invocation made after it — and the environment that
/// invocation needs is stood up in that window, exactly as a create's is. The run that adopts
/// it reads the configuration the update stored.
#[tokio::test]
async fn an_updated_functions_next_invocation_adopts_a_standing_environment() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "provisioned-updated";
    let created = Row {
        case: "before any update",
        name,
        runtime: "python3.12",
        architecture: "x86_64",
        greeting: "before",
        module: WORKS_WHILE_IT_IMPORTS,
    };
    create_function(&router, &created).await;
    settled(&router, created.case, name).await;
    // The rows are one per operation that provisions an instance of an existing function: its
    // code, and its configuration.
    let rows: &[Updated] = &[
        Updated {
            case: "an accepted code update",
            suffix: "code",
            body: json!({
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", IMPORTS_WITHOUT_WORK)])),
            }),
            greeting: "before",
        },
        Updated {
            case: "an accepted configuration update",
            suffix: "configuration",
            body: json!({ "Environment": { "Variables": { "GREETING": "after" } } }),
            greeting: "after",
        },
    ];
    let mut read = 0_usize;
    for row in rows {
        let case = row.case;
        let adopted_before = warm_pool::adopted_blanks();
        let updated = call(
            &router,
            "PUT",
            &format!("{FUNCTIONS_URI}/{name}/{}", row.suffix),
            &serde_json::to_vec(&row.body).expect("the update body serializes"),
            &[],
        )
        .await;
        assert_eq!(
            updated.status,
            200,
            "case {case:?}: the update must be accepted, got {} {}",
            updated.status,
            updated.text()
        );
        // The window the update opened is what the environment of the new revision is stood up
        // in, so one stands ready before the function is usable again.
        stands_ready_for(case, adopted_before).await;
        update_applied(&router, case, name).await;

        let answered = invoke(&router, name).await;
        if is_host_bound(answered.status, &answered.text()) {
            continue;
        }
        read += 1;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: the first invocation after the update must be answered, got {} {}",
            answered.status,
            answered.text()
        );
        // It adopted an environment that was already standing — the update's own revision has
        // none of its own standing before it — and it reads what the update stored.
        let adopted = warm_pool::adopted_blanks();
        assert!(
            adopted > adopted_before,
            "case {case:?}: expected the first invocation after the update to adopt an \
             execution environment that was already standing, so more than {adopted_before} \
             to have been adopted, got {adopted}"
        );
        let document = answered.json();
        let greeting = document.get("greeting").cloned().unwrap_or(Value::Null);
        assert_eq!(
            greeting,
            json!(row.greeting),
            "case {case:?}: expected the run to report the environment the update left, got \
             {greeting} in {document}"
        );
        // The code update replaced the module with the one that does no work while it is read
        // (`IMPORTS_WITHOUT_WORK`, whose `ready` is the fixed "none"), so a run that reports
        // that is a run of the module the function now serves rather than the one it was
        // created with — and the configuration update after it leaves that module in place.
        let ready = document.get("ready").and_then(Value::as_str);
        assert_eq!(
            ready,
            Some("none"),
            "case {case:?}: expected the run to report the module the function now serves, got \
             {ready:?} in {document}"
        );
    }
    assert!(
        read > 0,
        "no row of the table was answered by a run at all: every one was ended by this \
         process's own host bounds, so the rule was never observed"
    );
    delete_function(&router, "the updated function", name).await;
}
