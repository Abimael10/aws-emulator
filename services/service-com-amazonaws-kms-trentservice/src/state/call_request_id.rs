// SPDX-License-Identifier: CC0-1.0

//! The ONE id the response to the call being served names its request by, and the usage
//! record that id joins it to.
//!
//! Every response AWS sends names the request it answered — the id an SDK reports as that
//! call's `ResponseMetadata.RequestId` — and for KMS that id is not only a diagnostic: it
//! is the join key the key's usage tracking is defined on, since `KeyLastUsageData`
//! reports "the request ID of the cryptographic operation"
//! (`smithy_query(query="shape",shape="KeyLastUsageData")`). So the id a cryptographic
//! call's response carries and the `KmsRequestId` its usage record carries have to be ONE
//! value, and a caller holding its own response can look up what KMS last served for the
//! key and recognise its own call.
//!
//! WHERE each half of that value is needed makes the order plain: the usage record is
//! written while the call is being served, and the response that states the id does not
//! exist yet. So the id is minted the first time the call needs to state it
//! ([`request_id_of_this_call`]) and held for the rest of that call, and the layer that
//! renders the answer states THAT id rather than a second one
//! ([`stated_request_id`], read by `crate::service::management_events`). One call,
//! one id, however many records it writes: a `ReEncrypt` that records a use of two keys
//! records the same request against both, because it is one request.
//!
//! The id's FORM is the shared one every emulated service mints
//! ([`::service_common::query_error_envelope::new_request_id`]), so every reader of a
//! response's request id — an SDK, the CloudTrail record of the call, this service's own
//! usage tracking — recognises it.
//!
//! # What the holding is scoped to, and what never uses it
//!
//! The slot belongs to ONE call this service serves, and the two ends are both owned by the
//! layer every such call passes through ([`crate::service::management_events`]): it is
//! CLEARED before the call runs ([`forget_any_unstated_request_id`]) and TAKEN when the
//! response is named, so nothing a previous call left — a call whose response path never
//! finished, say — can name a later one. That is the same discipline the shared recorder
//! holds its own pending record under.
//!
//! A use of a key that is NOT this service serving a call has no such response to be named
//! by, so it never touches the slot: another service reaching this store in-process while
//! serving a call of its own — Amazon SQS asking for the data key an encrypted queue's send
//! needs — states the ids of the call IT made through
//! [`ComAmazonawsKmsTrentserviceState::record_last_usage_of_service_call`], which is the
//! one way a usage record is written from outside a served call.

use std::cell::RefCell;

use super::*;

thread_local! {
    /// The id the call this thread is serving states as the request its response answers,
    /// once something in that call has had to state it. Cleared before each served call and
    /// taken when its response is named (see the module docs), so it never outlives the
    /// call that minted it.
    static STATED_REQUEST_ID: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The id the response to the call being served will name its request by.
///
/// Minted on the first ask and the same on every later ask within the call, so two records
/// one call writes name one request.
#[must_use]
pub fn request_id_of_this_call() -> String {
    STATED_REQUEST_ID.with(|slot| {
        slot.borrow_mut()
            .get_or_insert_with(::service_common::query_error_envelope::new_request_id)
            .clone()
    })
}

/// The id this call has already stated, taken out of the slot so it is read once — or
/// `None` when nothing in the call had to state one, which leaves the response layer to
/// mint the id it names the request by.
#[must_use]
pub fn stated_request_id() -> Option<String> {
    STATED_REQUEST_ID.with(|slot| slot.borrow_mut().take())
}

/// Forget whatever id this thread may still hold, because it belongs to no call being
/// served now.
///
/// Called before each served call: an id is DISCARDED rather than reused, since a response
/// may never be named by a request another caller made (see the module docs).
pub fn forget_any_unstated_request_id() {
    STATED_REQUEST_ID.with(|slot| {
        slot.borrow_mut().take();
    });
}

impl ComAmazonawsKmsTrentserviceState {
    /// Record that `operation` used the key `key_id` that `region`'s endpoint serves, on
    /// behalf of a caller ANOTHER service is serving — the use its own in-process call to
    /// this service made.
    ///
    /// Such a call is KMS's call all the same: it is recorded in the account's event history
    /// as a call of this service, and the key it used reports it as its last use. What it
    /// does not have is a response of its own that a client reads, so the two ids are stated
    /// by the service that made the call — `kms_request_id` the id that call was answered
    /// under and `cloud_trail_event_id` the event it was recorded in — rather than taken
    /// from a served call's slot, which holds the id of no such call.
    pub fn record_last_usage_of_service_call(
        &mut self,
        region: &str,
        key_id: &str,
        operation: &str,
        kms_request_id: String,
        cloud_trail_event_id: Option<String>,
    ) {
        if let Some(key) = self.key_mut_in(region, key_id) {
            key.record_last_usage_named(operation, kms_request_id, cloud_trail_event_id);
        }
    }

    /// Name the CloudTrail event the call answered under `kms_request_id` was recorded in,
    /// on every usage record that call wrote.
    ///
    /// `KeyLastUsageData$CloudTrailEventId` is "the CloudTrail event ID of the
    /// cryptographic operation"
    /// (`smithy_query(query="shape",shape="KeyLastUsageData")`), so it is the id of a
    /// record that exists rather than one of its own: the event is written once the
    /// response that answers the call exists, and the usage record is named from it then —
    /// which is why a record carries no event id until the call it describes has been
    /// recorded.
    ///
    /// The records this names are found by the request they belong to, so a call that
    /// recorded a use of two keys names the same event on both and a call that recorded
    /// none names nothing.
    pub fn name_recorded_usage_event(&mut self, kms_request_id: &str, cloud_trail_event_id: &str) {
        for key in self.keys.values_mut() {
            if let Some(usage) = key
                .last_usage
                .as_mut()
                .filter(|usage| usage.kms_request_id == kms_request_id)
            {
                usage.cloud_trail_event_id = Some(cloud_trail_event_id.to_string());
            }
        }
    }
}
