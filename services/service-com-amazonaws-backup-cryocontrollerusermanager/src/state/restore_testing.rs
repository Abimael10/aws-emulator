// SPDX-License-Identifier: CC0-1.0

//! The RESTORE TESTING side of this service's state: the plan
//! `CreateRestoreTestingPlan` stores, and what every call that names a
//! `RestoreTestingPlanName` resolves against.
//!
//! # The rules this module holds
//!
//! * a restore testing plan is addressed by NAME — the name "cannot be changed
//!   after creation" (model doc for `RestoreTestingPlanName`) — and that name is
//!   unique to the account: the store is keyed by it, so the first create of a
//!   name stores the one plan that name has and a create naming one the account
//!   already holds is refused as already existing
//!   ([`super::restore_testing_plan_already_exists`]);
//! * a name the account holds no plan under is the not-found condition
//!   ([`super::restore_testing_plan_not_found`]), which every read of a plan
//!   answers;
//! * the plan body a create carries is stored as submitted, except for the
//!   members the model gives a default for, which the service fills where the
//!   request states none ([`DEFAULT_START_WINDOW_HOURS`],
//!   [`DEFAULT_SELECTION_WINDOW_DAYS`], an empty `ExcludeVaults` and the zone a
//!   schedule is read in,
//!   [`super::schedule::DEFAULT_SCHEDULE_EXPRESSION_TIMEZONE`]), and is
//!   reported back through the ONE read projection below, so the plan a create
//!   stored and the plan a read reports can never differ;
//! * a plan carries the instant it was last written — the create that stored it
//!   until an update rewrites it — so a read reports when its configuration last
//!   changed, and an update rewrites only the members its body states
//!   ([`RestoreTestingPlanRecord::apply_update`]);
//! * the updates of ONE plan are serialized: a call takes the plan for the span
//!   it spends applying its update ([`RESTORE_TESTING_PLAN_UPDATE_SPAN`]) and an
//!   update that arrives while the plan is held is refused as a concurrent one
//!   ([`restore_testing_plan_concurrent_update`]), so whichever of the updates
//!   in flight together took the plan is the one whose body a later read
//!   reports. The hold lasts only as long as the REQUEST that took it, which is
//!   why it is a claim rather than the due-instant device a framework's
//!   deployment carries ([`super::FrameworkRecord::deployment_in_flight`]): a
//!   framework really is unavailable for a span after its action returns, and a
//!   call that arrives in that span is refused, whereas a restore testing plan
//!   is written whole by the call that took it and is available again the
//!   instant that call answers — so an update sent after one returned must be
//!   served, and only one sent while another is still being applied is refused;
//! * a plan stops being held when it is taken out of the store
//!   ([`ComAmazonawsBackupCryocontrollerusermanagerState::remove_restore_testing_plan`]),
//!   which a plan that still holds restore testing selections bars
//!   ([`restore_testing_plan_selections_exist`]).

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use super::ComAmazonawsBackupCryocontrollerusermanagerState;
use crate::ops::types::{
    ConflictException, InvalidParameterValueException, InvalidRequestException,
    RestoreTestingPlanForGet, RestoreTestingPlanForList, RestoreTestingPlanForUpdate,
    RestoreTestingRecoveryPointSelection,
};

/// The most restore testing plans this emulator keeps.
///
/// An EMULATOR-CHOSEN ceiling, not an AWS quota, for the reason given on
/// [`super::MAX_VAULTS`]: the store is in memory and every request can add to
/// it. AWS caps the restore testing plans an account may hold too, and answers
/// the same modelled `LimitExceededException` for a create past it.
pub const MAX_RESTORE_TESTING_PLANS: usize = 500;

/// The start window a plan runs with when its create states none: "Defaults to
/// 24 hours" (model doc for `RestoreTestingPlanForCreate$StartWindowHours`).
pub const DEFAULT_START_WINDOW_HOURS: i32 = 24;

/// The least hours a stated start window may be, and the most: "If this value
/// is included, this parameter has a maximum value of 168 hours (one week)"
/// (model doc for `RestoreTestingPlanForCreate$StartWindowHours`), which a
/// window of hours is counted from one.
pub const MIN_START_WINDOW_HOURS: i32 = 1;
pub const MAX_START_WINDOW_HOURS: i32 = 168;

/// The selection window a plan picks recovery points within when its create
/// states none: "If not included, the value defaults to 30" (model doc for
/// `RestoreTestingRecoveryPointSelection$SelectionWindowDays`).
pub const DEFAULT_SELECTION_WINDOW_DAYS: i32 = 30;

