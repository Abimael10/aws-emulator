// SPDX-License-Identifier: CC0-1.0

//! The restore testing SELECTIONS of a restore testing plan: what
//! `CreateRestoreTestingSelection` stores inside the plan it names, and what
//! every call that addresses a selection by name resolves against.
//!
//! # The rules this module holds
//!
//! * a selection belongs to ONE restore testing plan and is addressed by name
//!   within it — the name "cannot be changed after creation" (model doc for
//!   `RestoreTestingSelectionForList$RestoreTestingSelectionName`) — so the
//!   store is keyed by the (plan name, selection name) pair and a create naming
//!   a selection that plan already holds is refused as already existing
//!   ([`restore_testing_selection_already_exists`]);
//! * a create naming a plan the account does not hold resolves nothing to store
//!   the selection in, which is the plan's own not-found condition
//!   ([`super::restore_testing_plan_not_found`]), and a selection name the named
//!   plan does not hold is this resource's
//!   ([`restore_testing_selection_not_found`]);
//! * a selection is "RestoreTestingSelectionName, ProtectedResourceType, and one
//!   of the following: ProtectedResourceArns ProtectedResourceConditions …
//!   Cannot select by both protected resource types AND specific ARNs. Request
//!   will fail if both are included" (the operation's own model doc), which is
//!   the reading [`rejected_restore_testing_selection_body`] applies before
//!   anything is stored;
//! * the body is stored EXACTLY as submitted: a collection the request states as
//!   empty is stored empty and reported empty, and one it states nothing for is
//!   stored and reported as absent — the members are optional with no modelled
//!   default, so emptiness and absence are two different answers and the read
//!   reports the one the create carried.

use std::collections::HashMap;
use std::time::SystemTime;

use super::ComAmazonawsBackupCryocontrollerusermanagerState;
use crate::ops::types::{
    AlreadyExistsException, InvalidParameterValueException, LimitExceededException,
    MissingParameterValueException, ProtectedResourceConditions, ResourceNotFoundException,
    RestoreTestingSelectionForGet,
};

/// The most restore testing selections this emulator keeps for one plan.
///
/// An EMULATOR-CHOSEN ceiling, not an AWS quota, for the reason given on
/// [`super::MAX_RESTORE_TESTING_PLANS`]: the store is in memory and every
/// request can add to it. AWS caps the selections one restore testing plan may
/// hold too, and answers the same modelled `LimitExceededException` for a create
/// past it.
pub const MAX_RESTORE_TESTING_SELECTIONS_PER_PLAN: usize = 500;

/// The most specific protected resource ARNs one selection may name: "you can
/// include up to 30 specific protected resource ARNs in ProtectedResourceArns"
/// (model doc for `CreateRestoreTestingSelection`).
pub const MAX_PROTECTED_RESOURCE_ARNS: usize = 30;

/// The `ProtectedResourceArns` entry that selects EVERY protected resource of
/// the selection's type rather than one named resource: "you can include a
/// wildcard: `ProtectedResourceArns: ["*"]`, but not both" (model doc for the
/// member).
pub const PROTECTED_RESOURCE_ARNS_WILDCARD: &str = "*";

/// The least and the most hours a stated validation window may be: "This is
/// amount of hours (0 to 168) available to run a validation script on the data"
/// (model doc for `RestoreTestingSelectionForCreate$ValidationWindowHours`).
/// The member carries a modelled default of `0`, so a request that states
/// nothing for it states the floor of that range.
pub const MIN_VALIDATION_WINDOW_HOURS: i32 = 0;
pub const MAX_VALIDATION_WINDOW_HOURS: i32 = 168;

/// The most characters a `RestoreTestingSelectionName` may carry: "The name
/// consists of only alphanumeric characters and underscores. Maximum length is
/// 50" (model doc for
/// `RestoreTestingSelectionForCreate$RestoreTestingSelectionName`).
pub const MAX_RESTORE_TESTING_SELECTION_NAME_LENGTH: usize = 50;

