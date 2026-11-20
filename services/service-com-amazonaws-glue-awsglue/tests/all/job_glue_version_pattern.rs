// SPDX-License-Identifier: CC0-1.0

//! The `GlueVersion` a job definition may carry, and the refusal of one outside it.
//!
//! Two rules are pinned here, each over a table of inputs:
//!
//! * a `GlueVersion` outside the FORM the member admits is refused at the request boundary
//!   with HTTP 400, naming the member in the service's own lowerCamel spelling and quoting
//!   the form it had to satisfy. The form is the `@pattern` of `GlueVersionString`, the shape
//!   `CreateJob$GlueVersion` and `JobUpdate$GlueVersion` target
//!   (`smithy_query(query="shape",shape="GlueVersionString")`): `^((\w+\.)+\w+|auto)$`, with
//!   `@length` 1..255. The refusal quotes that expression exactly as the model writes it,
//!   anchoring pair included, which is the expression this service publishes for that shape.
//! * a `GlueVersion` of that form which names no version the service RUNS is refused by the
//!   operation itself, with its modelled `InvalidInputException` naming the version asked
//!   for: the form admits any dotted word, while the versions of the Glue engine are a
//!   published, discrete set.
//!
//! The rows differ along what the rules generalise over: how many dotted components a
//! version has, whether it is the literal `auto`, which published version it is, and the
//! ways a value can miss the form (no dot at all, a trailing dot, a word that only starts
//! like `auto`, and the wrong case of it). The recorded cases — a create sending the dotless
//! `4`, and one sending a two-component version the service does not run — are one refused
//! row each.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::DEFAULT_GLUE_VERSION;

use crate::wire::{call, emulator, error_code, ok};

/// The role ARN every job definition below is created with.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/sim2real-glue-job-role";

/// The form a `GlueVersion` has to satisfy, as the member's `@pattern` spells it. A refusal
/// quotes exactly this text — the expression this service publishes for the shape — which is
/// what the expectation below compares against.
const GLUE_VERSION_PATTERN: &str = r"^((\w+\.)+\w+|auto)$";

/// The requirement clause a `GlueVersion` outside [`GLUE_VERSION_PATTERN`] breaks, as a
/// constraint clause states it.
fn stated_glue_version_requirement() -> String {
    format!("Member must satisfy regular expression pattern: {GLUE_VERSION_PATTERN}")
}

/// A `CreateJob` body carrying `glue_version`.
fn create_body(name: &str, glue_version: &str) -> Value {
    json!({
        "Name": name,
        "Role": ROLE_ARN,
        "Command": { "Name": "glueetl", "ScriptLocation": "s3://etl/x.py" },
        "GlueVersion": glue_version,
    })
}

/// An `UpdateJob` body submitting `glue_version` on the definition of `name`.
fn update_body(name: &str, glue_version: &str) -> Value {
    json!({
        "JobName": name,
        "JobUpdate": {
            "Role": ROLE_ARN,
            "Command": { "Name": "glueetl", "ScriptLocation": "s3://etl/x.py" },
            "GlueVersion": glue_version,
        },
    })
}

/// The wording of a failure body, whichever case the protocol spells the member in.
fn message_of(body: &Value) -> String {
    for member in ["message", "Message"] {
        if let Some(message) = body.get(member).and_then(Value::as_str) {
            return message.to_string();
        }
    }
    format!("<no message in {body}>")
}

/// A `GlueVersion` of the form the member admits AND of a version the service runs is stored
/// and read back — published versions, the version the service fills a definition in with,
/// and the literal `auto`.
#[tokio::test]
async fn a_glue_version_of_the_admitted_form_round_trips() {
    // Versions inside `^((\w+\.)+\w+|auto)$` that the service runs, including three the
    // issue did not cite (`auto`, the earliest published version, and the version a
    // definition naming none is given — so a definition read back and resubmitted unchanged
    // is still admitted).
    let admitted = ["4.0", "5.0", "5.1", "0.9", DEFAULT_GLUE_VERSION, "auto"];

    for (index, version) in admitted.iter().enumerate() {
        let router = emulator();
        let name = format!("version-{index}");
        ok(&router, "CreateJob", &create_body(&name, version)).await;

        let read = ok(&router, "GetJob", &json!({ "JobName": name })).await;
        assert_eq!(
            read["Job"]["GlueVersion"],
            json!(version),
            "case {version:?}: expected the stored Glue version read back, got {read}"
        );

        ok(&router, "UpdateJob", &update_body(&name, version)).await;
        let after_update = ok(&router, "GetJob", &json!({ "JobName": name })).await;
        assert_eq!(
            after_update["Job"]["GlueVersion"],
            json!(version),
            "case {version:?}: expected the updated Glue version read back, got {after_update}"
        );
    }
}

