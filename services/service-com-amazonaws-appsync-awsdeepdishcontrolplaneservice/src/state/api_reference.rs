// SPDX-License-Identifier: CC0-1.0

//! How a request NAMES an API, and how a named API is resolved to the record this account
//! holds for it.
//!
//! AppSync publishes a GraphQL API's identity in TWO forms: the `apiId` it minted and the ARN
//! built on that id (`arn:<partition>:appsync:<region>:<account>:apis/<apiId>`), which a
//! create answers together and which every read of the API reports side by side — the ARN is
//! also what a CloudFormation `AWS::AppSync::GraphQLApi` records as the resource's physical
//! id. An `Api` (the Event API family) publishes its identity the same way, as `apiId` and
//! `apiArn`. So a reference of either form names the API it was built for, and only a value
//! that is NEITHER form names no API at all.
//!
//! WHICH of the two client refusals the model binds to every API-scoped operation an `apiId`
//! that names no API of this account draws is fixed per OPERATION, not read off the value:
//!
//! * an operation that SCREENS the reference answers a value of a form no API reference can
//!   have as the malformed request it is ([`is_api_id`], or the stricter
//!   [`is_issued_api_reference`]), with [`invalid_api_id_message`] naming the rejected value,
//!   before anything is resolved;
//! * an operation that only RESOLVES the reference hands it straight to [`ApiStore`] and
//!   reports EVERY value it cannot resolve as the missing API it names, with the fixed
//!   sentence [`crate::state::API_NOT_FOUND_MESSAGE`] — whatever that value's length or
//!   alphabet.
//!
//! The operations of an API's type collection (`/v1/apis/{apiId}/types…`: `ListTypes`,
//! `CreateType`, `GetType`, `UpdateType` and `DeleteType`) are the resolving kind, so none of
//! them tests the form of the reference it is handed.
//!
//! The resolution is in ONE place, [`ApiStore`], which is how every operation of this crate
//! reaches an API: a map of the APIs of one family keyed by the id AppSync minted, whose
//! `get` / `get_mut` / `contains_key` / `remove` take a reference in either form. So a read
//! and a write of the same API can never disagree about which APIs exist, and a caller
//! holding either form reaches the same API and the same records held UNDER it (those are
//! keyed by the API's own id, which [`ApiStore::api_id_of`] answers).
//!
//! Resolving an ARN costs one map lookup, not a scan: the id is the ARN's own last segment,
//! so it is read off the reference ([`api_id_in_arn`]) and looked up by key, and the stored
//! record's own ARN is then compared whole — so an ARN of another Region, another account or
//! another partition names no API of this account even when its last segment is an id this
//! account holds. Nothing here indexes, slices or unwraps: the parse is a `starts_with` and a
//! `rsplit_once`, both total, so no reference a caller can send can panic.

use std::ops::{Deref, DerefMut};

use super::*;

/// The number of characters an AppSync API id has.
///
/// The ids this service mints are 26 base-32 characters
/// ([`ComAmazonawsAppsyncAwsdeepdishcontrolplaneserviceState::mint_id`]), the shape AppSync's
/// own `apiId` takes. The model types every `apiId` member as a plain `String`, so nothing
/// refuses a longer one at the boundary; [`is_api_id`] is where the operations that screen a
/// reference refuse it.
pub const API_ID_LEN: usize = 26;

/// Whether `candidate` could name an AppSync API at all: an API id, or an ARN — the two forms
/// AppSync publishes an API's identity in (see this module's own documentation).
///
/// The test on a BARE id is on LENGTH only: an id longer than [`API_ID_LEN`] holds more
/// characters than an AppSync API id has, so it names no API and the call is a bad request,
/// while a shorter one is simply an id this account does not hold and the call is a
/// not-found. The alphabet is deliberately NOT part of the test: AppSync answers not-found
/// for a short id drawn from outside the id alphabet, so a character outside it does not make
/// the request malformed.
///
/// Read only by the operations that SCREEN an `apiId` before resolving it (this module's own
/// documentation states which kind an operation is): an operation that only resolves the
/// reference calls nothing here and reports an unresolvable value as the missing API it names.
///
/// This is NOT the test for the form of an id the service ISSUES ([`is_opaque_id`]), which
/// reads the length and the alphabet both: the two are deliberately different tests, because
/// the operations that classify an `apiId` by the form of the reference rather than by its
/// length are refused differently ([`is_issued_api_reference`]). Neither is a worse copy of
/// the other, so they are not to be unified.
pub fn is_api_id(candidate: &str) -> bool {
    api_id_in_arn(candidate).is_some()
        || (!candidate.is_empty() && candidate.chars().count() <= API_ID_LEN)
}