/// The least and the most days a stated selection window may be: "Accepted
/// values are integers from 1 to 365" (model doc for the member).
pub const MIN_SELECTION_WINDOW_DAYS: i32 = 1;
pub const MAX_SELECTION_WINDOW_DAYS: i32 = 365;

/// The most characters a `RestoreTestingPlanName` may carry: "Maximum length is
/// 50" (model doc for `CreateRestoreTestingPlanOutput$RestoreTestingPlanName`).
pub const MAX_RESTORE_TESTING_PLAN_NAME_LENGTH: usize = 50;

/// How long this service spends APPLYING one update of a restore testing plan:
/// the span between the call taking the plan and its write of the merged body
/// landing. An emulator-chosen span, like the one a sampling-rule create spends
/// in the X-Ray service, not a published AWS figure: what it stands for is that
/// writing a plan is not instantaneous, so every update a caller sent without
/// waiting for an answer is still in flight when the one that took the plan
/// first writes it — which is what makes the serialization observable at all.
pub const RESTORE_TESTING_PLAN_UPDATE_SPAN: Duration = Duration::from_millis(100);

/// The FIXED text AWS states for "another update of this restore testing plan is
/// still being applied", around its one variable part: the name the call
/// addressed the plan by. Composed by
/// [`restore_testing_plan_concurrent_update_message`], the one place the sentence
/// is built.
pub const MESSAGE_RESTORE_TESTING_PLAN_CONCURRENT_UPDATE_PREFIX: &str =
    "Failed to update restore testing plan [";
/// The text that closes that sentence, after the plan name.
pub const MESSAGE_RESTORE_TESTING_PLAN_CONCURRENT_UPDATE_SUFFIX: &str =
    "] due to an attempt to update it concurrently.";

/// The sentence AWS states for "another update of this restore testing plan is
/// still being applied": the fixed text above around the name the REQUEST
/// carried, so the refusal says which plan was contended for.
pub fn restore_testing_plan_concurrent_update_message(name: &str) -> String {
    format!(
        "{MESSAGE_RESTORE_TESTING_PLAN_CONCURRENT_UPDATE_PREFIX}{name}\
         {MESSAGE_RESTORE_TESTING_PLAN_CONCURRENT_UPDATE_SUFFIX}"
    )
}

/// The modelled refusal for an update of a restore testing plan another update
/// is still applying — see [`restore_testing_plan_concurrent_update_message`].
/// AWS serializes the updates of one plan, so the update that loses the race is
/// refused rather than blended into the one already running; no `Code`, as the
/// framework's own concurrent-update refusal states none
/// ([`super::framework_action_in_progress`]).
pub fn restore_testing_plan_concurrent_update(name: &str) -> ConflictException {
    ConflictException {
        message: Some(restore_testing_plan_concurrent_update_message(name)),
        ..Default::default()
    }
}

/// `Code` for "the restore testing plan this delete addresses still holds
/// restore testing selections, which have to be deleted first" — the restore
/// testing plan's own token for the condition the BACKUP plan's delete states
/// with its own ([`super::ERROR_CODE_PLAN_SELECTIONS_EXIST`]), so a client that
/// branches on it tells the two resources' deletes apart.
pub const ERROR_CODE_RESTORE_TESTING_PLAN_SELECTIONS_EXIST: &str = "ERROR_RT_9201";

/// The sentence this service states for a restore testing plan delete the plan's
/// own selections still bar — FIXED text: it names neither the plan nor how many
/// selections bar the delete, only what has to be deleted first. "Deletion can
/// only successfully occur if all associated restore testing selections are
/// deleted first" (model doc for `DeleteRestoreTestingPlan`), worded as this
/// service words the same condition for a backup plan
/// ([`super::MESSAGE_PLAN_SELECTIONS_EXIST`]). Its token is
/// [`ERROR_CODE_RESTORE_TESTING_PLAN_SELECTIONS_EXIST`].
pub const MESSAGE_RESTORE_TESTING_PLAN_SELECTIONS_EXIST: &str =
    "Related restore testing selections must be deleted prior to restore testing plan deletion";

