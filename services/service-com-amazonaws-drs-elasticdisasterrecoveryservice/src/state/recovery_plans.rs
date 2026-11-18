// SPDX-License-Identifier: CC0-1.0

//! The Recovery Plan resource: what the store holds for one plan, how a plan is
//! reported on the wire, and how a plan nothing holds is refused.
//!
//! A Recovery Plan is addressed by its ARN rather than by its id
//! (`GetRecoveryPlanRequest$recoveryPlanArn`), so the two directions of that
//! addressing live here together: [`arn`] builds a plan's ARN from the id the store
//! keys it by, and [`id_from_arn`] reads that id back out of an ARN a request named.
//!
//! The ONE place a stored plan becomes a wire plan ([`RecoveryPlanRecord::to_wire`]),
//! so a created plan and a plan read back cannot disagree about any member — and the
//! one place a plan's `status` is decided, which is a FUNCTION of the steps the plan
//! holds rather than a member anything stores.

use std::collections::HashMap;

use crate::ops::types::{
    RecoveryPlan, RecoveryPlanStep, RecoveryPlanStepConfiguration, ResourceNotFoundException,
};

/// The prefix of every Recovery Plan id: the model's `RecoveryPlanID` is
/// `^plan-[0-9a-zA-Z]{17,59}$` (length 22..64), which the 17-character suffix
/// [`crate::state::ComAmazonawsDrsElasticdisasterrecoveryserviceState::next_id`]
/// mints satisfies at its shortest form.
pub const ID_PREFIX: &str = "plan";

/// The resource part of a Recovery Plan's ARN, as the resource's own ARN template
/// spells it (`RecoveryPlanResource`: `recovery-plan/{recoveryPlanID}`).
///
/// Public because the tagging route reads the KIND of resource an ARN addresses off the
/// same prefixes (`crate::state::tags`), and the two must not spell it differently.
pub const ARN_RESOURCE_PREFIX: &str = "recovery-plan/";

/// How this service names the Recovery Plan resource when it reports one in an
/// error: the `resourceType` of a not-found refusal, and the subject of its message.
pub const RESOURCE_TYPE: &str = "Recovery Plan";

/// The status of a plan that can be executed, from the model's `RecoveryPlanStatus`
/// (see `crate::ops::types::RECOVERY_PLAN_STATUS_VALUES`), documented as "executable".
pub const STATUS_ACTIVE: &str = "ACTIVE";

/// The status of a plan that holds no `SERVER` step: the same enum documents
/// `INVALID` as a plan that "has no `SERVER` type steps and cannot be executed".
pub const STATUS_INVALID: &str = "INVALID";

/// How many Recovery Plans the store holds before `CreateRecoveryPlan` refuses with
/// the modelled `ServiceQuotaExceededException`.
///
/// This is the emulator's OWN bound on the store, not a claim about any account's
/// AWS quota (Service Quotas is not modelled here). It is set far above any workflow
/// an emulator serves, and it is what stops a caller growing the state without end.
pub const QUOTA: usize = 1_000;

/// A stored Recovery Plan: the members a create supplies, the ones the service
/// assigns (`id`, `arn`, `createdAt`, `updatedAt`), the `clientToken` the create
/// carried, and the steps the plan orchestrates.
#[derive(Debug, Clone, Default)]
pub struct RecoveryPlanRecord {
    /// The `RecoveryPlanID` the store keys this plan by, and the resource part of
    /// [`Self::arn`].
    pub id: String,
    pub arn: String,
    pub name: String,
    pub description: Option<String>,
    /// A plan always carries a tag set, so a create that named none stores an empty
    /// one: the service reports this member whether or not a client sent it.
    pub tags: HashMap<String, String>,
    /// `createdAt`, as the model's `ISO8601DatetimeString`; never changed after the
    /// create that minted the plan.
    pub created_at: String,
    /// `updatedAt`: the instant of the last call that changed this plan, which is
    /// the create until one does.
    pub updated_at: String,
    /// The non-empty `clientToken` of the create that minted this plan, which is
    /// what makes a repeat of that create report THIS plan instead of minting a
    /// second one. `None` for a create that named no token.
    pub client_token: Option<String>,
    /// The steps this plan orchestrates, in step order. A plan is created holding
    /// none — steps are added by the step operations — and which kinds of step a
    /// plan holds is what decides its [`Self::status`].
    pub steps: Vec<RecoveryPlanStep>,
}