/// The sentence AWS states for "the restore testing selection asked about is not
/// one the named plan holds" — fixed, and worded as this service words the same
/// condition for the plan that holds it
/// ([`super::MESSAGE_RESTORE_TESTING_PLAN_NOT_FOUND`]): it names no part of the
/// request, so the name asked about does not appear in it. No `Code`: this
/// condition has no observed token.
pub const MESSAGE_RESTORE_TESTING_SELECTION_NOT_FOUND: &str =
    "Cannot find restore testing selection";

/// The sentence AWS states for "a restore testing selection of this name already
/// exists in this plan", worded as this service words that condition for every
/// create of a named resource — a backup vault's
/// ([`super::MESSAGE_VAULT_ALREADY_EXISTS`]), a backup plan's, one of its
/// selections', a restore testing plan's
/// ([`super::MESSAGE_RESTORE_TESTING_PLAN_ALREADY_EXISTS`]): fixed text that
/// names no part of the request. No `Code`: this condition has no observed
/// token.
pub const MESSAGE_RESTORE_TESTING_SELECTION_ALREADY_EXISTS: &str =
    "Restore testing selection with the same name already exists";

/// A restore testing selection: what `CreateRestoreTestingSelection` stores and
/// `GetRestoreTestingSelection` reports.
///
/// The body is kept as the create submitted it — including which of its optional
/// collections the request stated at all — so a read reports what was stored
/// rather than a value recomputed per call; the instant only this service can
/// know, when the selection was stored, is its own member.
#[derive(Debug, Clone)]
pub struct RestoreTestingSelectionRecord {
    /// The plan this selection belongs to, the other half of the key it is
    /// stored under: a selection's name identifies it within ONE restore testing
    /// plan, so the same name under another plan is another selection.
    pub plan_name: String,
    pub name: String,
    /// The role a restore test started from this selection restores as, stored
    /// as submitted.
    pub iam_role_arn: String,
    pub protected_resource_type: String,
    /// The resources this selection restores, as the create stated them:
    /// `None` where it stated nothing, and `Some` of what it stated otherwise —
    /// an empty list included.
    pub protected_resource_arns: Option<Vec<String>>,
    /// The tag conditions this selection filters protected resources by, as the
    /// create stated them, `None` where it stated nothing.
    pub protected_resource_conditions: Option<ProtectedResourceConditions>,
    /// The restore metadata keys this selection overrides, as the create stated
    /// them, `None` where it stated nothing.
    pub restore_metadata_overrides: Option<HashMap<String, String>>,
    pub validation_window_hours: i32,
    pub creator_request_id: Option<String>,
    pub creation_time: SystemTime,
}

impl RestoreTestingSelectionRecord {
    /// The selection as a READ reports it — the ONE projection of a stored
    /// selection, so a read can report nothing the create did not store.
    pub fn for_get(&self) -> RestoreTestingSelectionForGet {
        RestoreTestingSelectionForGet {
            restore_testing_plan_name: self.plan_name.clone(),
            restore_testing_selection_name: self.name.clone(),
            iam_role_arn: self.iam_role_arn.clone(),
            protected_resource_type: self.protected_resource_type.clone(),
            protected_resource_arns: self.protected_resource_arns.clone(),
            protected_resource_conditions: self.protected_resource_conditions.clone(),
            restore_metadata_overrides: self.restore_metadata_overrides.clone(),
            validation_window_hours: self.validation_window_hours,
            creator_request_id: self.creator_request_id.clone(),
            creation_time: super::modelled_timestamp(self.creation_time),
        }
    }
}

/// The modelled refusal for a `RestoreTestingSelectionName` the named restore
/// testing plan holds no selection under — see
/// [`MESSAGE_RESTORE_TESTING_SELECTION_NOT_FOUND`].
///
/// Only that condition: a `RestoreTestingPlanName` that names no plan of this
/// account is a DIFFERENT condition, which the plan's own refusal states
/// ([`super::restore_testing_plan_not_found`]), so a client can tell "no such
/// selection of this plan" from "no such plan".
pub fn restore_testing_selection_not_found() -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(MESSAGE_RESTORE_TESTING_SELECTION_NOT_FOUND.to_string()),
        ..Default::default()
    }
}

