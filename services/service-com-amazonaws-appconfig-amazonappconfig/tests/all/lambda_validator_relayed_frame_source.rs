// SPDX-License-Identifier: CC0-1.0

//! Every frame of the error document a `LAMBDA` validator's refusal relays carries the
//! SOURCE STATEMENT it is standing on, not only the location of it.
//!
//! The rule under test: a validator "provides a syntactic or semantic check to ensure the
//! configuration that you want to deploy functions as intended … the configuration
//! deployment or update can only proceed when the configuration data is valid"
//! (`smithy_query(query="doc",shape="Validator")`), so for a `LAMBDA` validator the verdict
//! is the FUNCTION's and the call it gates is refused with `BadRequestException` / HTTP 400
//! (`smithy_query(query="operation",operation="StartDeployment,ValidateConfiguration")`)
//! whose `Message` relays the invocation's own answer — the `FunctionError` it signalled,
//! the `LogResult` it carried and the response payload, which for a run that raised is the
//! error document the function's runtime wrote. That document is relayed WHOLE, and a
//! traceback frame in it is the frame a rendered traceback is: its location line and the
//! source line at that location, indented under it. A frame reduced to its location drops
//! the statement that did the rejecting, which is the only account the caller gets of WHY
//! the configuration was refused beyond the exception's own message.
//!
//! The rows differ along what the rule generalises over: HOW DEEP the stack the validator
//! raised from is (the handler itself, one call in, two calls in), WHAT the offending
//! statement is (a raise the handler wrote, a raise in a helper, a subscript of a key the
//! document does not carry, an arithmetic fault), and WHERE in the module the statement
//! sits (near the top of a short module, and far down a longer one) — because the frame is
//! rendered from the deployed code and not from any one shape of failure.
//!
//! The statement each frame must carry is read back out of the module the row UPLOADED, at
//! the line the frame itself names, so nothing about the expected text is written here. Each
//! row also deploys a configuration the SAME validator finds compliant, so what is asserted
//! is the function's verdict on the content rather than the mere presence of a validator,
//! and reads the same relay through `ValidateConfiguration`, the other call the profile's
//! validators gate.

use serde_json::Value;

use crate::appconfig_workflow::{create_application, create_environment, Answer};
use crate::configuration_profile_validators::{
    assert_bad_request, emulator, given_profile, given_strategy, given_version, start_deployment,
    validate_configuration, validator,
};
use crate::deployment_tick_directive::given_function;

/// The fixed text of the relay: the two members of the `Invoke` answer that come before the
/// payload, as an answer to an invocation that asked for no log tail carries them.
const RELAYED_PREFIX: &str = "Function Error: Unhandled, Log Result: null, Payload: ";

/// The file a frame of the uploaded module names: the module name the package holds, under
/// the directory a deployment package is mounted at.
const HANDLER_FILE: &str = "/var/task/lambda_function.py";

/// The configuration document every row's validator finds compliant.
const COMPLIANT: &[u8] = br#"{"schema_version":2,"replicas":3}"#;

/// The configuration document every row's validator rejects: a document whose declared
/// version is not the one the validators below demand, carrying no `replicas` entry.
const NON_COMPLIANT: &[u8] = br#"{"schema_version":1}"#;

/// A validator that raises in the HANDLER itself, in a short module.
const RAISE_IN_HANDLER: &str = r#"import base64
import json


def lambda_handler(event, context):
    document = json.loads(base64.b64decode(event["content"]))
    if document.get("schema_version") == 2:
        return None
    raise Exception("schema_version must be 2, got %r" % (document.get("schema_version"),))
"#;

/// A validator that raises ONE call into its own module, so the relayed stack holds the
/// handler's frame and the helper's.
const RAISE_IN_HELPER: &str = r#"import base64
import json


def check(document):
    raise ValueError("schema_version %r is not supported" % (document.get("schema_version"),))


def lambda_handler(event, context):
    document = json.loads(base64.b64decode(event["content"]))
    if document.get("schema_version") == 2:
        return None
    return check(document)
