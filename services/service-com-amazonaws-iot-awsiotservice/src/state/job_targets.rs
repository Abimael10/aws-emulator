// SPDX-License-Identifier: CC0-1.0

//! Which DEVICES the `targets` of work that runs on devices name, resolved against this
//! account's REGISTRY.
//!
//! Parsing a target ARN into a thing or a thing group is the registry-free half of the
//! job-target rule ([`super::resolve_job_targets`]); this module is the half that needs
//! the store: a `thing/` target names a device of this account, a `thinggroup/` target
//! names the devices that group holds, and a target naming neither is refused with the
//! class the model declares for it rather than written as work nothing can run.
//!
//! Every operation that points work at devices — `CreateJob`, `AssociateTargetsWithJob`,
//! `CreateOTAUpdate` — resolves its targets HERE, so one request is refused for one
//! reason in one wording whichever of them carried it.

use super::*;

/// How every operation that resolves `targets` refuses MORE targets than the devices
/// one job may be pointed at.
///
/// The fixed text is the jobs plane's sentence about the target LIST, whose single
/// variable part is the published ceiling itself ([`MAX_JOB_TARGETS`]) — nothing of the
/// refused request is in it, since what the caller has to change is the size of the list
/// and not any one entry.
pub fn target_list_over_limit_message() -> String {
    format!("Target list exceeds size limit {MAX_JOB_TARGETS}.")
}

impl ComAmazonawsIotAwsiotserviceState {
    /// The DEVICES `fanout`'s targets name, in the order the request named them, or the
    /// reason one of them names none.
    ///
    /// A `thing/` target names ONE device, and it has to be a device this account holds:
    /// a job's execution is work handed to a registered thing, so a target naming a
    /// `thingName` the registry does not hold is the modelled `ResourceNotFoundException`
    /// ([`super::not_found::missing_job_target_thing_message`]) and nothing is written.
    /// A `thinggroup/` target names the things the group holds — however many it holds,
    /// including NONE: a group with no members is a target, so group-targeted work is
    /// created whether or not a device is in the group yet (a continuous job reaches a
    /// device that joins later). The group itself still has to be one whose devices are
    /// known ([`Self::thing_group_target_refusal`]).
    ///
    /// A device named twice — directly and through a group, or by two groups — is ONE
    /// device, so it is handed one execution. The result is bounded by
    /// [`MAX_JOB_TARGETS`], the same ceiling the request's own target list is bounded by,
    /// so the fan-out a group target adds cannot exceed what one request may name. The
    /// ceiling is checked AS the devices accumulate, so it bounds the walk itself rather
    /// than only its result.
    pub fn job_target_devices(
        &self,
        fanout: &JobTargetFanout,
    ) -> Result<Vec<String>, JobTargetsRefusal> {
        let over_limit = || JobTargetsRefusal::OverLimit(target_list_over_limit_message());
        let mut devices: Vec<String> = Vec::with_capacity(fanout.things.len());
        for thing_name in &fanout.things {
            if !self.things.contains_key(thing_name) {
                return Err(JobTargetsRefusal::NotFound(
                    not_found::missing_job_target_thing_message(thing_name),
                ));
            }
            if devices.len() == MAX_JOB_TARGETS {
                return Err(over_limit());
            }
            devices.push(thing_name.clone());
        }
        for thing_group_name in &fanout.groups {
            if let Some(refusal) = self.thing_group_target_refusal(thing_group_name) {
                return Err(refusal);
            }
            for thing_name in self.things_in_thing_group(thing_group_name) {
                if devices.contains(&thing_name) {
                    continue;
                }
                if devices.len() == MAX_JOB_TARGETS {
                    return Err(over_limit());
                }
                devices.push(thing_name);
            }
        }
        Ok(devices)
    }
}