/// The modelled refusal for a create naming a restore testing selection the
/// named plan already holds.
///
/// `Arn` and `CreatorRequestId` are left UNSET for the reason given on
/// [`super::vault_already_exists`].
pub fn restore_testing_selection_already_exists() -> AlreadyExistsException {
    AlreadyExistsException {
        message: Some(MESSAGE_RESTORE_TESTING_SELECTION_ALREADY_EXISTS.to_string()),
        ..Default::default()
    }
}

/// The modelled refusal for a selection create that states neither of the two
/// members a selection selects its resources with.
///
/// A selection "consists of RestoreTestingSelectionName, ProtectedResourceType,
/// and one of the following: ProtectedResourceArns ProtectedResourceConditions"
/// (model doc for `CreateRestoreTestingSelection`), and neither member carries
/// the `@required` trait, so the boundary validator passes a request stating
/// neither while a selection that states neither selects no resource at all. The
/// sentence is the one this service states for the same class of condition — a
/// create whose selection selects nothing
/// ([`super::MESSAGE_MISSING_RESOURCE_SELECTION`]).
pub fn missing_protected_resource_selection() -> MissingParameterValueException {
    MissingParameterValueException {
        message: Some(super::MESSAGE_MISSING_RESOURCE_SELECTION.to_string()),
        ..Default::default()
    }
}

/// The modelled refusal for a selection naming more specific protected resource
/// ARNs than one may: "you can include up to 30 specific protected resource ARNs
/// in ProtectedResourceArns" (model doc for the operation), which is "a maximum
/// number of items allowed in a request" — the condition the modelled
/// `LimitExceededException` is documented for.
pub fn too_many_protected_resource_arns() -> LimitExceededException {
    LimitExceededException {
        message: Some(format!(
            "ProtectedResourceArns cannot contain more than \
             {MAX_PROTECTED_RESOURCE_ARNS} protected resource ARNs"
        )),
        ..Default::default()
    }
}

/// Whether `value` is a name a restore testing selection can be addressed by:
/// "The name consists of only alphanumeric characters and underscores. Maximum
/// length is 50" (model doc for
/// `RestoreTestingSelectionForCreate$RestoreTestingSelectionName`).
///
/// The member targets a plain `String`, so the framework refuses nothing for it
/// before a handler runs: the rule is applied here instead, so no name the
/// service could never report is ever stored.
pub fn is_modelled_restore_testing_selection_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_RESTORE_TESTING_SELECTION_NAME_LENGTH
        && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether the `ProtectedResourceArns` a selection states names its resources by
/// WILDCARD — "a restore testing selection can include a wildcard value (\"*\")
/// for ProtectedResourceArns along with ProtectedResourceConditions" (model doc
/// for the operation), which is what makes conditions admissible beside it.
fn selects_by_wildcard(arns: &[String]) -> bool {
    arns.iter()
        .any(|arn| arn == PROTECTED_RESOURCE_ARNS_WILDCARD)
}

/// Why a restore testing selection body is not one AWS stores, as
/// [`rejected_restore_testing_selection_body`] decides it.
///
/// The three refusals are kept apart because they are three different modelled
/// errors: one states a value AWS does not accept, one that a member the
/// selection cannot do without was not stated, and one that the body is past a
/// documented limit.
#[derive(Debug)]
pub enum RejectedRestoreTestingSelectionBody {
    /// A member of the body carries a value AWS does not accept, as this
    /// modelled refusal states.
    Value(InvalidParameterValueException),
    /// The body selects no protected resource at all — see
    /// [`missing_protected_resource_selection`].
    SelectsNothing(MissingParameterValueException),
    /// The body names more specific protected resource ARNs than one selection
    /// may — see [`too_many_protected_resource_arns`].
    TooManyProtectedResourceArns(LimitExceededException),
}

