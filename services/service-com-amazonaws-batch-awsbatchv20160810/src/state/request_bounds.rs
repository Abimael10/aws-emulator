// SPDX-License-Identifier: CC0-1.0

//! The sentences AWS Batch refuses a request that is OUTSIDE A PUBLISHED BOUND with, and
//! the bounds themselves.
//!
//! Every operation of this model declares exactly one client error (`ClientException`,
//! HTTP 400), so the MESSAGE is the only machine-readable detail a refusal carries: a
//! caller telling an over-limit bulk request, an out-of-range page size and an over-long
//! describe list apart has nothing else to branch on. So each bound refusal states the rule
//! the way AWS Batch states it — the bound it holds the request to, interpolated from the
//! constant the check itself reads, and nothing the request carried — inside the frontend
//! envelope every such pre-lookup refusal of this service is answered in
//! ([`super::client_exception_enveloped`]).
//!
//! AWS words these PER OPERATION rather than once for the service: the page-size windows
//! below differ in their comparatives, their terminator and their maximum, so each
//! operation's sentence is built from its own constants here instead of being normalised
//! into one. Only the operations whose wording is not recorded fall back on this service's
//! own sentence ([`super::max_results_bound_message`]).

use super::*;

/// What a bulk job operation answers for a `jobs` list longer than the documented maximum
/// ([`limits::MAX_BULK_JOBS`]): the fixed sentence Batch states the per-request maximum
/// with, carrying the maximum and nothing of the request.
///
/// One sentence for all three bulk operations of this model (`CancelJobs`, `TerminateJobs`,
/// `TerminateServiceJobs`), because the bound is the same documented one — each of their
/// `jobs` members is "an array of up to 50 … IDs" — and the rule a caller has to satisfy is
/// the same: split the request into chunks of at most that many ids. The maximum is
/// interpolated from the constant the handlers check against, so the sentence and the bound
/// it reports cannot drift apart.
pub fn bulk_jobs_bound_message() -> String {
    format!(
        "A maximum of {} jobs can be specified per request.",
        limits::MAX_BULK_JOBS
    )
}

/// What `ListSchedulingPolicies` answers for a `maxResults` outside the window it accepts:
/// the window stated with STRICT comparatives on both sides, terminated.
///
/// The bounds are interpolated from the ones the handler enforces
/// ([`super::MIN_LIST_SCHEDULING_POLICIES_RESULTS`],
/// [`super::MAX_LIST_SCHEDULING_POLICIES_RESULTS`]), so the sentence and the check cannot
/// drift apart; the comparatives and the terminator are this operation's own, which is why
/// it does not share the consumable-resource listings' sentence below.
pub fn list_scheduling_policies_max_results_message() -> String {
    format!(
        "Max Results should be greater than {MIN_LIST_SCHEDULING_POLICIES_RESULTS} and less \
         than {MAX_LIST_SCHEDULING_POLICIES_RESULTS}."
    )
}

/// What `ListConsumableResources` answers for a `maxResults` outside its window: the window
/// stated as an exclusive floor and an INCLUSIVE ceiling, unterminated.
pub fn list_consumable_resources_max_results_message() -> String {
    max_results_inclusive_window(
        MIN_LIST_CONSUMABLE_RESOURCES_RESULTS,
        MAX_LIST_CONSUMABLE_RESOURCES_RESULTS,
    )
}

/// What `ListJobsByConsumableResource` answers for a `maxResults` outside its window: the
/// same form its sibling listing states, with this operation's own maximum
/// ([`super::MAX_LIST_JOBS_BY_CONSUMABLE_RESOURCE_RESULTS`]).
pub fn list_jobs_by_consumable_resource_max_results_message() -> String {
    max_results_inclusive_window(
        MIN_LIST_JOBS_BY_CONSUMABLE_RESOURCE_RESULTS,
        MAX_LIST_JOBS_BY_CONSUMABLE_RESOURCE_RESULTS,
    )
}

/// The page-size window stated with an exclusive floor and an inclusive ceiling: the floor
/// is the value just below the smallest accepted one, so the sentence is derived from the
/// bounds the operation enforces rather than restating them.
fn max_results_inclusive_window(minimum: i32, maximum: i32) -> String {
    format!(
        "Max Results should be greater than {} and less than or equal to {maximum}",
        minimum - 1
    )
}

/// The fewest entries a `Describe*` identifier list is reported as admitting: these
/// sentences state the window as `1` to the maximum whether or not the member also accepts
/// an empty list to mean "every one of them".
pub const MIN_DESCRIBE_LIST_ENTRIES: usize = 1;

