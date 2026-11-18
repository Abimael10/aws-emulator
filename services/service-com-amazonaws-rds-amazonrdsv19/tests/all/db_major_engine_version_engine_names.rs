// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the engine VOCABULARY `DescribeDBMajorEngineVersions` resolves its
//! `Engine` member against.
//!
//! `Engine` is "the database engine to return major version details for": it RESOLVES an
//! engine family rather than merely narrowing the rows, so a value that names no engine
//! this front door publishes names no family at all and is refused with the Query API's
//! `InvalidParameterValue` at HTTP 400 — not answered with the empty report that would
//! read as "this engine has no major versions". That vocabulary is the same one
//! `DescribeDBEngineVersions` resolves its own `Engine` against, so the two catalogue
//! reads cannot disagree about which names are engines, and it is shared here rather than
//! copied (`crate::db_engine_version_engine_selection`).
//!
//! The rules are asserted over TABLES rather than over one request: the positive table is
//! the whole published vocabulary — every engine of this model's own value lists plus the
//! two sibling control planes' — and the negative one spans the ways a name can look like
//! an engine of a family this service publishes without being one, in the case the request
//! spelled it and in another.

use crate::db_engine_version_engine_selection::{MODEL_ENGINE_VOCABULARY, NEAR_MISS_ENGINES};
use crate::db_instances::{call, emulator, expect_error, expect_ok, subtrees, text};

/// The operation under test.
const ACTION: &str = "DescribeDBMajorEngineVersions";

/// The Query API code this front door answers a value its member does not accept with, and
/// the `Error/Type` that classifies it as the caller's fault.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";
const SENDER: &str = "Sender";

/// The engines this one `rds` front door serves on behalf of ANOTHER control plane. They
/// are published engines — `docdb` and `neptune` are each their own model's `Engine` value
/// — so the member resolves them; this report publishes no family for either, which is an
/// empty report rather than a refusal.
const SIBLING_CONTROL_PLANE_ENGINES: [&str; 2] = ["docdb", "neptune"];

#[tokio::test]
async fn every_published_engine_resolves_and_is_served() {
    let router = emulator();

    for engine in MODEL_ENGINE_VOCABULARY
        .iter()
        .copied()
        .chain(SIBLING_CONTROL_PLANE_ENGINES)
    {
        let body = expect_ok(engine, call(&router, ACTION, &[("Engine", engine)]).await);
        assert!(
            body.contains("<DescribeDBMajorEngineVersionsResponse"),
            "case {engine:?}: expected the success envelope, got {body}"
        );
        assert!(
            !body.contains("<ErrorResponse"),
            "case {engine:?}: a published engine resolves, so it carries no error \
             envelope, got {body}"
        );
        // Whatever this report publishes for the engine, every entry it answered is that
        // engine's: the member resolved the family rather than selecting another's.
        for entry in subtrees(&body, "DBMajorEngineVersion") {
            let reported = text(&entry, "Engine");
            assert!(
                reported.eq_ignore_ascii_case(engine),
                "case {engine:?}: expected only this engine's families, got {reported:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_name_outside_the_published_vocabulary_is_refused() {
    let router = emulator();

    // The near-miss class, each row also sent in another case: the vocabulary is resolved
    // case-insensitively, so a name outside it is outside it however it is spelled, and
    // the refusal echoes the value as the REQUEST spelled it rather than a folded copy.
    let mut rows: Vec<(String, String)> = Vec::new();
    for (case, value) in NEAR_MISS_ENGINES {
        rows.push((format!("{case:?} ({value:?})"), (*value).to_string()));
        let shouted = value.to_ascii_uppercase();
        rows.push((format!("{case:?} in upper case ({shouted:?})"), shouted));
    }
    // Beside them, names of no engine family at all: a word, a name of the form a
    // user-supplied engine string takes, and a digit string.
    for value in ["not-a-real-engine", "postgressql", "aurora_mysql", "17"] {
        rows.push((format!("a value that is no engine ({value:?})"), value.to_string()));
    }

    for (case, value) in rows {
        let body = expect_error(
            &case,
            call(&router, ACTION, &[("Engine", value.as_str())]).await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let kind = text(&body, "Type");
        assert_eq!(
            kind, SENDER,
            "case {case}: expected the refusal classified {SENDER:?}, got {kind:?}"
        );
        let expected = format!("unrecognized engine name: {value}");
        let message = text(&body, "Message");
        assert_eq!(
            message, expected,
            "case {case}: expected {expected:?}, got {message:?}"
        );
        assert!(
            !body.contains("<DBMajorEngineVersion"),
            "case {case}: a refusal carries no family at all, got {body}"
        );
    }
}
