// SPDX-License-Identifier: CC0-1.0

//! The predefined WORKER TYPES this service accepts, and the clause a value outside them is
//! refused with.
//!
//! One statement of that set, because it belongs to the enumerated TYPE
//! (`com.amazonaws.glue#WorkerType`) and not to a member or an operation: every request that
//! names a worker type — a job definition's, a job run's, a session's, a machine learning
//! transform's — is judged against it and refused in one wording.
//!
//! WHICH VALUES the set holds is the only thing here that is not read off the model. The set
//! the live service accepts is LARGER than this checkout's model declares: the model's own
//! values are the generated, model-derived [`MODEL_DECLARED_WORKER_TYPES`], and the service
//! also runs the memory-optimised (`R.*`) and Graviton (`*.GRAVITON`) families, which a
//! recorded refusal of an unknown worker type enumerates beside them (corpus
//! `simparity-wire-aws-v1`, fixture `glue-gjwr-traj-hand-written-job-definition-refusals`
//! step `task#4`, where `CreateJob` with an unknown `WorkerType` is refused listing all 23).
//! So only the ADDED half is stated here, as data ([`ADDED_WORKER_TYPES`]), after the model's
//! own ([`accepted_worker_types`]) — and the members that carry a worker type are taken off
//! the boundary's value-set check (`skip_validation` on them in `src/ops/types/`) and judged
//! by the operation that read them, so the set a request is ADMITTED against and the set its
//! refusal ENUMERATES are the same one. A value the model gains later reaches this with the
//! model, and only has to be dropped from the added half.
//!
//! The ORDER the clause lists them in is not stated here either: it is the order the layer
//! in front of this service enumerates its own table of a value set in (`as_glue_clause`,
//! in this module's parent), which is computed from the value texts themselves.

use ::smithy_server_runtime::{Refusal, Violation, ViolationKind};

/// `com.amazonaws.glue#WorkerType` as this checkout's model declares it: the GENERATED,
/// model-derived value set, so a value the model gains reaches this set with the model and
/// nothing of the model is re-typed here.
pub const MODEL_DECLARED_WORKER_TYPES: &[&str] = crate::ops::types::WORKER_TYPE_VALUES;

/// The worker families the live service accepts and this checkout's model omits (see the
/// module note for the recording that enumerates them).
///
/// Each plain family comes before its Graviton variant: the order two values that share a
/// bucket of the stated set's own table are listed from follows the order they reach the
/// clause in, which is the model's own values first and then these.
pub const ADDED_WORKER_TYPES: &[&str] = &[
    "G.12X",
    "G.16X",
    "R.1X",
    "R.2X",
    "R.4X",
    "R.8X",
    "G.1X.GRAVITON",
    "G.2X.GRAVITON",
    "G.4X.GRAVITON",
    "G.8X.GRAVITON",
    "G.12X.GRAVITON",
    "G.16X.GRAVITON",
    "R.1X.GRAVITON",
    "R.2X.GRAVITON",
    "R.4X.GRAVITON",
    "R.8X.GRAVITON",
];

/// Every `WorkerType` this service accepts: the model's own ([`MODEL_DECLARED_WORKER_TYPES`])
/// and then the ones it adds ([`ADDED_WORKER_TYPES`]).
#[must_use]
pub fn accepted_worker_types() -> Vec<&'static str> {
    MODEL_DECLARED_WORKER_TYPES
        .iter()
        .chain(ADDED_WORKER_TYPES.iter())
        .copied()
        .collect()
}

/// Whether `value` names a worker type this service runs.
///
/// Compared exactly, as a value set is: an enumerated value differs from a value of another
/// case, and the recorded refusal of a worker type that differs only in case shows the
/// service refusing it rather than folding it.
#[must_use]
pub fn is_accepted_worker_type(value: &str) -> bool {
    MODEL_DECLARED_WORKER_TYPES.contains(&value) || ADDED_WORKER_TYPES.contains(&value)
}

/// The constraint refusal a worker type outside [`accepted_worker_types`] earns at
/// `member_path`, or `None` for a request that named none or named one this service runs.
///
/// AWS's own validation-error envelope over the one clause that fired, naming the value the
/// caller submitted, the member path it was sent at and the value set it had to come from —
/// built through this module's parent `glue_constraint_refusal`, exactly as the sibling
/// check [`super::hash_string_pattern_refusal`] is, so a clause this service states itself
/// reads like one the boundary states for a constraint it does check.
///
/// The value is quoted back under the ceiling every clause of this emulator quotes one under
/// (`service_common::validation::quoted_echo`), so no refusal is sized by the request; the
/// CHECK itself is always made against the bytes the caller sent.
#[must_use]
pub fn worker_type_refusal(member_path: &str, worker_type: Option<&str>) -> Option<String> {
    let value = worker_type?;
    if is_accepted_worker_type(value) {
        return None;
    }
    super::glue_constraint_refusal(&Refusal::Invalid(vec![Violation {
        path: member_path.to_string(),
        kind: ViolationKind::EnumValue(
            accepted_worker_types()
                .into_iter()
                .map(str::to_string)
                .collect(),
        ),
        value: Some(::service_common::validation::quoted_echo(value).into_owned()),
        sensitive: false,
    }]))
    .map(|refused| refused.message)
}
