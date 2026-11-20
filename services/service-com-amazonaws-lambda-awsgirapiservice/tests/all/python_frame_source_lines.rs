// SPDX-License-Identifier: CC0-1.0

//! What ONE frame of a failed Python invocation's `stackTrace` is rendered as.
//!
//! The rule under test: a traceback frame of a module the execution environment holds is
//! its location line AND the SOURCE LINE at that location — the statement the frame is
//! standing on, indented under the location, exactly as the managed runtime renders a
//! traceback. A frame reduced to its location alone drops the one half of it that says WHAT
//! was running, which is what a client (or a state machine relaying the cause of a task
//! failure) reads a failed invocation by.
//!
//! The rows differ along what the rule generalises over: WHICH module of the package the
//! frame is in (the handler's own, another module it imports), HOW DEEP the stack is when
//! the failure is taken (raised in the handler, one call in, two calls in across a module
//! boundary), WHAT the offending statement is (a bare `raise`, a subscript of an absent key,
//! an arithmetic fault, a call of something that is not callable) and WHERE in the file it
//! sits (the first statement of a module's function, and one far down a longer file).
//!
//! Every row drives real HTTP requests through the router, and the source line each frame
//! must carry is read back out of the SUBMITTED module rather than written out here, so the
//! assertion is the rule and not one recorded traceback.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

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

/// The `Runtime` every row runs.
const PYTHON: &str = "python3.12";

/// The directory the deployment package is mounted at, which is the prefix of the file every
/// frame of the package's own modules names.
const TASK_ROOT: &str = "/var/task";

/// How long a poll waits for every created function to report the `Active` state the
/// provisioning window owes it. Generous on purpose: the loop ends on the CONDITION, and
/// this is only the deadline past which the emulator is considered broken.
const ACTIVE_DEADLINE: Duration = Duration::from_secs(60);

/// How long one look at a provisioning function waits before the next; the loop ends on the
/// condition, never on this.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How many times a call waits out one of the emulator's own host bounds before its answer
/// is read as the run's, and how long it pauses between those attempts. The pause is not
/// what the assertion turns on: an answer of the run's own is returned at once.
const SLOT_ATTEMPTS: usize = 6;
const SLOT_RETRY_PAUSE: Duration = Duration::from_millis(150);

/// One service mounted on its own engine, reached the way the production router reaches it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// What a dispatched request answered.
struct Answer {
    status: u16,
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

