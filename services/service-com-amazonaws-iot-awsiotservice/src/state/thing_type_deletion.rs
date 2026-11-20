// SPDX-License-Identifier: CC0-1.0

//! WHEN a thing type may be DELETED, and how a delete that may not happen is refused.
//!
//! `DeleteThingType` is documented with the procedure it enforces: "You cannot delete a
//! thing type if it has things associated with it. To delete a thing type, first mark it as
//! deprecated by calling `DeprecateThingType`, then remove any associated things by calling
//! `UpdateThing` to change the thing type on any associated thing, and finally use
//! `DeleteThingType` to delete the thing type." Each step of it is a precondition on the
//! records the account holds, so each is read here — against the type's own deprecation
//! instant ([`super::RegisteredThingType::deprecation_date`]) and against the things the
//! registry holds — and every one of them is decided BEFORE anything is removed, so a
//! refused delete leaves the type exactly as it was, still (un)deprecated.
//!
//! The deprecation also has to have had TIME to take effect: AWS IoT requires a wait after
//! a type is deprecated before it can be deleted ([`DEPRECATION_DELETION_WAIT`]), and the
//! wait is measured from the recorded deprecation instant against the clock, so nothing is
//! slept for and a reader of the type observes the same record throughout.

use std::time::{Duration, SystemTime};

use super::{not_found, ComAmazonawsIotAwsiotserviceState};

/// How long after a thing type is DEPRECATED it may be deleted.
///
/// AWS IoT states the wait in the refusal it answers an earlier delete with — "Please wait
/// for 5 minutes after deprecation and then retry" — so the duration is the service's own
/// documented one rather than a span this emulator chose, and the sentence below states it
/// from this constant so the two cannot drift.
pub const DEPRECATION_DELETION_WAIT: Duration = Duration::from_secs(5 * 60);

/// Why a thing type cannot be deleted — what
/// [`ComAmazonawsIotAwsiotserviceState::delete_thing_type`] answers instead of removing the
/// record.
///
/// It is the registry's reason, not a wire error: the handler maps it onto the error its own
/// model declares, and the wording lives here so one rule is stated once.
#[derive(Debug, Clone)]
pub enum ThingTypeDeletionRefusal {
    /// `thingTypeName` names no thing type this account holds.
    NotRegistered(String),
    /// The type was never deprecated (or its deprecation was undone), so the first step of
    /// the documented procedure has not been taken.
    NotDeprecated(String),
    /// The type IS deprecated, but [`DEPRECATION_DELETION_WAIT`] has not passed since.
    WithinDeprecationWait(String),
    /// Things in the registry are still associated with the type, so deleting it would
    /// leave those records pointing at a type that does not exist.
    AssociatedThings(String),
}

impl ThingTypeDeletionRefusal {
    /// What the refusal reports, in the wording AWS IoT states each of these in.
    pub fn message(&self) -> String {
        match self {
            Self::NotRegistered(thing_type_name) => {
                not_found::missing_thing_type_message(thing_type_name)
            }
            Self::NotDeprecated(thing_type_name) => {
                format!("Cannot delete non deprecated thing type {thing_type_name}")
            }
            Self::WithinDeprecationWait(thing_type_name) => format!(
                "Cannot delete thing type : {thing_type_name}. Please wait for {} minutes \
                 after deprecation and then retry",
                DEPRECATION_DELETION_WAIT.as_secs() / 60
            ),
            // The clause states the rule the operation's own documentation states, in the
            // form this family words a refusal about one thing type in: the family noun,
            // the name the request addressed, and what has to change before a retry can
            // succeed. How MANY things are associated is not part of it.
            Self::AssociatedThings(thing_type_name) => format!(
                "Cannot delete thing type : {thing_type_name}. There are things associated \
                 with this thing type"
            ),
        }
    }
}

impl ComAmazonawsIotAwsiotserviceState {
    /// The names of the things whose `thingTypeName` is `thing_type_name`, in registry
    /// order.
    ///
    /// A thing's association with a type is the thing's own
    /// [`super::ThingConfiguration::thing_type_name`], so the association is derived from
    /// the thing records and never recorded beside the type: `CreateThing`, the bulk
    /// provisioning path and `UpdateThing` all write that one member, and this reads it.
    pub fn things_of_thing_type(&self, thing_type_name: &str) -> Vec<String> {
        self.things
            .values()
            .filter(|thing| {
                thing
                    .configuration
                    .thing_type_name
                    .as_deref()
                    .is_some_and(|associated| associated == thing_type_name)
            })
            .map(|thing| thing.thing_name.clone())
            .collect()
    }

    /// Remove the thing type registered as `thing_type_name`, as of `now` — the ONE write
    /// that makes a thing type stop existing.
    ///
    /// Every precondition of the documented procedure is read first, so a refused delete
    /// writes nothing: the type has to be one the account holds, it has to have been
    /// deprecated, that deprecation has to be at least [`DEPRECATION_DELETION_WAIT`] old,
    /// and no thing may still be associated with it. The removal is the record leaving the
    /// registry, which is what makes every later reader of it — the describe path, the
    /// listings, and `CreateThing`'s own resolution of a `thingTypeName` — stop reporting
    /// it.
    pub fn delete_thing_type(
        &mut self,
        thing_type_name: &str,
        now: SystemTime,
    ) -> Result<(), ThingTypeDeletionRefusal> {
        let Some(thing_type) = self.thing_types.get(thing_type_name) else {
            return Err(ThingTypeDeletionRefusal::NotRegistered(
                thing_type_name.to_string(),
            ));
        };
        let Some(deprecated_at) = thing_type.deprecation_date else {
            return Err(ThingTypeDeletionRefusal::NotDeprecated(
                thing_type.thing_type_name.clone(),
            ));
        };
        // The wait is read from the recorded instant against the clock the call runs at. A
        // deprecation whose instant is somehow ahead of `now` has not waited either, which
        // is what the `is_ok_and` reading of the elapsed duration says.
        let waited = now
            .duration_since(deprecated_at)
            .is_ok_and(|elapsed| elapsed >= DEPRECATION_DELETION_WAIT);
        if !waited {
            return Err(ThingTypeDeletionRefusal::WithinDeprecationWait(
                thing_type.thing_type_name.clone(),
            ));
        }
        if !self.things_of_thing_type(thing_type_name).is_empty() {
            return Err(ThingTypeDeletionRefusal::AssociatedThings(
                thing_type.thing_type_name.clone(),
            ));
        }
        self.thing_types.remove(thing_type_name);
        Ok(())
    }
}
