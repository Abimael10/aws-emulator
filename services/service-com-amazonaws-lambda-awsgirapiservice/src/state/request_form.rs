// SPDX-License-Identifier: CC0-1.0

//! The FORM a request has to have BEFORE anything is looked up: a `FunctionName` that names
//! the function ITSELF where that is all the operation addresses, and a `Marker` this
//! service issued.
//!
//! Lambda reads such a request as a REQUEST first and as a resource second: the value is
//! judged against what the operation admits, and only a request that passes reaches the
//! store. So the answer to a malformed identifier is the same whether or not the resource
//! exists — which is the whole difference a caller acts on, because a 404 says "create it
//! first" while this 400 says "stop sending this value". A request that DOES pass and finds
//! nothing is answered for the resource it resolved, which is what
//! [`reserved_concurrency_not_found_message`] words for the operations whose subject is a
//! function's reserved concurrency.
//!
//! Two clauses live here rather than once per handler, because each is the SERVICE's and not
//! one operation's:
//!
//! 1. an operation whose subject is the FUNCTION — the aliases it holds, the versions it
//!    holds, the reserved concurrency of the whole function, the asynchronous-invocation
//!    configurations it holds — addresses no version and no alias, so it takes no qualifier
//!    at all and a `FunctionName` carrying one names nothing it could act on
//!    ([`names_a_qualifier`]). The refusal is the modelled `InvalidParameterValueException`
//!    (400), with the fixed sentence that operation words for it — fixed, because the only
//!    thing wrong is that a qualifier is there at all;
//! 2. a `Marker` is "the pagination token that's returned by a previous request", so a value
//!    no page of a listing ever handed back names no place to continue from, and every
//!    paginated listing of this service refuses it with the one sentence Lambda words for it
//!    ([`INVALID_MARKER_MESSAGE`]).
//!
//! Every sentence here is RECORDED behaviour, from corpus `simparity-wire-aws-v1`; each
//! constant names the fixture and the step it was recorded at.

/// Whether `function_name` carries a QUALIFIER — a published version or an alias — as well
/// as the name of a function.
///
/// The model's `NamespacedFunctionName` pattern admits a trailing `:<qualifier>` on every
/// documented form of the member (`smithy_query(query="shape",
/// shape="NamespacedFunctionName")`), so the pattern the request walk checks cannot be what
/// refuses one: the rule is the OPERATION's, and this is the reading of the value it applies.
///
/// The unpublished version is not a qualifier in this sense: `$LATEST` is the function's own
/// current configuration, which is what an unqualified name already addresses, so a name
/// carrying it addresses exactly what the bare name does.
///
/// A value in none of the documented forms names no function at all, which is a different
/// refusal (`super::resolve_function` states it), so it is not reported as qualified here.
#[must_use]
pub fn names_a_qualifier(function_name: &str) -> bool {
    super::parse_function_reference(function_name).is_some_and(|reference| {
        reference
            .qualifier
            .as_deref()
            .is_some_and(|qualifier| qualifier != super::VERSION_LATEST)
    })
}

/// What `ListAliases` refuses a qualified `FunctionName` with.
///
/// Recorded behaviour: fixture `lambda-listing-qualified-identifier-and-missing-function-errors`,
/// step `task#5`, where a `FunctionName` carrying a qualifier is answered
/// `400 InvalidParameterValueException` with `message` exactly this.
pub const LIST_ALIASES_QUALIFIER_MESSAGE: &str =
    "Should not specify a qualifier in list alias request.";

/// What `ListVersionsByFunction` refuses a qualified `FunctionName` with.
///
/// Recorded behaviour: fixture `lambda-listing-qualified-identifier-and-missing-function-errors`,
/// step `task#8`.
pub const LIST_VERSIONS_QUALIFIER_MESSAGE: &str =
    "Should not specify a version number in list version request.";