/// How Batch words the refusal of a `Describe*` identifier list longer than the documented
/// maximum: the KIND of resource counted, then the window the member admits — never the
/// number of entries the request carried.
///
/// One template for every such list of this model, each operation naming its own kind: the
/// scheduling-policy `arns` count ([`super::scheduling_policy_arns_bound_message`]), the
/// `computeEnvironments` list and the `jobQueues` list, whose members each document "a list
/// of up to 100 … entries" ([`super::MAX_DESCRIBE_FILTER_ENTRIES`]).
pub fn describe_entries_bound_message(kind: &str, minimum: usize, maximum: usize) -> String {
    format!("Number of {kind} should be between {minimum} and {maximum}.")
}

/// What `DescribeComputeEnvironments` answers for a `computeEnvironments` list past the
/// documented maximum.
pub fn compute_environments_bound_message() -> String {
    describe_entries_bound_message(
        "compute environments",
        MIN_DESCRIBE_LIST_ENTRIES,
        MAX_DESCRIBE_FILTER_ENTRIES,
    )
}

/// What `DescribeJobQueues` answers for a `jobQueues` list past the documented maximum —
/// the same rule as its compute-environment sibling, naming its own kind.
pub fn job_queues_bound_message() -> String {
    describe_entries_bound_message(
        "job queues",
        MIN_DESCRIBE_LIST_ENTRIES,
        MAX_DESCRIBE_FILTER_ENTRIES,
    )
}

/// How many bytes one kilobyte is in the size limits Batch publishes for a request.
pub const BYTES_PER_KILOBYTE: usize = 1024;

/// The job-definition size limit AWS Batch publishes as a service quota ("Maximum job
/// definition size: 24 KiB"), in kilobytes, which is also the number its refusal states.
pub const MAX_JOB_DEFINITION_KILOBYTES: usize = 24;

/// That limit in bytes: a registration whose serialized definition reaches it is refused
/// and nothing is stored, since the sentence it answers states the size "must be less than"
/// the limit.
pub const MAX_JOB_DEFINITION_BYTES: usize = MAX_JOB_DEFINITION_KILOBYTES * BYTES_PER_KILOBYTE;

/// What `RegisterJobDefinition` answers for a definition past [`MAX_JOB_DEFINITION_BYTES`]:
/// the published limit, then the size of the definition the request carried, both in whole
/// kilobytes.
///
/// `bytes` is the definition's own serialized length, and the size reported is that length
/// FLOORED to whole kilobytes — the unit the limit is published in — so the sentence states
/// the request's size in the same unit as the ceiling it missed.
pub fn job_definition_size_message(bytes: usize) -> String {
    format!(
        "JobDefinition size must be less than {MAX_JOB_DEFINITION_KILOBYTES} KB, got {} KB.",
        bytes / BYTES_PER_KILOBYTE
    )
}

/// How many bytes `value` occupies on the wire of this service's protocol, or `None` when
/// the codec cannot render it.
///
/// Rendered through the protocol's OWN codec (`JsonCodec::rest_json`, the one that
/// serializes this service's bodies) rather than measured member by member, so the length a
/// size limit is applied to is the length of the compact JSON document the shape amounts
/// to: the member spelling, the number formatting and every nested shape are the protocol's.
/// That length is derived from the request and nothing else — the schema's member set is
/// fixed, so the same definition always measures the same, whatever whitespace the client
/// put around it.
pub fn serialized_byte_length(
    schema: &::aws_smithy_schema::Schema,
    value: &dyn ::aws_smithy_schema::serde::SerializableStruct,
) -> Option<usize> {
    use ::aws_smithy_schema::codec::Codec;
    use ::aws_smithy_schema::serde::ShapeSerializer;

    let mut serializer =
        ::aws_smithy_schema_server::json::JsonCodec::rest_json().create_serializer();
    if let Err(error) = serializer.write_struct(schema, value) {
        ::tracing::warn!(
            %error,
            "the JSON codec could not render a request, so its size is not measured"
        );
        return None;
    }
    Some(serializer.into_string().len())
}

/// What the consumable-resource surface answers for a quantity below zero: the fixed
/// sentence for the rule both of its quantity members publish ("Must be a non-negative
/// value." / "Must be non-negative."), which states the rule and does NOT echo the value it
/// refused.
pub const NEGATIVE_QUANTITY: &str = "Quantity cannot be a negative number.";