/// The modelled refusal for a restore testing plan delete the plan's own restore
/// testing selections still bar — see
/// [`MESSAGE_RESTORE_TESTING_PLAN_SELECTIONS_EXIST`], with the condition's own
/// token. ONE builder for the class, so the plan's STATE is stated the same way
/// wherever a delete is refused for it.
pub fn restore_testing_plan_selections_exist() -> InvalidRequestException {
    InvalidRequestException {
        code: Some(ERROR_CODE_RESTORE_TESTING_PLAN_SELECTIONS_EXIST.to_string()),
        message: Some(MESSAGE_RESTORE_TESTING_PLAN_SELECTIONS_EXIST.to_string()),
        ..Default::default()
    }
}

/// A restore testing plan: what `CreateRestoreTestingPlan` stores and
/// `GetRestoreTestingPlan` reports.
///
/// The body is kept as the create submitted it, so a read reports what was
/// stored rather than a value recomputed per call; the two instants only this
/// service can know — when the plan was last written, and when a restore test
/// last ran with it — are its own members.
#[derive(Debug, Clone)]
pub struct RestoreTestingPlanRecord {
    pub name: String,
    pub arn: String,
    /// Which recovery points a run of this plan restores, stored as submitted
    /// with the selection window filled in.
    pub recovery_point_selection: RestoreTestingRecoveryPointSelection,
    /// The cron expression a run of this plan is initiated by.
    pub schedule_expression: String,
    /// The zone that expression is read in, materialised where the request
    /// stated none ([`super::schedule::DEFAULT_SCHEDULE_EXPRESSION_TIMEZONE`]),
    /// so the zone the plan really runs in is what every read reports.
    pub schedule_expression_timezone: String,
    pub start_window_hours: i32,
    pub creator_request_id: Option<String>,
    pub creation_time: SystemTime,
    /// When this plan was last written: the instant the create stored it until
    /// an update rewrites it.
    pub last_update_time: SystemTime,
    /// When a restore test last ran with this plan; `None` until one has.
    pub last_execution_time: Option<SystemTime>,
    pub tags: BTreeMap<String, String>,
}

impl RestoreTestingPlanRecord {
    /// The plan as a READ reports it — the ONE projection of a stored plan, so a
    /// read can report nothing the create did not store.
    pub fn for_get(&self) -> RestoreTestingPlanForGet {
        RestoreTestingPlanForGet {
            restore_testing_plan_name: self.name.clone(),
            restore_testing_plan_arn: self.arn.clone(),
            recovery_point_selection: self.recovery_point_selection.clone(),
            schedule_expression: self.schedule_expression.clone(),
            schedule_expression_timezone: Some(self.schedule_expression_timezone.clone()),
            start_window_hours: self.start_window_hours,
            creator_request_id: self.creator_request_id.clone(),
            creation_time: super::modelled_timestamp(self.creation_time),
            last_update_time: Some(super::modelled_timestamp(self.last_update_time)),
            last_execution_time: self
                .last_execution_time
                .map(super::modelled_timestamp),
        }
    }

    /// The plan as a LISTING reports it — the metadata of the SAME stored
    /// record the read projects, minus the recovery-point selection the
    /// `RestoreTestingPlanForList` shape has no member for, so a listing can
    /// report nothing a read of the plan would not.
    pub fn for_list(&self) -> RestoreTestingPlanForList {
        RestoreTestingPlanForList {
            restore_testing_plan_name: self.name.clone(),
            restore_testing_plan_arn: self.arn.clone(),
            schedule_expression: self.schedule_expression.clone(),
            schedule_expression_timezone: Some(self.schedule_expression_timezone.clone()),
            start_window_hours: self.start_window_hours,
            creation_time: super::modelled_timestamp(self.creation_time),
            last_update_time: Some(super::modelled_timestamp(self.last_update_time)),
            last_execution_time: self.last_execution_time.map(super::modelled_timestamp),
        }
    }

    /// Rewrite the members an update may change, and record the instant it
    /// changed them.
    ///
    /// `RestoreTestingPlanName` "cannot be updated after it is created" (the
    /// update's model doc) and the ARN and the creation instant are the
    /// identifiers minted from it, so the body an update carries — the recovery
    /// point selection, the schedule, its zone and the start window — is all
    /// this rewrites. A member the update states NOTHING for keeps the value the
    /// plan already held; a submitted selection is completed exactly as a create
    /// completes one
    /// ([`stored_restore_testing_recovery_point_selection`]), so a plan an
    /// update rewrote and a plan a create stored read back the same way.
    pub fn apply_update(&mut self, body: RestoreTestingPlanForUpdate, now: SystemTime) {
        if let Some(selection) = body.recovery_point_selection {
            self.recovery_point_selection =
                stored_restore_testing_recovery_point_selection(selection);
        }
        if let Some(schedule_expression) = body.schedule_expression {
            self.schedule_expression = schedule_expression;
        }
        if let Some(timezone) = body.schedule_expression_timezone {
            self.schedule_expression_timezone = timezone;
        }
        // A start window the update states nothing for reaches this service as
        // the modelled default of its integer member (`0`), which is outside the
        // member's documented range: it states no new window, so the stored one
        // stands.
        if body.start_window_hours != 0 {
            self.start_window_hours = body.start_window_hours;
        }
        self.last_update_time = now;
    }
}

