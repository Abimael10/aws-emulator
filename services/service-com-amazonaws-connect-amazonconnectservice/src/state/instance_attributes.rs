// SPDX-License-Identifier: CC0-1.0

//! An instance's feature toggles: the `Attribute` values `UpdateInstanceAttribute`
//! writes, `DescribeInstanceAttribute` reads back and `ListInstanceAttributes`
//! enumerates.
//!
//! One concern of its own, so its own file, re-exported so a handler reaches it through
//! `crate::state` like everything else there.
//!
//! The modelled `Attribute` is "a toggle for an individual feature at the instance
//! level" (`smithy_query(query="shape",shape="Attribute")`), so its value is one of the
//! two booleans a toggle takes and nothing else.
//!
//! A toggle nobody has updated is NOT stored: it is DERIVED, so an instance reports a
//! value for every attribute type the model declares from the moment it exists, and
//! reading one stores nothing. Two of those types — the call directions — are the
//! instance's OWN flags rather than a second copy of them: `CreateInstance` takes
//! `InboundCallsEnabled` / `OutboundCallsEnabled`, `DescribeInstance` reports them, and
//! this file reads and writes those same fields for `INBOUND_CALLS` and
//! `OUTBOUND_CALLS` so a caller cannot see the toggle and the instance disagree.

use std::collections::BTreeSet;

use super::{invalid_enum_value, InstanceRecord};
use crate::ops::types::{Attribute, InvalidRequestException, INSTANCE_ATTRIBUTE_TYPE_VALUES};

/// The value an enabled toggle carries, as Connect words a boolean instance attribute
/// on the wire: `InstanceAttributeValue` is a string, and the values AWS answers for an
/// instance's toggles are `true` and `false`.
pub const INSTANCE_ATTRIBUTE_ENABLED: &str = "true";
/// See [`INSTANCE_ATTRIBUTE_ENABLED`].
pub const INSTANCE_ATTRIBUTE_DISABLED: &str = "false";

/// The `InstanceAttributeType` member whose toggle IS the instance's inbound-calls flag,
/// from `com.amazonaws.connect#InstanceAttributeType`.
pub const INBOUND_CALLS_ATTRIBUTE: &str = "INBOUND_CALLS";
/// The outbound half of [`INBOUND_CALLS_ATTRIBUTE`].
pub const OUTBOUND_CALLS_ATTRIBUTE: &str = "OUTBOUND_CALLS";

/// The `InstanceAttributeType` members a new instance carries ENABLED, from the toggle
/// set AWS answers for a freshly created instance (`AUTO_RESOLVE_BEST_VOICES`,
/// `CONTACT_LENS` and `EARLY_MEDIA` are on; the other optional features are off until a
/// caller turns them on).
///
/// The call directions are not here: they are the instance's own flags, so their
/// default is whatever `CreateInstance` was given (see
/// [`InstanceRecord::instance_attribute_value`]).
pub const ATTRIBUTES_ENABLED_BY_DEFAULT: &[&str] =
    &["AUTO_RESOLVE_BEST_VOICES", "CONTACT_LENS", "EARLY_MEDIA"];

/// The name Connect publishes for an instance attribute's value member, as the model
/// spells it (`UpdateInstanceAttributeRequest$Value`) — what a refusal of that member
/// names.
pub const INSTANCE_ATTRIBUTE_VALUE_PARAMETER: &str = "Value";

/// The name Connect publishes for an instance attribute's TYPE member, as the model
/// spells it (`DescribeInstanceAttributeRequest$AttributeType`) — what a refusal of that
/// member names.
pub const INSTANCE_ATTRIBUTE_TYPE_PARAMETER: &str = "AttributeType";

/// The page size `ListInstanceAttributes` uses when the caller sends no `MaxResults`:
/// its page-size member documents no default
/// (`smithy_query(query="operation",operation="ListInstanceAttributes")` -> "The maximum
/// number of results to return per page."), so the modelled maximum of the shape it
/// targets stands in (`MaxResult7` -> range 1..7), as every other `List*` of this service
/// does for a page-size member that documents no default.
///
/// A CONVENTION, not a recorded value: no recorded answer to this operation was asked
/// without a page size, so what AWS pages an unpaginated caller at is unverified here. A
/// caller that sends `MaxResults` is served the size it asked for, which is the part the
/// recorded answer does establish.
pub const LIST_INSTANCE_ATTRIBUTES_DEFAULT_PAGE_SIZE: usize = 7;