/// A `GlueVersion` outside that form is refused with 400, and the refusal names the member
/// as the service spells it and quotes the form the value had to satisfy — the same refusal
/// whichever operation carried the value, and nothing is stored.
#[tokio::test]
async fn a_glue_version_outside_the_admitted_form_is_refused() {
    // Values outside `^((\w+\.)+\w+|auto)$`: no dot at all (the recorded case), a trailing
    // dot, a word that only begins like the literal, and the wrong case of it.
    let refused = ["4", "4.", "automatic", "AUTO"];

    for (index, version) in refused.iter().enumerate() {
        let router = emulator();
        let name = format!("refused-{index}");
        // One stored definition under another name, so the update below addresses a job
        // that exists and the refusal is about the submitted version alone.
        ok(&router, "CreateJob", &create_body("stored", "4.0")).await;

        let requests: Vec<(&str, Value)> = vec![
            ("CreateJob", create_body(&name, version)),
            ("UpdateJob", update_body("stored", version)),
        ];

        for (operation, body) in &requests {
            let (status, answer) = call(&router, operation, body).await;
            assert_eq!(
                status, 400,
                "case {version:?} / {operation}: expected 400, got {status} with {answer}"
            );
            let message = message_of(&answer);
            assert!(
                message.contains("glueVersion"),
                "case {version:?} / {operation}: the refusal must name the member it refused, \
                 got {message:?} ({})",
                error_code(&answer)
            );
            assert!(
                message.contains(&stated_glue_version_requirement()),
                "case {version:?} / {operation}: the refusal must quote the form the value had \
                 to satisfy, {}, got {message:?}",
                stated_glue_version_requirement()
            );
        }

        // The refused create stored nothing, and the refused update left the stored
        // definition's version where the create put it.
        let (status, answer) = call(&router, "GetJob", &json!({ "JobName": name })).await;
        assert_eq!(
            status, 400,
            "case {version:?}: a refused create must store no job definition, got {status} \
             with {answer}"
        );
        let stored = ok(&router, "GetJob", &json!({ "JobName": "stored" })).await;
        assert_eq!(
            stored["Job"]["GlueVersion"],
            json!("4.0"),
            "case {version:?}: a refused update must leave the stored version alone, got \
             {stored}"
        );
    }
}

/// A `GlueVersion` of the admitted form that names no version the service runs is refused by
/// the operation itself — its modelled `InvalidInputException`, naming the version asked for
/// — on both writes that take a job definition, and nothing is stored.
#[tokio::test]
async fn a_glue_version_the_service_does_not_run_is_refused_by_the_operation() {
    // Values inside `^((\w+\.)+\w+|auto)$` that name no version of the engine: one past the
    // published versions (the recorded case), one BETWEEN two of them, a three-component
    // one, and a dotted word that is no version at all.
    let unavailable = ["9.9", "3.5", "3.9.1", "spark.three"];

    for (index, version) in unavailable.iter().enumerate() {
        let router = emulator();
        let name = format!("unavailable-{index}");
        ok(&router, "CreateJob", &create_body("stored", "4.0")).await;

        let requests: Vec<(&str, Value)> = vec![
            ("CreateJob", create_body(&name, version)),
            ("UpdateJob", update_body("stored", version)),
        ];

        for (operation, body) in &requests {
            let (status, answer) = call(&router, operation, body).await;
            assert_eq!(
                status, 400,
                "case {version:?} / {operation}: expected 400, got {status} with {answer}"
            );
            assert!(
                error_code(&answer).ends_with("InvalidInputException"),
                "case {version:?} / {operation}: expected InvalidInputException, got {} in \
                 {answer}",
                error_code(&answer)
            );
            assert_eq!(
                message_of(&answer),
                format!("Glue version: {version} is invalid"),
                "case {version:?} / {operation}: unexpected wording"
            );
        }

        // Nothing was stored, and the stored definition keeps the version its create set.
        let (status, answer) = call(&router, "GetJob", &json!({ "JobName": name })).await;
        assert_eq!(
            status, 400,
            "case {version:?}: a refused create must store no job definition, got {status} \
             with {answer}"
        );
        let stored = ok(&router, "GetJob", &json!({ "JobName": "stored" })).await;
        assert_eq!(
            stored["Job"]["GlueVersion"],
            json!("4.0"),
            "case {version:?}: a refused update must leave the stored version alone, got \
             {stored}"
        );
    }
}
