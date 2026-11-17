// SPDX-License-Identifier: CC0-1.0

//! What this registry judges about a `repositoryName` it is asked to RESOLVE a repository
//! by, and what it leaves to the lookup.
//!
//! `RepositoryName` is modelled `@length(min: 2, max: 256)` with the `@pattern`
//! `^[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(\/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*$`
//! (`smithy_query(query="shape", shape="RepositoryName")`). Whether that EXPRESSION is
//! policed on a request is ECR's own per-OPERATION fact rather than a property of the
//! value: the recorded answers for two calls of one client, each naming a repository the
//! expression does not admit, are
//!
//! * `ListImages` -> HTTP 400 `RepositoryNotFoundException` with this registry's not-found
//!   wording ([`super::repository_not_found_message`]) naming the name as the request
//!   spelled it — the name reached the lookup;
//! * `DescribeImages` -> HTTP 400 `InvalidParameterException` stating the expression ECR
//!   publishes — the name never reached one.
//!
//! So the boundary check is off `ListImages`'s name alone (`skip_validation =
//! ["repositoryName"]` in `src/ops/types/list_images_types.rs`), and every other
//! operation's — `CreateRepository`, which mints the name, and `DescribeImages`, which
//! resolves one — keeps the model's constraint as declared. The difference matters to every
//! caller that branches on the error class: the common "does this repository exist yet?"
//! probe, and every tool that passes a registry-qualified image reference where a
//! repository name belongs, takes the not-found branch only if the answer says not found.
//!
//! The name's own BOUNDS are not operation-specific, so they are judged here, at the one
//! place a name reaches a lookup
//! ([`super::ComAmazonawsEcrAmazonec2containerregistryV20150921State::repository`] and its
//! `_mut` twin): a name outside them is the parameter failure the boundary would have
//! stated, worded the same way, so nothing a caller sees for such a name changes.

use super::Fault;

/// `RepositoryName`'s modelled minimum length, from its `@length(min: 2, max: 256)`.
pub const REPOSITORY_NAME_MIN_LENGTH: i64 = 2;

/// `RepositoryName`'s modelled maximum length, from the same `@length`.
pub const REPOSITORY_NAME_MAX_LENGTH: i64 = 256;

/// The API label a repository name is carried as, which a refusal about one names.
const REPOSITORY_NAME_PARAMETER: &str = "repositoryName";

/// How long `name` is to the model's `@length`: the number of CHARACTERS, which is what the
/// model-driven walk counts for a string.
fn name_length(name: &str) -> i64 {
    i64::try_from(name.chars().count()).unwrap_or(i64::MAX)
}

/// Whether `name` is inside `RepositoryName`'s modelled length bounds — which is what
/// decides whether a name is looked up at all (see the module docs).
pub fn within_repository_name_length(name: &str) -> bool {
    (REPOSITORY_NAME_MIN_LENGTH..=REPOSITORY_NAME_MAX_LENGTH).contains(&name_length(name))
}

/// What this registry refuses about a `repositoryName` it is asked to resolve: the modelled
/// `@length`, and nothing else.
///
/// The clause states the parameter and the requirement the boundary states for that same
/// `@length`, derived from the violation kind through
/// `service_common::validation_message::requirement` — the tree's one derivation of a
/// constraint's requirement — so a name refused here cannot be worded differently from one
/// refused at the boundary.
pub fn check_repository_name(name: &str) -> Result<(), Fault> {
    if within_repository_name_length(name) {
        return Ok(());
    }
    let kind = if name_length(name) < REPOSITORY_NAME_MIN_LENGTH {
        ::smithy_server_runtime::ViolationKind::MinLength(REPOSITORY_NAME_MIN_LENGTH)
    } else {
        ::smithy_server_runtime::ViolationKind::MaxLength(REPOSITORY_NAME_MAX_LENGTH)
    };
    Err(super::invalid_parameter_at(
        REPOSITORY_NAME_PARAMETER,
        &::service_common::validation_message::requirement(kind),
    ))
}
