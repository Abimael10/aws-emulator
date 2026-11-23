// SPDX-License-Identifier: CC0-1.0

//! The references OTHER records of this account hold on ONE stage, and what becomes of
//! them when that stage is removed.
//!
//! A `UsagePlan`'s `apiStages` is "the associated API stages of a usage plan"
//! (`UsagePlan$apiStages`): each entry names an API and a stage of it whose traffic the
//! plan's throttle and quota meter. Metering a stage that no longer exists is not
//! something a plan can do, so the association does not outlive the stage it names: when
//! the stage goes, every entry naming it goes with it, and no read of a plan reports a
//! stage that resolves to nothing.
//!
//! The reclamation lives here rather than in the delete handler because it is a property
//! of the ASSOCIATION — a plan never names a stage this account does not hold — so one
//! place owns it and every caller that removes a stage keeps the invariant.

use super::ComAmazonawsApigatewayBackplanecontrolserviceState;

impl ComAmazonawsApigatewayBackplanecontrolserviceState {
    /// Drop every usage-plan association that names the stage `stage_name` of the API
    /// `api_id`, answering how many plans stopped metering it.
    ///
    /// Called when a STAGE is deleted, for the reason
    /// [`ComAmazonawsApigatewayBackplanecontrolserviceState::forget_api_key`] is called
    /// when an API key is: the association names a record that no longer exists, so it is
    /// reclaimed with that record instead of sitting in the plans as a dangling reference.
    /// An entry carrying only one half of the pair names no API stage at all and is left
    /// exactly where it is, since it cannot be about this stage.
    ///
    /// The plan itself, its limits, its tags and the API keys it meters are untouched: what
    /// goes is the association, which is the only thing the stage's removal is about.
    pub fn forget_api_stage(&mut self, api_id: &str, stage_name: &str) -> usize {
        let mut unmetered = 0;
        for plan in self.usage_plans.values_mut() {
            let held = plan.api_stages.len();
            plan.api_stages.retain(|association| {
                !(association.api_id.as_deref() == Some(api_id)
                    && association.stage.as_deref() == Some(stage_name))
            });
            if plan.api_stages.len() != held {
                unmetered += 1;
            }
        }
        unmetered
    }
}