/// Whether `value` is a name a restore testing plan can be addressed by: "it
/// must consist of only alphanumeric characters and underscores" and "maximum
/// length is 50" (model doc for `RestoreTestingPlanForCreate$RestoreTestingPlanName`).
///
/// The member targets a plain `String`, so the framework refuses nothing for it
/// before a handler runs: the rule is applied here instead, so no name the
/// service could never report is ever stored.
pub fn is_modelled_restore_testing_plan_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_RESTORE_TESTING_PLAN_NAME_LENGTH
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Why the body a create or an update carries is not one a restore testing plan
/// can be stored from, or `None` when it is one.
///
/// The ONE reading of a plan body, so every operation that stores one applies
/// the same documented rules and names the same member in its refusal: the name
/// is alphanumeric and underscores up to 50 characters, the schedule is a cron
/// expression this service can compute a run from
/// ([`super::schedule::parse_schedule_expression`]), a stated start window is
/// within [`MIN_START_WINDOW_HOURS`] to [`MAX_START_WINDOW_HOURS`] and a stated
/// selection window is within [`MIN_SELECTION_WINDOW_DAYS`] to
/// [`MAX_SELECTION_WINDOW_DAYS`].
///
/// A window the request states NOTHING for is not a stated one: it reaches this
/// service as the modelled default of its integer member (`0`), which is the
/// value [`stored_start_window_hours`] and
/// [`stored_restore_testing_recovery_point_selection`] replace with the
/// documented default.
pub fn rejected_restore_testing_plan_body(
    name: Option<&str>,
    schedule_expression: Option<&str>,
    start_window_hours: i32,
    selection_window_days: i32,
) -> Option<InvalidParameterValueException> {
    if name.is_some_and(|name| !is_modelled_restore_testing_plan_name(name)) {
        return Some(super::rejected_parameter("RestoreTestingPlanName"));
    }
    if schedule_expression
        .is_some_and(|expression| super::schedule::parse_schedule_expression(expression).is_none())
    {
        return Some(super::rejected_parameter("ScheduleExpression"));
    }
    if start_window_hours != 0
        && !(MIN_START_WINDOW_HOURS..=MAX_START_WINDOW_HOURS).contains(&start_window_hours)
    {
        return Some(super::rejected_parameter("StartWindowHours"));
    }
    if selection_window_days != 0
        && !(MIN_SELECTION_WINDOW_DAYS..=MAX_SELECTION_WINDOW_DAYS)
            .contains(&selection_window_days)
    {
        return Some(super::rejected_parameter("SelectionWindowDays"));
    }
    None
}

/// The two members of a stored plan the model gives a DEFAULT for, filled where
/// the request stated neither: the selection the plan picks recovery points
/// with, as submitted otherwise.
///
/// * `SelectionWindowDays` reaches this service as the modelled default of its
///   integer member (`0`) when the request states nothing for it, and `0` is
///   outside the member's documented range, so it is exactly the "not included"
///   case [`DEFAULT_SELECTION_WINDOW_DAYS`] is documented for;
/// * `ExcludeVaults` "defaults to empty list if not listed" (model doc for the
///   member), so a plan stored from a request that listed none excludes no
///   vault, and a read of it reports that empty list rather than nothing.
pub fn stored_restore_testing_recovery_point_selection(
    selection: RestoreTestingRecoveryPointSelection,
) -> RestoreTestingRecoveryPointSelection {
    let selection_window_days = if selection.selection_window_days == 0 {
        DEFAULT_SELECTION_WINDOW_DAYS
    } else {
        selection.selection_window_days
    };
    RestoreTestingRecoveryPointSelection {
        selection_window_days,
        exclude_vaults: Some(selection.exclude_vaults.unwrap_or_default()),
        ..selection
    }
}

