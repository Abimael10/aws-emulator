// SPDX-License-Identifier: CC0-1.0

//! Applying the PATCH wrappers this service's update operations take.
//!
//! An update of a registry or of a registry record "uses PATCH semantics: specify only the
//! fields you want to change, and omit the rest to leave them unchanged". A field whose
//! value is itself optional is carried in a WRAPPER so that the three cases stay
//! distinguishable on the wire — leave it alone, set it, unset it — and the model declares
//! two kinds of wrapper:
//!
//! * a LEAF wrapper, whose `optionalValue` is the value to set ("omit the wrapper to leave
//!   the field unchanged"), applied by [`patch_optional`]; and
//! * a wrapper over a STRUCTURE, whose own members are each independently patchable,
//!   applied by [`patch_structure`].
//!
//! Both are written once here, so every operation of this service that takes a patch gives
//! an omitted member, a supplied one and an empty wrapper the same meaning.

/// Apply a LEAF patch wrapper to the optional field it patches, which is what the model's
/// "Omit to leave the field unchanged; provide an empty wrapper to unset it" means: no
/// wrapper at all (`None`) leaves the stored value, a wrapper carrying a value replaces
/// it, and a wrapper carrying none unsets it.
pub fn patch_optional<T>(field: &mut Option<T>, wrapper: Option<Option<T>>) {
    if let Some(value) = wrapper {
        *field = value;
    }
}

/// Apply a STRUCTURE patch wrapper: a wrapper carrying no value clears the structure
/// ("supply an empty object to clear them"), and one carrying a per-field patch applies
/// `apply` to the stored structure — materialising it first when the resource holds none,
/// so a patch can fill a structure that was never set.
pub fn patch_structure<T: Default, P>(
    field: &mut Option<T>,
    wrapper: Option<Option<P>>,
    apply: impl FnOnce(&mut T, P),
) {
    match wrapper {
        None => {}
        Some(None) => *field = None,
        Some(Some(patch)) => {
            let mut current = field.take().unwrap_or_default();
            apply(&mut current, patch);
            *field = Some(current);
        }
    }
}