/// The modelled `@range` minimum of `com.amazonaws.connect#MaxResult7`, the shape
/// `ListInstanceAttributes`'s `MaxResults` targets
/// (`smithy_query(query="operation",operation="ListInstanceAttributes")` -> range min 1
/// max 7).
///
/// Only the minimum is a check on the REQUEST, for the reason
/// [`super::LIST_INSTANCES_MAX_RESULTS_MIN`] records for the page-size member whose two
/// ends are recorded: Connect refuses a page size below the minimum and SERVES one above
/// the maximum. Here the served page is what says so — AWS answers this operation with a
/// page holding more attributes than `MaxResult7`'s maximum — so the maximum bounds
/// neither the request nor the page, and the caller's own page size is honoured.
pub const LIST_INSTANCE_ATTRIBUTES_MAX_RESULTS_MIN: i32 = 1;

/// The instance attribute types AWS reports for an instance that the modelled
/// `com.amazonaws.connect#InstanceAttributeType` enum does NOT declare.
///
/// That enum is not the whole toggle set: the listing AWS answers for an instance carries
/// these feature toggles beside the modelled ones, so an instance reporting only the
/// modelled members would under-report its own features — and a caller that enumerated
/// them could not then read one back. Written by hand because the model is where this
/// service's shapes come from and the model is behind the service here; the served set is
/// the union of the two ([`InstanceRecord::instance_attributes_in_order`]).
///
/// They are OFF for a new instance, like every other optional feature
/// ([`ATTRIBUTES_ENABLED_BY_DEFAULT`] names the ones that are on).
///
/// The OBSERVED set to date, not an authoritative one: it holds the types answers of this
/// service have shown beyond the enum, and a type a later answer reveals is added here.
/// Being short a type under-reports an instance's features; it can never make the emulator
/// report a value it did not derive, since every value comes from
/// [`InstanceRecord::instance_attribute_value`].
pub const INSTANCE_ATTRIBUTE_TYPES_BEYOND_THE_MODEL: &[&str] = &[
    "AUTOMATED_INTERACTION_LOG",
    "BOT_MANAGEMENT",
    "ENABLE_BOT_ANALYTICS_AND_TRANSCRIPTS",
];

/// Whether `attribute_type` names a feature toggle of an instance at all: a member of the
/// modelled `InstanceAttributeType` enum
/// (`smithy_query(query="shape",shape="InstanceAttributeType")`), or one of the types the
/// service reports beyond it ([`INSTANCE_ATTRIBUTE_TYPES_BEYOND_THE_MODEL`]).
///
/// The two sets together are what an instance reports, so they are also what it can be
/// asked about: every type the listing serves reads back through
/// `DescribeInstanceAttribute`, and nothing else names a toggle.
pub fn is_instance_attribute_type(attribute_type: &str) -> bool {
    INSTANCE_ATTRIBUTE_TYPE_VALUES.contains(&attribute_type)
        || INSTANCE_ATTRIBUTE_TYPES_BEYOND_THE_MODEL.contains(&attribute_type)
}

/// How an operation that addresses ONE instance attribute refuses an `AttributeType` that
/// names no attribute type, or `None` for one it can act on.
///
/// One function, so every operation taking that member applies ONE rule ahead of any
/// lookup: a value that names no toggle of an instance is a value the service cannot read,
/// answered as the modelled `InvalidRequestException` (HTTP 400) in the fixed sentence
/// [`invalid_enum_value`] states for the member — which is also what keeps the refusal
/// inside the errors these operations model.
pub fn instance_attribute_type_refusal(attribute_type: &str) -> Option<InvalidRequestException> {
    (!is_instance_attribute_type(attribute_type))
        .then(|| invalid_enum_value(INSTANCE_ATTRIBUTE_TYPE_PARAMETER))
}