/// Why the body a selection create carries is not one a restore testing
/// selection can be stored from, or `None` when it is one.
///
/// The ONE reading of a selection body, so every operation that stores one
/// applies the same documented rules and names the same member in its refusal:
///
/// * the name is alphanumeric and underscores up to 50 characters, and the
///   protected resource type and the role the restore runs as are stated — the
///   role as an ARN, which the member is typed as with no pattern, so the
///   framework refuses no other value for it;
/// * the resources are selected ONE way: "a restore testing selection can
///   include a wildcard value (\"*\") for ProtectedResourceArns along with
///   ProtectedResourceConditions. Alternatively, you can include up to 30
///   specific protected resource ARNs in ProtectedResourceArns. Cannot select by
///   both protected resource types AND specific ARNs. Request will fail if both
///   are included" (model doc for the operation), so the wildcard mixed into a
///   list of ARNs, and specific ARNs stated beside conditions, are both refused,
///   and more than [`MAX_PROTECTED_RESOURCE_ARNS`] specific ARNs is past the
///   documented limit;
/// * a selection selects its resources by one of those two means, so a body
///   that selects by neither selects nothing;
/// * a stated validation window is within [`MIN_VALIDATION_WINDOW_HOURS`] to
///   [`MAX_VALIDATION_WINDOW_HOURS`].
///
/// A collection the request states as EMPTY is a stated one — it is stored and
/// reported as the empty collection it is — but it selects no resource, which is
/// why a body whose only selection is empty is the missing-selection condition
/// rather than an invalid value.
pub fn rejected_restore_testing_selection_body(
    name: &str,
    protected_resource_type: &str,
    iam_role_arn: &str,
    protected_resource_arns: Option<&Vec<String>>,
    protected_resource_conditions: Option<&ProtectedResourceConditions>,
    validation_window_hours: i32,
) -> Option<RejectedRestoreTestingSelectionBody> {
    use RejectedRestoreTestingSelectionBody::{
        SelectsNothing, TooManyProtectedResourceArns, Value,
    };

    if !is_modelled_restore_testing_selection_name(name) {
        return Some(Value(super::rejected_parameter(
            "RestoreTestingSelectionName",
        )));
    }
    if protected_resource_type.is_empty() {
        return Some(Value(super::rejected_parameter("ProtectedResourceType")));
    }
    if let Some(refusal) = super::rejected_arn("IamRoleArn", iam_role_arn) {
        return Some(Value(refusal));
    }
    let arns: &[String] = protected_resource_arns.map_or(&[], Vec::as_slice);
    let wildcard = selects_by_wildcard(arns);
    let specific_arns = arns
        .iter()
        .filter(|arn| arn.as_str() != PROTECTED_RESOURCE_ARNS_WILDCARD)
        .count();
    let conditions = states_a_condition(protected_resource_conditions);
    // The wildcard and specific ARNs are two ways of naming the same member's
    // resources, and specific ARNs beside conditions are two ways of selecting
    // the selection's: "but not both" in either case.
    if wildcard && specific_arns > 0 {
        return Some(Value(super::rejected_parameter("ProtectedResourceArns")));
    }
    if specific_arns > 0 && conditions {
        return Some(Value(super::rejected_parameter("ProtectedResourceArns")));
    }
    if specific_arns > MAX_PROTECTED_RESOURCE_ARNS {
        return Some(TooManyProtectedResourceArns(
            too_many_protected_resource_arns(),
        ));
    }
    if !wildcard && specific_arns == 0 && !conditions {
        return Some(SelectsNothing(missing_protected_resource_selection()));
    }
    if !(MIN_VALIDATION_WINDOW_HOURS..=MAX_VALIDATION_WINDOW_HOURS)
        .contains(&validation_window_hours)
    {
        return Some(Value(super::rejected_parameter("ValidationWindowHours")));
    }
    None
}