impl RecoveryPlanRecord {
    /// The plan's `status`: [`STATUS_ACTIVE`] once it holds a `SERVER` step,
    /// [`STATUS_INVALID`] while it holds none.
    ///
    /// Derived from the steps rather than stored, so a plan can never report a
    /// status its steps contradict: the model documents `INVALID` as exactly "the
    /// plan has no `SERVER` type steps and cannot be executed", which is the state
    /// every freshly created plan is in.
    pub fn status(&self) -> &'static str {
        let executable = self.steps.iter().any(|step| {
            matches!(
                step.configuration,
                RecoveryPlanStepConfiguration::serverStepConfiguration(_)
            )
        });
        if executable {
            STATUS_ACTIVE
        } else {
            STATUS_INVALID
        }
    }

    /// The stored plan as the modelled `RecoveryPlan` structure — the output member
    /// of every operation that reports one plan.
    pub fn to_wire(&self) -> RecoveryPlan {
        RecoveryPlan {
            recovery_plan_arn: self.arn.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            status: self.status().to_string(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            tags: Some(self.tags.clone()),
        }
    }
}

/// The ARN of a Recovery Plan:
/// `arn:aws:drs:<region>:<account>:recovery-plan/<recoveryPlanID>`, the resource's
/// own ARN template.
pub fn arn(region: &str, id: &str) -> String {
    ::engine::emulator_arn("drs", region, &format!("{ARN_RESOURCE_PREFIX}{id}"))
}

/// The Recovery Plan id an ARN addresses, or `None` for an ARN that addresses no
/// plan of this service.
///
/// An ARN is `arn:<partition>:<service>:<region>:<account>:<resource>`, and a plan's
/// resource part is [`ARN_RESOURCE_PREFIX`] followed by the id the store keys it by,
/// so this is the read that turns an ARN a request named into a store lookup.
pub fn id_from_arn(arn: &str) -> Option<&str> {
    let resource = arn.splitn(6, ':').nth(5)?;
    let id = resource.strip_prefix(ARN_RESOURCE_PREFIX)?;
    (!id.is_empty()).then_some(id)
}

/// The modelled `ResourceNotFoundException` (HTTP 404) for a Recovery Plan this
/// store does not hold.
///
/// The ONE place that refusal is built, so every plan operation reports the same
/// `resourceType`, the same `resourceId` (the ARN the request named, verbatim) and
/// the same message: the service's name for the resource type, then what it was
/// asked for, then that it was not found.
pub fn not_found(plan_arn: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(format!(
            "{RESOURCE_TYPE} with Arn: '{plan_arn}' was not found"
        )),
        code: None,
        resource_id: Some(plan_arn.to_string()),
        resource_type: Some(RESOURCE_TYPE.to_string()),
    }
}

/// `at` as the model's `ISO8601DatetimeString` — the form `createdAt` and
/// `updatedAt` are constrained to (`^[1-9][0-9]*-..T..:..:..(\.[0-9]+)?Z$`), which
/// is the RFC-3339 date-time rendering in UTC.
///
/// The rendering fails only for an instant outside the four-digit years that form
/// covers, which the host clock does not produce; such an instant falls back to
/// [`EPOCH`] rather than to an empty string, so what is stored and reported always
/// has the member's modelled form.
pub fn iso8601(at: std::time::SystemTime) -> String {
    ::aws_smithy_types::DateTime::from(at)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|_| EPOCH.to_string())
}

/// The instant an unrenderable clock reading is reported as: the start of the Unix
/// epoch, in the form [`iso8601`] renders.
const EPOCH: &str = "1970-01-01T00:00:00Z";

/// The instant of the call now being served, in that same form: what a create
/// stamps both of a plan's timestamps with, and what a later change stamps
/// `updatedAt` with.
pub fn now() -> String {
    iso8601(std::time::SystemTime::now())
}