"#;

/// A validator that fails on an OPERATION rather than on a statement it wrote to raise: the
/// subscript of a key the candidate document does not carry.
const SUBSCRIPT_OF_AN_ABSENT_KEY: &str = r#"import base64
import json


def lambda_handler(event, context):
    document = json.loads(base64.b64decode(event["content"]))
    if document.get("schema_version") == 2:
        return None
    return document["replicas"]
"#;

/// A validator whose fault is taken TWO calls in and far down a longer module, so a frame
/// whose source line was read off the wrong line of the file could not look right.
const ARITHMETIC_FAULT_DEEP_IN_A_MODULE: &str = r#"import base64
import json

# A module whose failing statements sit far from its first line.
LIMITS = {"replicas": 0}


def share(total):
    return 100 // total


def budget(document):
    return share(LIMITS["replicas"])


def lambda_handler(event, context):
    document = json.loads(base64.b64decode(event["content"]))
    if document.get("schema_version") == 2:
        return None
    return budget(document)
"#;

/// One row: the module the validator function runs, and how many frames of that module the
/// relayed stack must reach at least.
struct FrameCase {
    case: &'static str,
    name: &'static str,
    source: &'static str,
    /// The least number of frames naming the uploaded module.
    least_frames: usize,
}

/// The rows.
fn frame_cases() -> Vec<FrameCase> {
    vec![
        FrameCase {
            case: "a raise the handler itself wrote",
            name: "relayed-frame-handler-raise",
            source: RAISE_IN_HANDLER,
            least_frames: 1,
        },
        FrameCase {
            case: "a raise one call into the handler's module",
            name: "relayed-frame-helper-raise",
            source: RAISE_IN_HELPER,
            least_frames: 2,
        },
        FrameCase {
            case: "a subscript of a key the document does not carry",
            name: "relayed-frame-absent-key",
            source: SUBSCRIPT_OF_AN_ABSENT_KEY,
            least_frames: 1,
        },
        FrameCase {
            case: "an arithmetic fault two calls in, far down a longer module",
            name: "relayed-frame-deep-fault",
            source: ARITHMETIC_FAULT_DEEP_IN_A_MODULE,
            least_frames: 3,
        },
    ]
}

/// The lines of one rendered frame, its trailing newline dropped.
fn frame_lines(frame: &str) -> Vec<&str> {
    frame
        .strip_suffix('\n')
        .unwrap_or(frame)
        .split('\n')
        .collect()
}

/// The file and the line number a frame's location line states, or `None` when the line is
/// not one.
fn located(line: &str) -> Option<(String, usize)> {
    let rest = line.strip_prefix("  File \"")?;
    let (file, rest) = rest.split_once("\", line ")?;
    let (number, _name) = rest.split_once(", in ")?;
    Some((file.to_string(), number.parse().ok()?))
}

/// The payload a relayed refusal carries, as the document it is: the refusal is the modelled
/// 400, its message opens with the relay's fixed text, and what follows is the JSON error
/// document the function's runtime wrote.
fn relayed_payload(case: &str, answer: &Answer) -> Value {
    assert_bad_request(case, answer);
    let message = answer.string("Message").unwrap_or_default();
    assert!(
        message.starts_with(RELAYED_PREFIX),
        "case {case:?}: expected a refusal opening {RELAYED_PREFIX:?}, got {message:?}"
    );
    let payload = &message[RELAYED_PREFIX.len()..];
    serde_json::from_str(payload).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: expected the relayed payload to be the runtime's error document, \
             got {payload:?} ({error})"
        )
    })
}