/// The start window a stored plan carries: the stated value, or
/// [`DEFAULT_START_WINDOW_HOURS`] where the request stated none — which reaches
/// this service as the modelled default of the member (`0`), for the reason
/// given above.
pub fn stored_start_window_hours(start_window_hours: i32) -> i32 {
    if start_window_hours == 0 {
        DEFAULT_START_WINDOW_HOURS
    } else {
        start_window_hours
    }
}

impl ComAmazonawsBackupCryocontrollerusermanagerState {
    /// Resolve the restore testing plan a `RestoreTestingPlanName` addresses, or
    /// the modelled refusal for one this account does not hold.
    ///
    /// The ONE resolution step every call that names a plan shares, so none of
    /// them can answer a different class or a different sentence for the same
    /// condition.
    pub fn stored_restore_testing_plan(
        &self,
        name: &str,
    ) -> Result<&RestoreTestingPlanRecord, crate::ops::types::ResourceNotFoundException> {
        self.restore_testing_plans
            .get(name)
            .ok_or_else(super::restore_testing_plan_not_found)
    }

    /// Resolve the restore testing plan a `RestoreTestingPlanName` addresses FOR
    /// WRITING, or the modelled refusal for one this account does not hold — the
    /// same resolution, and so the same refusal, every read of a plan answers.
    pub fn stored_restore_testing_plan_mut(
        &mut self,
        name: &str,
    ) -> Result<&mut RestoreTestingPlanRecord, crate::ops::types::ResourceNotFoundException> {
        self.restore_testing_plans
            .get_mut(name)
            .ok_or_else(super::restore_testing_plan_not_found)
    }

    /// Take the named plan for an update, answering whether this call took it.
    ///
    /// `false` means another update of that plan is still being applied, which is
    /// the concurrent-update condition
    /// ([`restore_testing_plan_concurrent_update`]): the updates of one plan are
    /// serialized, so exactly one of however many arrive together holds the plan
    /// and writes it. A call that took the plan releases it with
    /// [`Self::release_restore_testing_plan_update`] however it ends.
    pub fn claim_restore_testing_plan_update(&mut self, name: &str) -> bool {
        self.restore_testing_plan_updates_in_flight
            .insert(name.to_string())
    }

    /// Release the named plan once the update that took it has answered, so the
    /// next update of it is served rather than refused.
    pub fn release_restore_testing_plan_update(&mut self, name: &str) {
        self.restore_testing_plan_updates_in_flight.remove(name);
    }

    /// Take the restore testing plan a `RestoreTestingPlanName` addresses OUT of
    /// the store, or answer the modelled refusal for a plan whose own restore
    /// testing selections still bar its deletion.
    ///
    /// The ONE removal step, so the resource a delete reports gone is gone from
    /// the store every later read and mutation of that name resolves against:
    /// after it, the name answers the plan's not-found condition
    /// ([`super::restore_testing_plan_not_found`]) and is free for a create
    /// again — which mints another plan, with its own ARN.
    ///
    /// "Deletion can only successfully occur if all associated restore testing
    /// selections are deleted first" (model doc for `DeleteRestoreTestingPlan`),
    /// so a plan that still holds one is left exactly as it was and the call is
    /// refused ([`restore_testing_plan_selections_exist`]).
    ///
    /// Otherwise the removal is IDEMPOTENT: `None` says the account held no plan
    /// under that name, which is the state the caller asked for, so a repeat of
    /// the call is not a different answer — and the operation carries no modelled
    /// not-found error to state one with.
    pub fn remove_restore_testing_plan(
        &mut self,
        name: &str,
    ) -> Result<Option<RestoreTestingPlanRecord>, InvalidRequestException> {
        if self.restore_testing_selections_of_plan(name).next().is_some() {
            return Err(restore_testing_plan_selections_exist());
        }
        Ok(self.restore_testing_plans.remove(name))
    }

    /// The plan a repeated `CreatorRequestId` replays: "This identifies the
    /// request and allows failed requests to be retried without the risk of
    /// running the operation twice. If the request includes a
    /// `CreatorRequestId` that matches an existing backup plan, that plan is
    /// returned" (model doc for the member), so a retry carrying the token of a
    /// create that already stored a plan answers with that plan rather than
    /// storing a second one.
    pub fn restore_testing_plan_by_creator_request_id(
        &self,
        creator_request_id: &str,
    ) -> Option<&RestoreTestingPlanRecord> {
        self.restore_testing_plans
            .values()
            .find(|plan| plan.creator_request_id.as_deref() == Some(creator_request_id))
    }
}
