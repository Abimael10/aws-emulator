// SPDX-License-Identifier: CC0-1.0

//! An instance's feature toggles: the `Attribute` values `UpdateInstanceAttribute`
//! writes and `DescribeInstanceAttribute` reads back.
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

use super::InstanceRecord;
use crate::ops::types::Attribute;

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
    /// anything else is that type's default — so every attribute type the model declares
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