/// Whether `value` is one of the two values a feature toggle takes.
///
/// The model bounds `InstanceAttributeValue` by length alone, so this is the rule the
/// handler applies on top of it: anything else is not a toggle setting at all.
pub fn is_instance_attribute_toggle(value: &str) -> bool {
    value == INSTANCE_ATTRIBUTE_ENABLED || value == INSTANCE_ATTRIBUTE_DISABLED
}

/// A toggle's value for a flag that is `enabled`, or not.
pub fn instance_attribute_toggle(enabled: bool) -> &'static str {
    if enabled {
        INSTANCE_ATTRIBUTE_ENABLED
    } else {
        INSTANCE_ATTRIBUTE_DISABLED
    }
}

/// The value `attribute_type` carries while nobody has updated it and the instance holds
/// no flag of its own for it: enabled for the types in
/// [`ATTRIBUTES_ENABLED_BY_DEFAULT`], disabled for every other one.
pub fn default_instance_attribute_value(attribute_type: &str) -> &'static str {
    instance_attribute_toggle(ATTRIBUTES_ENABLED_BY_DEFAULT.contains(&attribute_type))
}

impl InstanceRecord {
    /// The value this instance's `attribute_type` toggle holds.
    ///
    /// Derived, never stored on read: the call directions come off the instance's own
    /// flags, an updated toggle comes out of [`InstanceRecord::instance_attributes`], and
    /// anything else is that type's default — so every attribute type an instance holds
    /// has a value, and reading one changes nothing a later caller sees.
    pub fn instance_attribute_value(&self, attribute_type: &str) -> String {
        match attribute_type {
            INBOUND_CALLS_ATTRIBUTE => {
                instance_attribute_toggle(self.inbound_calls_enabled).to_string()
            }
            OUTBOUND_CALLS_ATTRIBUTE => {
                instance_attribute_toggle(self.outbound_calls_enabled).to_string()
            }
            other => self
                .instance_attributes
                .get(other)
                .cloned()
                .unwrap_or_else(|| default_instance_attribute_value(other).to_string()),
        }
    }

    /// The modelled `Attribute` shape `DescribeInstanceAttribute` returns for
    /// `attribute_type`.
    pub fn instance_attribute(&self, attribute_type: &str) -> Attribute {
        Attribute {
            attribute_type: Some(attribute_type.to_string()),
            value: Some(self.instance_attribute_value(attribute_type)),
        }
    }

    /// Every attribute type this instance holds a value for, each paired with the
    /// modelled `Attribute` that reports it, in ascending attribute-type order.
    ///
    /// That order is the key a page cursor is cut on, and it is the order AWS answers
    /// `ListInstanceAttributes` in. The set is every type the service reports for an
    /// instance — the modelled enum's members and
    /// [`INSTANCE_ATTRIBUTE_TYPES_BEYOND_THE_MODEL`] — each with the value
    /// [`InstanceRecord::instance_attribute_value`] derives, so the listing and the
    /// single-attribute read can never disagree, together with any other type an update
    /// stored. An instance reports its WHOLE toggle set, not just the part a caller has
    /// written, and the set is derived on every call, so listing it stores nothing.
    pub fn instance_attributes_in_order(&self) -> Vec<(String, Attribute)> {
        let mut types: BTreeSet<&str> = INSTANCE_ATTRIBUTE_TYPE_VALUES.iter().copied().collect();
        types.extend(INSTANCE_ATTRIBUTE_TYPES_BEYOND_THE_MODEL.iter().copied());
        types.extend(self.instance_attributes.keys().map(String::as_str));
        types
            .into_iter()
            .map(|attribute_type| {
                (
                    attribute_type.to_string(),
                    self.instance_attribute(attribute_type),
                )
            })
            .collect()
    }

