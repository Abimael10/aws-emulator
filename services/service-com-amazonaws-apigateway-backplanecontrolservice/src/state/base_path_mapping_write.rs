// SPDX-License-Identifier: CC0-1.0

//! How long this service spends WRITING a base path mapping — the span between taking such a
//! request in and storing the record, which is what a call arriving beside it is served
//! alongside rather than after.
//!
//! Why the span is kept at all: `DeleteRestApi` is refused while a base path mapping still
//! routes to the API ([`rest_api_in_use_by_base_path_mapping`]), and that precondition is
//! READ from the stored mappings
//! ([`ComAmazonawsApigatewayBackplanecontrolserviceState::domain_names_mapping_rest_api`]).
//! That read and a mapping write belong to two different requests, and nothing evaluates
//! them against each other: a mapping write still being served when a delete arrives is not
//! in the state the delete's read resolves, so the delete is ACCEPTED — it answers 202 — and
//! the mapping is stored after it, against state that still held the API
//! ([`ComAmazonawsApigatewayBackplanecontrolserviceState::rest_api_referenceable`]). The
//! record then outlives the API it names, which is the orphaned mapping a caller that issues
//! the two calls together gets, and the one the delete's refusal exists to prevent a caller
//! that issues them in order from creating.
//!
//! A caller that waits for its answer before asking again is never inside the span, because
//! the record is stored and reported before the write answers: a delete issued after a
//! mapping's answer reads that mapping and is refused, however quickly the two calls follow
//! one another.

use std::time::Duration;

#[allow(unused_imports)]
use super::{
    rest_api_in_use_by_base_path_mapping, ComAmazonawsApigatewayBackplanecontrolserviceState,
};

/// How long this service takes between taking a base path mapping write in and storing the
/// record: a `CreateBasePathMapping`, and an `UpdateBasePathMapping` that points the mapping
/// at another API.
///
/// CHOSEN rather than documented — API Gateway publishes no duration for such a write. What
/// it stands in for is the time the service itself spends serving one, which is what makes a
/// write a caller dispatched TOGETHER with a delete of the same API overlap that delete's own
/// read of the mappings, so the pair resolves to the two answers the service gives for
/// concurrent calls (201 and 202) rather than to whichever of them reached the store first.
///
/// Short enough that it costs such a write about as much as one network round trip. It is
/// spent in front of the store, so the request's own input validation refuses a malformed
/// request without it, while a refusal the stored records decide — an unresolvable domain
/// name, API or stage, the per-domain quota, a base path another mapping already answers to
/// — is answered after it, as it is by a service that reads those records at the same point
/// in the write it reads them for.
pub const BASE_PATH_MAPPING_WRITE_SPAN: Duration = Duration::from_millis(100);