/// Assert every frame of `payload`'s stack that names the uploaded module is that frame's
/// location AND the statement at the line it names, read out of `source`.
fn assert_frames_carry_their_source(case: &str, payload: &Value, source: &str, least: usize) {
    let frames: Vec<String> = payload["stackTrace"]
        .as_array()
        .map(|frames| {
            frames
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !frames.is_empty(),
        "case {case:?}: the relayed document carries the handler's own stack, got {payload}"
    );

    let mut module_frames = 0usize;
    for (position, frame) in frames.iter().enumerate() {
        let lines = frame_lines(frame);
        let Some((file, number)) = located(lines[0]) else {
            panic!("case {case:?}: frame {position} must open with its location, got {frame:?}");
        };
        if file != HANDLER_FILE {
            // A frame of a module the package does not hold (the runtime's own) is another
            // suite's business.
            continue;
        }
        module_frames += 1;
        let statement = source
            .lines()
            .nth(number - 1)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: frame {position} names line {number} of {file:?}, which the \
                     uploaded module does not hold: {source:?}"
                )
            })
            .trim();
        assert_eq!(
            lines.len(),
            2,
            "case {case:?}: frame {position} must be its location and the source line at it, \
             got {frame:?}"
        );
        assert_eq!(
            lines[1],
            format!("    {statement}"),
            "case {case:?}: frame {position} must carry line {number} of {file:?} indented under \
             its location, expected {:?}, got {:?}",
            format!("    {statement}"),
            lines[1]
        );
    }
    assert!(
        module_frames >= least,
        "case {case:?}: expected at least {least} frames of the validator's own module, got \
         {frames:?}"
    );
}

/// RULE: a refusal relaying a `LAMBDA` validator's answer carries that answer's traceback
/// frames whole — each the location and the source statement at it — whatever the failure
/// was and however deep in the deployed code it was taken; and the same function lets a
/// configuration it finds compliant through.
#[tokio::test]
async fn a_relayed_frame_carries_the_source_line_it_stands_on() {
    let (router, engine) = emulator();
    let application = create_application(&router, "relayed-frame-source").await;
    let strategy = given_strategy(&router, "relayed-frame-source-instant").await;

    for (index, row) in frame_cases().into_iter().enumerate() {
        let function = given_function(&router, &engine, row.case, row.name, row.source).await;
        let profile = given_profile(
            &router,
            &application,
            &format!("relayed-frame-{index}"),
            &[validator("LAMBDA", &function)],
        )
        .await;
        let refused_version =
            given_version(&router, &application, &profile, row.case, NON_COMPLIANT).await;
        let allowed_version =
            given_version(&router, &application, &profile, row.case, COMPLIANT).await;
        let environment =
            create_environment(&router, &application, &format!("relayed-frame-{index}")).await;

        // The deployment the validator refuses: the relayed document's frames.
        let refused = start_deployment(
            &router,
            &application,
            &environment,
            &profile,
            &strategy,
            &refused_version,
        )
        .await;
        let payload = relayed_payload(row.case, &refused);
        assert!(
            payload["errorType"]
                .as_str()
                .is_some_and(|kind| !kind.is_empty()),
            "case {:?}: the relayed document names the class the run reported, got {payload}",
            row.case
        );
        assert_frames_carry_their_source(row.case, &payload, row.source, row.least_frames);

        // `ValidateConfiguration` reads the same seam, so it relays the same document — and
        // its frames are the frames of its own run.
        let reported =
            validate_configuration(&router, &application, &profile, &refused_version).await;
        let reported_payload = relayed_payload(row.case, &reported);
        assert_eq!(
            reported_payload["errorType"], payload["errorType"],
            "case {:?}: ValidateConfiguration relays the same verdict, got {reported_payload}",
            row.case
        );
        assert_frames_carry_their_source(
            row.case,
            &reported_payload,
            row.source,
            row.least_frames,
        );

        // The same validator, the same gate, a configuration it finds compliant: the verdict
        // is about the content and not about the validator being there.
        let accepted = start_deployment(
            &router,
            &application,
            &environment,
            &profile,
            &strategy,
            &allowed_version,
        )
        .await;
        assert_eq!(
            accepted.status,
            201,
            "case {:?}: the validator found this content compliant, so the deployment must \
             proceed, got {} / {}",
            row.case,
            accepted.status,
            String::from_utf8_lossy(&accepted.body)
        );
    }
}
