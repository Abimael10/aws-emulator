// SPDX-License-Identifier: CC0-1.0

//! The statuses this service's MODEL declares for an operation's errors — a shared reader,
//! not a suite of its own.
//!
//! A refusal of a request carries an error shape the refusing operation's own model declares
//! for it, so the status a client classifies the answer by is one of that operation's
//! declared statuses. Two suites check that rule — [`super::refresh_token_declared_statuses`]
//! on `RefreshToken`, and [`super::emulator_store_bound_refusals`] on every store this
//! emulator bounds on its own account — and both take the declared set from here rather than
//! from a list typed into a test, so neither can expect a status the shipped model does not
//! give the operation. `tests/all.rs` is one crate root, so the reader is written once.
//!
//! It reads one file, the model this crate ships and builds from; it starts no router,
//! writes nothing and reaches no network.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{json, Value};

/// This service's Smithy namespace, which prefixes every shape id in the model.
const NAMESPACE: &str = "com.amazonaws.amplifyuibuilder#";

/// The shipped Smithy model of this service, parsed once for the whole test target.
fn model() -> &'static Value {
    static MODEL: OnceLock<Value> = OnceLock::new();
    MODEL.get_or_init(|| {
        let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("smithy-model/com-amazonaws-amplifyuibuilder-amplifyuibuilder.json");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        serde_json::from_str(&text).expect("the shipped Smithy model is JSON")
    })
}

/// Every HTTP status the model declares for one operation: the `@httpError` of each shape in
/// that operation's `errors` list.
///
/// The operation is named by its short name, as the model's shape ids spell it after the
/// namespace. A model that declares the operation no errors answers an empty set, which is
/// an operation that may refuse nothing — not an operation free to answer any status.
pub(crate) fn declared_statuses(operation: &str) -> BTreeSet<u16> {
    let declared = &model()["shapes"][format!("{NAMESPACE}{operation}")];
    assert_eq!(
        declared["type"],
        json!("operation"),
        "case {operation:?}: the model must declare the operation, got {:?}",
        declared["type"]
    );
    declared["errors"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|error| {
            let target = error["target"].as_str().unwrap_or_else(|| {
                panic!("case {operation:?}: an error entry must name a target, got {error:?}")
            });
            modelled_status(target).unwrap_or_else(|| {
                panic!("case {operation:?}: {target} must carry an @httpError trait")
            })
        })
        .collect()
}

/// The status the model gives one error shape, named by its short name or its full shape id.
pub(crate) fn modelled_status(shape: &str) -> Option<u16> {
    let shape_id = if shape.starts_with(NAMESPACE) {
        shape.to_string()
    } else {
        format!("{NAMESPACE}{shape}")
    };
    model()["shapes"][shape_id]["traits"]["smithy.api#httpError"]
        .as_u64()
        .and_then(|status| u16::try_from(status).ok())
}