/// What `ListFunctionEventInvokeConfigs` refuses a qualified `FunctionName` with.
///
/// Recorded behaviour: fixture `function-event-invoke-config-qualifier-error-paths`, step
/// `task#6`.
pub const EVENT_INVOKE_CONFIGS_QUALIFIER_MESSAGE: &str =
    "The function name must not include a version number or alias.";

/// What an operation on the function's RESERVED CONCURRENCY refuses a qualified
/// `FunctionName` with.
///
/// "The number of simultaneous executions to reserve for the function" is a property of the
/// whole function, "including all published versions and the unpublished version"
/// (`PutFunctionConcurrency`), so every operation on it — the read, the write and the
/// removal — addresses the function alone and words this one sentence for a qualifier. What
/// it asks for is "either a function name or an UNQUALIFIED function ARN", so the
/// unpublished version is a qualifier here too: what is wrong is that the identifier carries
/// one at all, which is why these operations read `FunctionReference::qualifier` directly
/// rather than through [`names_a_qualifier`].
///
/// Recorded behaviour: fixture `lambda-concurrency-qualified-and-missing-function-errors`,
/// step `task#5`, where `GetFunctionConcurrency` on a qualified function ARN is answered
/// `400 InvalidParameterValueException` with `message` exactly this.
pub const FUNCTION_ONLY_OPERATION_MESSAGE: &str =
    "This operation is permitted on Lambda functions only. Aliases and versions do not \
     support this operation. Please specify either a function name or an unqualified \
     function ARN.";

/// What an operation on the function's RESERVED CONCURRENCY says it could not FIND: the fixed
/// `Function not found: ` followed by the ARN the request RESOLVED.
///
/// The capacity a reservation sets aside is the capacity an INVOCATION of the function holds,
/// so the resource one of these WRITES addresses is the function AT the version such an
/// invocation runs: the identifier they take carries no qualifier of its own
/// ([`FUNCTION_ONLY_OPERATION_MESSAGE`]), so it resolves to the unpublished version, and the
/// refusal names that fully qualified ARN — the same resource a synchronous invocation of the
/// same name says it could not find ([`super::resolved_invocation_arn`]).
///
/// WHICH spelling of the ARN the sentence carries is RECORDED PER OPERATION rather than
/// derived — as the sentence a listing refuses a marker with is
/// ([`INVALID_PAGINATION_KEY_MESSAGE`]) — so the READ of the reservation words its own
/// ([`reserved_concurrency_read_not_found_message`]).
///
/// `resolution_stated` is what the resolution itself worded, which stands wherever this
/// endpoint resolves no ARN for the reference at all: an ARN of ANOTHER Region names no
/// function here and is refused on the Region rather than on a version
/// ([`super::foreign_region_function_message`]).
///
/// Recorded behaviour: corpus `simparity-behavioral-aws-v1`, trace
/// `iam__lambda__logs.put-concurrency-refused-on-unknown-function-preserves-real-reservation`,
/// where `PutFunctionConcurrency` on an unqualified name no function is held under is answered
/// `404 ResourceNotFoundException` with the `:$LATEST`-qualified ARN after the fixed prefix.
#[must_use]
pub fn reserved_concurrency_not_found_message(
    region: &str,
    reference: &super::FunctionReference,
    resolution_stated: String,
) -> String {
    super::resolved_invocation_arn(region, reference, None)
        .map_or(resolution_stated, |arn| format!("Function not found: {arn}"))
}

/// What a READ of the function's reserved concurrency says it could not FIND: the same fixed
/// `Function not found: ` prefix, followed by the function's OWN ARN — the resource the read
/// addressed, spelled with exactly the qualifier the request named, which for these operations
/// is none at all ([`FUNCTION_ONLY_OPERATION_MESSAGE`], and [`super::addressed_arn_of`] for
/// that spelling).
///
/// `resolution_stated` stands for the same reason it does in
/// [`reserved_concurrency_not_found_message`]: a reference this endpoint resolves no ARN of
/// this Region for is refused on the Region.
///
/// Recorded behaviour: fixture `lambda-concurrency-qualified-and-missing-function-errors`,
/// step `task#6`, where `GetFunctionConcurrency` on an unqualified name no function is held
/// under is answered `404 ResourceNotFoundException` with the UNQUALIFIED function ARN after
/// the fixed prefix.
#[must_use]
pub fn reserved_concurrency_read_not_found_message(
    region: &str,
    reference: &super::FunctionReference,
    resolution_stated: String,
) -> String {
    super::addressed_arn_of(region, reference, None)
        .map_or(resolution_stated, |arn| format!("Function not found: {arn}"))
}

