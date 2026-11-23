// SPDX-License-Identifier: CC0-1.0

//! How this control plane refuses a create whose `name` the account already holds, for the
//! families that had been wording that refusal inside their own handler.
//!
//! One rule, for every resource whose create documents its name as unique within the
//! account: the refusal is the modelled `ConflictException` carrying this control plane's
//! duplicate-name sentence ([`super::name_conflict`]) — the family's subject, the name the
//! REQUEST carried in single quotes, and the account the name is held in — and it discloses
//! nothing about the resource that already holds the name. The identifier this service
//! generated for that other resource is not part of the sentence: it is a value the caller
//! never sent and was refused the right to learn, and quoting it would make the refusal
//! differ between two runs that asked the very same thing.
//!
//! Each family's SUBJECT is spelled the way that family's recorded refusal spells it, which
//! is why it is a constant per family rather than one noun derived from the resource: the
//! gateway family words its subject with the indefinite article and the noun in lower case
//! ("A gateway with name '<name>' already exists in this account"), while the families
//! already on this sentence elsewhere in [`crate::state`] word theirs as the bare noun
//! ("Browser with name …", "Consent portal with name …"). A family with no recording of
//! its own keeps the bare noun its siblings use.

use super::*;

/// How the gateway family spells its subject: with the indefinite article and the noun in
/// lower case, which is what its recorded duplicate-name refusal states.
const GATEWAY_CONFLICT_LABEL: &str = "A gateway";

/// The bare nouns the remaining families spell their subject with, as the families already
/// on this sentence spell theirs.
const AGENT_RUNTIME_CONFLICT_LABEL: &str = "Agent runtime";
const MEMORY_CONFLICT_LABEL: &str = "Memory";
const HARNESS_CONFLICT_LABEL: &str = "Harness";
const DATASET_CONFLICT_LABEL: &str = "Dataset";
const REGISTRY_CONFLICT_LABEL: &str = "Registry";
const CONFIGURATION_BUNDLE_CONFLICT_LABEL: &str = "Configuration bundle";
const WORKLOAD_IDENTITY_CONFLICT_LABEL: &str = "Workload identity";

/// The modelled conflict for a gateway `name` this account already holds.
///
/// `CreateGateway`'s `name` "must be unique within your account" (`smithy_query
/// query="shape" shape="CreateGatewayRequest"`), so a repeated name is a conflict rather
/// than a second gateway later reads cannot tell apart.
pub fn gateway_name_conflict(name: &str) -> ConflictException {
    super::name_conflict(GATEWAY_CONFLICT_LABEL, name)
}

/// The modelled conflict for an agent runtime `name` this account already holds.
pub fn agent_runtime_name_conflict(name: &str) -> ConflictException {
    super::name_conflict(AGENT_RUNTIME_CONFLICT_LABEL, name)
}

/// The modelled conflict for a memory `name` this account already holds.
pub fn memory_name_conflict(name: &str) -> ConflictException {
    super::name_conflict(MEMORY_CONFLICT_LABEL, name)
}

/// The modelled conflict for a harness `name` this account already holds.
pub fn harness_name_conflict(name: &str) -> ConflictException {
    super::name_conflict(HARNESS_CONFLICT_LABEL, name)
}

/// The modelled conflict for a dataset `name` this account already holds.
pub fn dataset_name_conflict(name: &str) -> ConflictException {
    super::name_conflict(DATASET_CONFLICT_LABEL, name)
}

/// The modelled conflict for a registry `name` this account already holds.
pub fn registry_name_conflict(name: &str) -> ConflictException {
    super::name_conflict(REGISTRY_CONFLICT_LABEL, name)
}

/// The modelled conflict for a configuration-bundle `name` this account already holds.
pub fn configuration_bundle_name_conflict(name: &str) -> ConflictException {
    super::name_conflict(CONFIGURATION_BUNDLE_CONFLICT_LABEL, name)
}

/// The modelled conflict for a workload-identity `name` this account already holds.
///
/// Carried as the SENTENCE rather than as a `ConflictException`: `CreateWorkloadIdentity`
/// models no conflict error, so its handler states this refusal in its modelled
/// `ValidationException` with the reason the enum has for a request the stored state
/// refuses — the words are this control plane's one duplicate-name sentence either way.
pub fn workload_identity_name_conflict_message(name: &str) -> String {
    super::name_conflict_message(WORKLOAD_IDENTITY_CONFLICT_LABEL, name)
}