    /// The frames of the stack a Python run reports, under `stackTrace`.
    fn frames(&self) -> Vec<String> {
        self.json()
            .get("stackTrace")
            .and_then(Value::as_array)
            .map(|frames| {
                frames
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
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
        body,
    }
}

/// Create a function holding `entries`, configured with `handler`. The provisioning window
/// the create documents is then waited out by polling each function for the `Active` state
/// it owes ([`poll_until_active`]), so no row waits a fixed time.
async fn create_function(
    router: &MultiServiceRouter,
    name: &str,
    entries: &[(&str, &str)],
    handler: &str,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": PYTHON,
            "Handler": handler,
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(entries)),
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
}

/// Look at `name`'s configuration until it reports the `Active` state the provisioning
/// window owes it, or fail once [`ACTIVE_DEADLINE`] has passed.
///
/// Waits for the CONDITION rather than for a span, so a loaded or a fast host only changes
/// how often it looks.
async fn poll_until_active(router: &MultiServiceRouter, name: &str) {
    let deadline = std::time::Instant::now() + ACTIVE_DEADLINE;
    loop {
        let read = call(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &[],
        )
        .await;
        let state = read
            .json()
            .get("State")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if state == "Active" {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "function {name:?} never reached Active within {ACTIVE_DEADLINE:?}, last read \
             {} {}",
            read.status,
            read.text()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// `Invoke` on `name`, re-driven while the answer reports one of the emulator's own host
/// bounds rather than what the run came to.
async fn invoke(router: &MultiServiceRouter, name: &str) -> Answer {
    let uri = format!("{FUNCTIONS_URI}/{name}/invocations");
    let mut answered = call(router, "POST", &uri, b"{}").await;
    for _ in 1..SLOT_ATTEMPTS {
        if !is_host_bound(answered.status, &answered.text()) {
            break;
        }
        tokio::time::sleep(SLOT_RETRY_PAUSE).await;
        answered = call(router, "POST", &uri, b"{}").await;
    }
    answered
}

/// One row: the modules the package holds, the handler string, and how many frames of the
/// package's own modules the stack must reach at least.
struct FrameCase {
    case: &'static str,
    entries: Vec<(&'static str, &'static str)>,
    handler: &'static str,
    /// The least number of frames naming a file under the task root.
    least_package_frames: usize,
}

/// A module long enough that the offending statement is far from its first line, so a frame
/// whose source line was read off the wrong line of the file could not look right.
const DEEP_MODULE: &str = "\
# A module whose failing statement sits far down the file.
VALUES = {\"a\": 1}


def first():
    return VALUES[\"a\"]


def second():
    return first() + 1


def third():
    return second() * 2


def handler(event, context):
    third()
    raise RuntimeError(\"deep in the module\")
";

/// The rows.
fn frame_cases() -> Vec<FrameCase> {
    vec![
        FrameCase {
            case: "a bare raise in the handler itself",
            entries: vec![(
                "index.py",
                "def handler(event, context):\n    raise ValueError('probe unhandled failure')\n",
            )],
            handler: "index.handler",
            least_package_frames: 1,
        },
        FrameCase {
            case: "a subscript of a key the event does not carry",
            entries: vec![(
                "index.py",
                "def handler(event, context):\n    if event[\"status\"] == \"SUCCEEDED\":\n        \
                 return 1\n    return 0\n",
            )],
            handler: "index.handler",
            least_package_frames: 1,
        },
        FrameCase {
            case: "an arithmetic fault one call into the handler's own module",
            entries: vec![(
                "index.py",
                "def ratio(total):\n    return 100 / total\n\n\n\
                 def handler(event, context):\n    return ratio(0)\n",
            )],
            handler: "index.handler",
            least_package_frames: 2,
        },
        FrameCase {
            case: "a call of something that is not callable, across a module boundary",
            entries: vec![
                (
                    "index.py",
                    "from pkg import helper\n\n\n\
                     def handler(event, context):\n    return helper.run()\n",
                ),
                ("pkg/__init__.py", "\n"),
                (
                    "pkg/helper.py",
                    "TARGET = None\n\n\ndef run():\n    return TARGET()\n",
                ),
            ],
            handler: "index.handler",
            least_package_frames: 2,
        },
        FrameCase {
            case: "a raise far down a longer module, under three nested calls",
            entries: vec![("index.py", DEEP_MODULE)],
            handler: "index.handler",
            least_package_frames: 1,
        },
    ]
}

/// The lines of one rendered frame, its trailing newline dropped.
fn frame_lines(frame: &str) -> Vec<&str> {
    frame.strip_suffix('\n').unwrap_or(frame).split('\n').collect()
}

/// The file and the line number a frame's location line states, or `None` when the line is
/// not one.
fn located(line: &str) -> Option<(String, usize)> {
    let rest = line.strip_prefix("  File \"")?;
    let (file, rest) = rest.split_once("\", line ")?;
    let (number, _name) = rest.split_once(", in ")?;
    Some((file.to_string(), number.parse().ok()?))
}

#[tokio::test]
async fn a_package_frame_carries_the_source_line_it_stands_on() {
    let _stream = one_invocation_stream_at_a_time().await;
    // One engine for the whole table: every row's package is its own function on it, so the
    // provisioning window the creates document is waited out once.
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let cases = frame_cases();
    for (index, case) in cases.iter().enumerate() {
        create_function(
            &router,
            &format!("python-frame-source-{index}"),
            &case.entries,
            case.handler,
        )
        .await;
    }
    for index in 0..cases.len() {
        poll_until_active(&router, &format!("python-frame-source-{index}")).await;
    }

    for (index, case) in cases.iter().enumerate() {
        let name = format!("python-frame-source-{index}");

        // What the package holds, by the path a frame of it names.
        let sources: BTreeMap<String, &str> = case
            .entries
            .iter()
            .map(|(path, source)| (format!("{TASK_ROOT}/{path}"), *source))
            .collect();

        let answered = invoke(&router, &name).await;
        assert_eq!(
            answered.status,
            200,
            "case {:?}: a run that raised is still an HTTP 200, got {} {}",
            case.case,
            answered.status,
            answered.text()
        );
        let frames = answered.frames();
        assert!(
            !frames.is_empty(),
            "case {:?}: expected the raised error's own frames, got {}",
            case.case,
            answered.text()
        );

        let mut package_frames = 0usize;
        for (position, frame) in frames.iter().enumerate() {
            let lines = frame_lines(frame);
            let Some((file, number)) = located(lines[0]) else {
                panic!(
                    "case {:?}: frame {position} must open with its location, got {frame:?}",
                    case.case
                );
            };
            let Some(source) = sources.get(&file) else {
                // A frame of a module the package does not hold (the runtime's own) is
                // another suite's business.
                continue;
            };
            package_frames += 1;
            let statement = source
                .lines()
                .nth(number - 1)
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: frame {position} names line {number} of {file:?}, which the \
                         submitted module does not hold: {source:?}"
                    ,
                        case.case
                    )
                })
                .trim();
            assert_eq!(
                lines.len(),
                2,
                "case {:?}: frame {position} must be its location and the source line at it, \
                 got {frame:?}",
                case.case
            );
            assert_eq!(
                lines[1],
                format!("    {statement}"),
                "case {:?}: frame {position} must carry line {number} of {file:?} indented \
                 under its location, expected {:?}, got {:?}",
                case.case,
                format!("    {statement}"),
                lines[1]
            );
        }
        assert!(
            package_frames >= case.least_package_frames,
            "case {:?}: expected at least {} frames of the package's own modules, got {frames:?}",
            case.case,
            case.least_package_frames
        );
    }
}
