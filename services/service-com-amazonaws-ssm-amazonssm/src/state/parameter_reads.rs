// SPDX-License-Identifier: CC0-1.0

//! How a parameter read is BOUNDED and SCOPED: the page size each of the paged parameter
//! reads serves, and the hierarchy a `Path` names.
//!
//! One concern — what one read may walk — shared by `GetParametersByPath`,
//! `DescribeParameters` and `GetParameterHistory`, so the modelled `MaxResults` ranges and
//! the reading of a hierarchy path are held in one place. Every name stays reachable as
//! `crate::state::<name>` through [`super`]'s re-export.

use super::*;

/// The bounds the modelled `GetParametersByPathMaxResults` carries (range 1..10).
pub const MIN_PARAMETERS_BY_PATH_PAGE_SIZE: i32 = 1;

/// See [`MIN_PARAMETERS_BY_PATH_PAGE_SIZE`]. It is also the page size a request that
/// names no `MaxResults` gets: "If you specify `MaxResults` in the request, the response
/// includes information up to the limit specified" (`GetParametersByPath`'s doc), so a
/// request naming none is served up to the modelled maximum.
pub const MAX_PARAMETERS_BY_PATH_PAGE_SIZE: i32 = 10;

/// The page size a read bounded by `MaxResults` serves, or the refusal message for one
/// outside the modelled range — which the protocol's boundary validator already refuses,
/// so this is a backstop rather than the first line of defence.
///
/// A request naming no `MaxResults` is served up to `maximum`: "the maximum number of
/// items to return for this call" bounds a page the caller asked to be bounded, so a
/// caller asking for no bound is served the whole page the operation allows. The
/// bounds differ per operation, so each read passes its own modelled range.
///
/// `pub(super)`, so the other paged reads of this state module — a command listing, a
/// document listing, an automation report — bound their own pages through this one
/// function rather than a second copy of the range check.
pub(super) fn page_size(max_results: Option<i32>, minimum: i32, maximum: i32) -> Result<usize, String> {
    let requested = max_results.unwrap_or(maximum);
    if !(minimum..=maximum).contains(&requested) {
        return Err(format!(
            "Value '{requested}' at 'maxResults' failed to satisfy constraint: Member must be \
             between {minimum} and {maximum}"
        ));
    }
    Ok(requested as usize)
}

/// The page size a hierarchy read serves (see [`page_size`]).
pub fn parameters_by_path_page_size(max_results: Option<i32>) -> Result<usize, String> {
    page_size(
        max_results,
        MIN_PARAMETERS_BY_PATH_PAGE_SIZE,
        MAX_PARAMETERS_BY_PATH_PAGE_SIZE,
    )
}

/// The lower bound the modelled `MaxResults` of a parameter-history read carries
/// (`GetParameterHistoryRequest$MaxResults`, range 1..50).
pub const MIN_PARAMETER_HISTORY_PAGE_SIZE: i32 = 1;

/// The upper bound of the same member. It is also the page size a request that names no
/// `MaxResults` gets: "The maximum number of items to return for this call" bounds a page
/// the caller asked to be bounded, so a caller asking for no bound is served up to the
/// modelled maximum.
pub const MAX_PARAMETER_HISTORY_PAGE_SIZE: i32 = 50;

/// The page size a parameter-history read serves (see [`page_size`]).
pub fn parameter_history_page_size(max_results: Option<i32>) -> Result<usize, String> {
    page_size(
        max_results,
        MIN_PARAMETER_HISTORY_PAGE_SIZE,
        MAX_PARAMETER_HISTORY_PAGE_SIZE,
    )
}

/// The lower bound the modelled `MaxResults` of a parameter describe carries
/// (`DescribeParametersRequest$MaxResults`, range 1..50).
pub const MIN_DESCRIBE_PARAMETERS_PAGE_SIZE: i32 = 1;

/// The upper bound of the same member, and the page size a request naming no
/// `MaxResults` is served up to (see [`page_size`]).
pub const MAX_DESCRIBE_PARAMETERS_PAGE_SIZE: i32 = 50;

/// The page size a parameter describe serves (see [`page_size`]).
pub fn describe_parameters_page_size(max_results: Option<i32>) -> Result<usize, String> {
    page_size(
        max_results,
        MIN_DESCRIBE_PARAMETERS_PAGE_SIZE,
        MAX_DESCRIBE_PARAMETERS_PAGE_SIZE,
    )
}

/// The hierarchy a read's `Path` names, canonical (no leading and no trailing `/`), or
/// the reason the request is refused.
///
/// "The hierarchy for the parameter. Hierarchies start with a forward slash (/)"
/// (`GetParametersByPath`'s `Path` doc), so a path that carries no leading separator
/// names no hierarchy; and a path is a parameter-name PREFIX, so every level of it is
/// spelled with the characters a name may carry ([`is_name_char`]) — which is also the
/// spaces rule, since no whitespace is one of them. A path breaking either is refused
/// with [`PARAMETER_NAME_REQUIREMENTS`], the paragraph the service states those
/// requirements in.
///
/// The reserved-prefix rule that paragraph also states bounds what a caller may CREATE,
/// not what it may read: AWS's own published hierarchy is reached by naming it
/// ([`public_parameters`]), so a path in it resolves rather than being refused.
pub fn parameter_path(path: &str) -> Result<&str, String> {
    // The service "removes any spaces specified for the beginning or end of a parameter
    // name" ([`trimmed_name`]), and a name is the path's own rule here.
    let trimmed = path.trim();
    if !trimmed.starts_with('/') || !trimmed.chars().all(is_name_char) {
        return Err(PARAMETER_NAME_REQUIREMENTS.to_string());
    }
    Ok(canonical_name(trimmed).trim_end_matches('/'))
}

/// Whether the parameter stored under canonical `name` lies in the hierarchy under
/// canonical `path` (both as [`canonical_name`] and [`parameter_path`] render them).
///
/// A parameter is under a path when the path is a whole prefix of its name's levels —
/// "the hierarchy is the parameter name except the last part of the parameter" — so the
/// parameter AT the path is not under it and a path is never half a level (`/Dev` does
/// not hold `/DevOps/x`). A non-recursive read reports only the level immediately below
/// the path; a recursive one reports every level under it ("Retrieve all parameters
/// within a hierarchy", the `Recursive` doc).
pub fn is_under_path(name: &str, path: &str, recursive: bool) -> bool {
    let rest = if path.is_empty() {
        name
    } else {
        match name.strip_prefix(path).and_then(|rest| rest.strip_prefix('/')) {
            Some(rest) => rest,
            None => return false,
        }
    };
    !rest.is_empty() && (recursive || !rest.contains('/'))
}