    /// Set this instance's `attribute_type` toggle to `value`.
    ///
    /// The call directions are written to the instance's own flags rather than beside
    /// them, so `DescribeInstance` reports what the toggle was last set to; every other
    /// type is stored by its name.
    pub fn set_instance_attribute(&mut self, attribute_type: &str, value: &str) {
        let enabled = value == INSTANCE_ATTRIBUTE_ENABLED;
        match attribute_type {
            INBOUND_CALLS_ATTRIBUTE => self.inbound_calls_enabled = enabled,
            OUTBOUND_CALLS_ATTRIBUTE => self.outbound_calls_enabled = enabled,
            other => {
                self.instance_attributes
                    .insert(other.to_string(), value.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::INSTANCE_ATTRIBUTE_TYPE_VALUES;

    /// The default-enabled list is written by hand, so it is pinned to the model's own
    /// value list: a name the enum does not declare is a stale list, not a defect in an
    /// instance.
    #[test]
    fn every_default_enabled_type_is_a_modelled_attribute_type() {
        for attribute_type in ATTRIBUTES_ENABLED_BY_DEFAULT {
            assert!(
                INSTANCE_ATTRIBUTE_TYPE_VALUES.contains(attribute_type),
                "case {attribute_type:?}: InstanceAttributeType does not declare it"
            );
        }
    }

    /// Only the two booleans are toggle settings.
    #[test]
    fn a_toggle_takes_one_of_two_values() {
        for (value, accepted) in [
            ("true", true),
            ("false", true),
            ("True", false),
            ("TRUE", false),
            ("1", false),
            ("yes", false),
            ("", false),
            (" true", false),
        ] {
            let actual = is_instance_attribute_toggle(value);
            assert_eq!(
                actual, accepted,
                "case {value:?}: expected accepted={accepted}, got {actual}"
            );
        }
    }

    /// Every type the service reports for an instance names an attribute type, and the
    /// refusal of anything else is the plane's one sentence.
    #[test]
    fn only_a_reported_type_names_an_attribute_type() {
        for attribute_type in INSTANCE_ATTRIBUTE_TYPE_VALUES
            .iter()
            .chain(INSTANCE_ATTRIBUTE_TYPES_BEYOND_THE_MODEL.iter())
        {
            assert!(
                is_instance_attribute_type(attribute_type),
                "case {attribute_type:?}: a reported type names an attribute type"
            );
            assert!(
                instance_attribute_type_refusal(attribute_type).is_none(),
                "case {attribute_type:?}: a reported type is not refused"
            );
        }
        for attribute_type in [
            "NOT_AN_ATTRIBUTE",
            "inbound_calls",
            "Contact_Lens",
            "CONTACT_LENSX",
            "EARLY MEDIA",
            "_",
            "",
        ] {
            assert!(
                !is_instance_attribute_type(attribute_type),
                "case {attribute_type:?}: no attribute type of an instance is named by it"
            );
            let actual = instance_attribute_type_refusal(attribute_type)
                .and_then(|refusal| refusal.message);
            assert_eq!(
                actual.as_deref(),
                Some("Invalid AttributeType"),
                "case {attribute_type:?}: expected the plane's fixed sentence, got {actual:?}"
            );
        }
    }

    /// The types reported beyond the model are exactly that: a name the enum DOES declare
    /// does not belong in that list, or the served set would carry it twice.
    #[test]
    fn the_types_beyond_the_model_are_not_modelled() {
        for attribute_type in INSTANCE_ATTRIBUTE_TYPES_BEYOND_THE_MODEL {
            assert!(
                !INSTANCE_ATTRIBUTE_TYPE_VALUES.contains(attribute_type),
                "case {attribute_type:?}: InstanceAttributeType declares it, so it is modelled"
            );
        }
    }

    /// Every modelled type has a default, and the call directions are not defaulted here
    /// at all — they follow the instance's own flags.
    #[test]
    fn every_other_type_defaults_to_the_feature_being_off() {
        for attribute_type in INSTANCE_ATTRIBUTE_TYPE_VALUES {
            let expected = if ATTRIBUTES_ENABLED_BY_DEFAULT.contains(attribute_type) {
                INSTANCE_ATTRIBUTE_ENABLED
            } else {
                INSTANCE_ATTRIBUTE_DISABLED
            };
            let actual = default_instance_attribute_value(attribute_type);
            assert_eq!(
                actual, expected,
                "case {attribute_type:?}: expected {expected:?}, got {actual:?}"
            );
        }
        assert!(
            !ATTRIBUTES_ENABLED_BY_DEFAULT.contains(&INBOUND_CALLS_ATTRIBUTE)
                && !ATTRIBUTES_ENABLED_BY_DEFAULT.contains(&OUTBOUND_CALLS_ATTRIBUTE),
            "the call directions follow the instance's own flags, so they carry no \
             default of their own"
        );
    }
}