/// What a paginated listing refuses a `Marker` it did not issue with.
///
/// A marker is a position in ONE listing's order, handed out by the page before it, so a
/// value that listing never minted names no place to continue from — whatever it is spelled
/// like. The sentence carries nothing of the request: the value is the caller's own, and what
/// is wrong with it is only that this service did not issue it.
///
/// Recorded behaviour: fixture `list-function-url-configs-error-paths`, step `task#6`, where
/// `ListFunctionUrlConfigs` with an unparseable `Marker` is answered
/// `400 InvalidParameterValueException` with `message` exactly this.
pub const INVALID_MARKER_MESSAGE: &str = "Invalid marker value from user.";

/// What `ListLayers` refuses a `Marker` it did not issue with.
///
/// The same CLASS of refusal as [`INVALID_MARKER_MESSAGE`] — a value this listing never
/// handed back names no place to continue from — worded as this listing words it, and
/// carrying nothing of the request for the same reason: the value is the caller's own, and
/// what is wrong with it is only that this service did not issue it.
///
/// Which sentence a listing words it with is RECORDED per operation rather than derived,
/// exactly as the fault class a refusal names is
/// (`crate::service::fault_class::NAMES_NO_CLASS`). Recorded behaviour: fixture
/// `lambda-lls-list-filter-and-paging-parameter-mistakes`, step `task#4`, where `ListLayers`
/// with a `Marker` this service never issued is answered
/// `400 InvalidParameterValueException` with `message` exactly this.
pub const INVALID_PAGINATION_KEY_MESSAGE: &str = "Invalid pagination key.";

#[cfg(test)]
mod tests {
    use super::*;

    /// A `FunctionName` carries a qualifier only when it names a version or an alias past
    /// the function's own name — one row per documented form of the member, qualified and
    /// not, plus the unpublished version (which is what the bare name already addresses) and
    /// the values that name no function at all.
    #[test]
    fn a_qualifier_is_recognised_in_every_documented_form_of_the_member() {
        // (case, the value a client sent, whether it names a version or an alias)
        let rows: [(&str, &str, bool); 14] = [
            ("a bare name", "my-function", false),
            ("a bare name with the unpublished qualifier", "my-function:$LATEST", false),
            ("a bare name with a version", "my-function:1", true),
            ("a bare name with an alias", "my-function:prod", true),
            ("a bare name with the latest published version", "my-function:$LATEST.PUBLISHED", true),
            ("the resource-type prefix form", "function:my-function", false),
            ("the resource-type prefix form with a version", "function:my-function:7", true),
            ("a partial ARN", "123456789012:function:my-function", false),
            ("a partial ARN with an alias", "123456789012:function:my-function:live", true),
            (
                "a function ARN",
                "arn:aws:lambda:us-west-2:123456789012:function:my-function",
                false,
            ),
            (
                "a function ARN with a version",
                "arn:aws:lambda:us-west-2:123456789012:function:my-function:3",
                true,
            ),
            (
                "a function ARN with an alias",
                "arn:aws:lambda:us-west-2:123456789012:function:my-function:blue",
                true,
            ),
            // Values in none of the documented forms: the identifier names no function, so
            // it is not a qualified one.
            ("two qualifiers", "my-function:1:2", false),
            ("an empty qualifier", "my-function:", false),
        ];
        for (case, value, expected) in rows {
            let actual = names_a_qualifier(value);
            assert_eq!(
                actual, expected,
                "case {case:?} ({value:?}): expected names_a_qualifier {expected}, got {actual}"
            );
        }
    }
}
