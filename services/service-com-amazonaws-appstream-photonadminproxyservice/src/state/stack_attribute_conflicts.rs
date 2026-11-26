// SPDX-License-Identifier: CC0-1.0

//! The refusal a request that both SETS and DELETES one attribute of a resource carries.
//!
//! `UpdateStack` and `UpdateAppBlockBuilder` each take the members that set their
//! resource's configuration AND an `AttributesToDelete` naming the attributes to clear, so
//! one request can ask for two different outcomes for one attribute. That combination is
//! refused rather than resolved one way, and the sentence it is refused with names the
//! CONFIGURATION in prose — not the request member that sets it and not the enum value that
//! clears it — so the sentence is stated here, once, over the attribute the request named
//! twice.
//!
//! Which member sets which attribute stays with the operation that reads the request
//! (`crate::ops::update_stack`, `crate::ops::update_app_block_builder`); what the refusal
//! SAYS is a property of the service, and so lives beside the other refusal sentences this
//! store words.

/// The words AWS's sentence names the configuration an attribute clears, for the
/// attributes whose wording is not the attribute's own words read as prose.
///
/// The `ACCESS_ENDPOINTS` attribute clears the interface VPC endpoints a stack is reached
/// through, and AWS's sentence names them as such ("vpc endpoints") rather than after the
/// attribute. Every other `StackAttribute` is named by its own words
/// ([`attribute_prose`]).
const STATED_ATTRIBUTE_PROSE: &[(&str, &str)] = &[("ACCESS_ENDPOINTS", "vpc endpoints")];

/// The configuration `attribute` names, in the words AWS's refusal states it with: the
/// wording stated above where there is one, and otherwise the attribute's own words — the
/// attribute enum value (`StackAttribute` for a stack, `AppBlockBuilderAttribute` for an
/// app block builder, kept as `STACK_ATTRIBUTE_VALUES` and
/// `APP_BLOCK_BUILDER_ATTRIBUTE_VALUES` in `ops/types/shared_types.rs`) read as prose,
/// lower case with the separators spelled as spaces.
fn attribute_prose(attribute: &str) -> String {
    STATED_ATTRIBUTE_PROSE
        .iter()
        .find(|(value, _)| *value == attribute)
        .map_or_else(
            || attribute.to_lowercase().replace('_', " "),
            |(_, prose)| (*prose).to_string(),
        )
}

/// AWS's `InvalidParameterCombinationException` sentence for a request that sets the
/// configuration `attribute` names and deletes it in the same call.
///
/// Fixed text apart from the configuration the attribute names ([`attribute_prose`]); the
/// apostrophe is the right single quotation mark (U+2019) AWS's own sentence carries, not
/// the ASCII one.
pub fn attribute_set_and_deleted_message(attribute: &str) -> String {
    format!(
        "You can’t update and delete {} at the same time.",
        attribute_prose(attribute)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sentence states one configuration per attribute, in prose, under one template:
    /// the rows differ along how many words the attribute carries and whether its wording
    /// is the stated one or its own words.
    #[test]
    fn every_attribute_is_named_in_prose_under_one_template() {
        for (attribute, expected) in [
            (
                "ACCESS_ENDPOINTS",
                "You can’t update and delete vpc endpoints at the same time.",
            ),
            (
                "USER_SETTINGS",
                "You can’t update and delete user settings at the same time.",
            ),
            (
                "REDIRECT_URL",
                "You can’t update and delete redirect url at the same time.",
            ),
            (
                "STORAGE_CONNECTOR_HOMEFOLDERS",
                "You can’t update and delete storage connector homefolders at the same \
                 time.",
            ),
        ] {
            let stated = attribute_set_and_deleted_message(attribute);
            assert_eq!(
                stated, expected,
                "case {attribute:?}: expected {expected:?}, got {stated:?}"
            );
        }
    }

    /// Every `StackAttribute` the model declares has a sentence, and it is one sentence:
    /// the template's fixed text with a non-empty configuration named in it.
    #[test]
    fn every_modelled_stack_attribute_has_a_sentence() {
        for attribute in crate::ops::types::STACK_ATTRIBUTE_VALUES {
            let stated = attribute_set_and_deleted_message(attribute);
            let named = stated
                .strip_prefix("You can’t update and delete ")
                .and_then(|rest| rest.strip_suffix(" at the same time."));
            let named = named.unwrap_or_else(|| {
                panic!("case {attribute:?}: expected AWS's template, got {stated:?}")
            });
            assert!(
                !named.is_empty() && named == named.to_lowercase(),
                "case {attribute:?}: expected a configuration named in lower-case prose, \
                 got {named:?}"
            );
        }
    }
}
