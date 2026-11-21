// SPDX-License-Identifier: CC0-1.0

//! The two refusals of a RULE ACROSS MEMBERS this registry raises, worded in the clause
//! every refused parameter of this service is stated in.
//!
//! ECR words every refused parameter `Invalid parameter at '<parameter>' failed to satisfy
//! constraint: '<constraint>'`, whichever validator decided it: the boundary walk over the
//! model's constraints ([`crate::service`], which words a violation's two halves) and a
//! handler checking a rule the model cannot express both state that one frame, so a caller
//! reads one form of refusal for an input ECR will not accept. The frame itself is
//! [`super::invalid_parameter_at`] / [`super::parameter_constraint`], written once there
//! and reached from here.
//!
//! What this module adds is the INNER SENTENCE of each cross-member rule, and which member
//! the clause names for it. The rules themselves live where they are enforced
//! ([`super::check_exclusion_filters`], and the operations that write an encryption
//! configuration); only their wording is here, so two operations enforcing one rule cannot
//! word it differently.

use super::Fault;

/// The member a refusal of the encryption posture names: the structure the request carries
/// it in, which is the member whose VALUE AS A WHOLE is contradictory — the type and the
/// key only disagree together, so neither of them alone is the parameter at fault.
pub const ENCRYPTION_CONFIGURATION_PARAMETER: &str = "encryptionConfiguration";

/// The member a refusal of the exclusion filters names: the list the request carries,
/// which is what may not be there beside the setting it was sent with.
pub const EXCLUSION_FILTERS_PARAMETER: &str = "imageTagMutabilityExclusionFilters";

/// What a request whose `encryptionConfiguration` names an encryption type that encrypts
/// with Amazon S3-managed keys and a `kmsKey` beside it failed: the key does not belong to
/// that type.
///
/// `encryption_type` is the type the request named, so the sentence states the posture the
/// caller actually asked for rather than one spelled into the message.
fn encryption_key_not_for_type_constraint(encryption_type: &str) -> String {
    format!(
        "Invalid encryption configuration: Encryption key should not be set for \
         {encryption_type} encryption."
    )
}

/// The refusal of that request, for an operation that answers a [`Fault`]: the
/// configuration is the parameter at fault, and the constraint is the one above.
pub fn encryption_key_not_for_type(encryption_type: &str) -> Fault {
    super::invalid_parameter_at(
        ENCRYPTION_CONFIGURATION_PARAMETER,
        &encryption_key_not_for_type_constraint(encryption_type),
    )
}

/// The same refusal as TEXT, for an operation that carries it on a modelled error of its
/// own rather than on a [`Fault`] — the pairing [`super::parameter_constraint`] stands
/// beside [`super::invalid_parameter_at`] for, so neither form words the clause itself.
pub fn encryption_key_not_for_type_clause(encryption_type: &str) -> String {
    super::parameter_constraint(
        ENCRYPTION_CONFIGURATION_PARAMETER,
        &encryption_key_not_for_type_constraint(encryption_type),
    )
}

/// The refusal of a request that pairs exclusion filters with an `imageTagMutability`
/// setting that has no exclusion to apply: the filter list is the parameter at fault, and
/// the constraint names the setting it was sent beside.
///
/// `setting` is the setting the request named, so the sentence states the one the caller
/// asked for — `MUTABLE`, which excludes nothing from overwriting, or `IMMUTABLE`, which
/// excludes nothing from protection.
pub fn exclusion_filters_not_allowed(setting: &str) -> Fault {
    super::invalid_parameter_at(
        EXCLUSION_FILTERS_PARAMETER,
        &format!(
            "ImageTagMutabilityExclusionFilters are not allowed when imageTagMutability is \
             set as {setting}"
        ),
    )
}

/// The constraint ECR states for a member holding an ARN that is not one: the KIND of ARN
/// the member carries and the value that is not one, rather than an expression quoted back
/// at the caller.
///
/// The one wording of that sentence, so the boundary walk over a member's `@pattern`
/// ([`crate::service`]) and a handler judging a member the model gives no pattern state it
/// alike. `kind` is the kind of ARN in words (`custom role`, `signing profile`), which the
/// boundary walk derives from the member's own name.
pub fn malformed_arn_constraint(kind: &str, value: &str) -> String {
    format!("The {kind} ARN '{value}' must be well formed")
}