/// AppSync's refusal text for an `apiId` that names no API at all ([`is_api_id`]): the
/// rejected value, so the caller can see what was refused.
pub fn invalid_api_id_message(api_id: &str) -> String {
    format!("Invalid API ID {api_id}")
}

/// Whether `candidate` is a reference this service could have ISSUED for an API: the opaque id
/// it mints ([`is_opaque_id`]) or an ARN built on one.
///
/// The counterpart of [`is_api_id`] for the operations that classify an `apiId` by the FORM of
/// the reference rather than by its length, so a value of either published form is a reference
/// the caller could be holding and only a value of neither makes the request malformed.
pub fn is_issued_api_reference(candidate: &str) -> bool {
    api_id_in_arn(candidate).is_some_and(is_opaque_id) || is_opaque_id(candidate)
}

/// The API id `reference` carries when it is an ARN — the last segment of
/// `arn:…:apis/<apiId>` — or `None` for a reference that is not an ARN at all.
///
/// Total by construction: a `starts_with` and a `rsplit_once`, so no value a caller can send
/// is indexed, sliced or unwrapped here.
fn api_id_in_arn(reference: &str) -> Option<&str> {
    reference
        .starts_with(ARN_PREFIX)
        .then(|| reference.rsplit_once('/').map(|(_, id)| id))
        .flatten()
        .filter(|id| !id.is_empty())
}

/// What an API record of either family publishes as its OWN ARN, which is the form of
/// reference [`ApiStore`] resolves beside the id.
pub trait PublishedArn {
    /// The ARN AppSync built for this API.
    fn published_arn(&self) -> &str;
    /// The `apiId` AppSync minted for it, which every record held under the API is keyed by.
    fn minted_api_id(&self) -> &str;
}

impl PublishedArn for GraphqlApiRecord {
    fn published_arn(&self) -> &str {
        &self.arn
    }

    fn minted_api_id(&self) -> &str {
        &self.api_id
    }
}

impl PublishedArn for EventApiRecord {
    fn published_arn(&self) -> &str {
        &self.api_arn
    }

    fn minted_api_id(&self) -> &str {
        &self.api_id
    }
}

/// The APIs of one resource family this account holds, keyed by the `apiId` AppSync minted
/// for each.
///
/// A plain map of them with ONE thing added: a lookup resolves a reference in either form
/// AppSync publishes an API's identity in (this module's own documentation states the rule and
/// why the resolution costs one lookup rather than a scan). Everything else a map does is the
/// map's own, through [`Deref`] — a listing still iterates `values()`, a create still
/// `insert`s under the id it minted — so this type changes how an API is FOUND and nothing
/// about how the APIs are held.
#[derive(Debug, Clone)]
pub struct ApiStore<R>(BTreeMap<String, R>);

impl<R> Default for ApiStore<R> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}

impl<R: PublishedArn> ApiStore<R> {
    /// The API `reference` names, in either published form, or `None` when this account holds
    /// no such API.
    pub fn get(&self, reference: &str) -> Option<&R> {
        match api_id_in_arn(reference) {
            // An ARN names the API whose own ARN it IS, so the stored ARN is compared whole:
            // the id is only what makes the lookup a keyed one.
            Some(api_id) => self
                .0
                .get(api_id)
                .filter(|record| record.published_arn() == reference),
            None => self.0.get(reference),
        }
    }

    /// The same API, for a call that WRITES it — resolved exactly as [`Self::get`] resolves it
    /// for a read, so a read and a write can never disagree about which APIs exist.
    pub fn get_mut(&mut self, reference: &str) -> Option<&mut R> {
        match api_id_in_arn(reference) {
            Some(api_id) => self
                .0
                .get_mut(api_id)
                .filter(|record| record.published_arn() == reference),
            None => self.0.get_mut(reference),
        }
    }

    /// Whether this account holds the API `reference` names.
    pub fn contains_key(&self, reference: &str) -> bool {
        self.get(reference).is_some()
    }

    /// Remove the API `reference` names and answer the record it held, or `None` when this
    /// account holds no such API.
    pub fn remove(&mut self, reference: &str) -> Option<R> {
        let api_id = self.api_id_of(reference)?;
        self.0.remove(&api_id)
    }

    /// The `apiId` AppSync minted for the API `reference` names, which is the id every record
    /// kept UNDER that API is keyed by — so a call that addressed the API by its ARN reaches
    /// the same records as one that addressed it by its id. `None` when this account holds no
    /// such API.
    pub fn api_id_of(&self, reference: &str) -> Option<String> {
        self.get(reference)
            .map(|record| record.minted_api_id().to_string())
    }
}

impl<R> Deref for ApiStore<R> {
    type Target = BTreeMap<String, R>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<R> DerefMut for ApiStore<R> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