/// Whether the `ProtectedResourceConditions` a selection states filters
/// protected resources at all: a conditions structure whose two lists are empty
/// or unstated matches on nothing, so it selects no resource and is not the
/// "along with ProtectedResourceConditions" the wildcard rule speaks of.
pub fn states_a_condition(conditions: Option<&ProtectedResourceConditions>) -> bool {
    conditions.is_some_and(|conditions| {
        conditions
            .string_equals
            .as_ref()
            .is_some_and(|pairs| !pairs.is_empty())
            || conditions
                .string_not_equals
                .as_ref()
                .is_some_and(|pairs| !pairs.is_empty())
    })
}

impl ComAmazonawsBackupCryocontrollerusermanagerState {
    /// Resolve the restore testing selection a (`RestoreTestingPlanName`,
    /// `RestoreTestingSelectionName`) pair addresses, or the modelled refusal
    /// for one this account does not hold.
    ///
    /// The ONE resolution step every call that names a selection shares, so none
    /// of them can answer a different class or a different sentence for the same
    /// condition. The plan is resolved FIRST, so a name no plan of this account
    /// carries answers the plan's not-found condition rather than the
    /// selection's.
    pub fn stored_restore_testing_selection(
        &self,
        plan_name: &str,
        selection_name: &str,
    ) -> Result<&RestoreTestingSelectionRecord, ResourceNotFoundException> {
        self.stored_restore_testing_plan(plan_name)?;
        self.restore_testing_selections
            .get(&(plan_name.to_string(), selection_name.to_string()))
            .ok_or_else(restore_testing_selection_not_found)
    }

    /// Every selection the named plan holds, in name order — the one traversal
    /// of this plan's share of the store, which is a contiguous range of it
    /// because the map is keyed plan-first.
    pub fn restore_testing_selections_of_plan<'a>(
        &'a self,
        plan_name: &str,
    ) -> impl Iterator<Item = &'a RestoreTestingSelectionRecord> + 'a {
        let plan = plan_name.to_string();
        let first = (plan.clone(), String::new());
        self.restore_testing_selections
            .range((std::ops::Bound::Included(first), std::ops::Bound::Unbounded))
            .take_while(move |((held, _), _)| *held == plan)
            .map(|(_, selection)| selection)
    }

    /// The selection a repeated `CreatorRequestId` replays: the token
    /// "identifies the request and allows failed requests to be retried without
    /// the risk of running the operation twice. If the request includes a
    /// CreatorRequestId that matches an existing [resource], that [resource] is
    /// returned" (model doc for
    /// `RestoreTestingSelectionForGet$CreatorRequestId`), so a retry carrying
    /// the token of a create that already stored a selection answers with that
    /// selection rather than storing a second one.
    ///
    /// Scoped to the plan the create addresses: the create is bound to one
    /// `RestoreTestingPlanName` (the model binds it to that path), so the call a
    /// token identifies is a call on that plan.
    pub fn restore_testing_selection_by_creator_request_id(
        &self,
        plan_name: &str,
        creator_request_id: &str,
    ) -> Option<&RestoreTestingSelectionRecord> {
        self.restore_testing_selections_of_plan(plan_name)
            .find(|selection| selection.creator_request_id.as_deref() == Some(creator_request_id))
    }

    /// How many selections the named plan already holds, which the per-plan
    /// ceiling is measured against.
    pub fn restore_testing_selection_count(&self, plan_name: &str) -> usize {
        self.restore_testing_selections_of_plan(plan_name).count()
    }
}

/// The key a selection is stored under: the plan it belongs to, then its own
/// name, so one plan's selections are contiguous in the map and a read of them
/// is a range of this one store rather than a second index beside it.
pub fn restore_testing_selection_key(plan_name: &str, selection_name: &str) -> (String, String) {
    (plan_name.to_string(), selection_name.to_string())
}
