// SPDX-License-Identifier: CC0-1.0

//! The TARGETS registered with a maintenance window: "the targets, either managed nodes or
//! tags" a window's tasks run against
//! (`smithy_query(query="shape",shape="MaintenanceWindowTarget")`).
//!
//! `RegisterTargetWithMaintenanceWindow` mints one and `DescribeMaintenanceWindowTargets`
//! reports the ones a window holds, both through the one store below
//! ([`super::ComAmazonawsSsmAmazonssmState::maintenance_window_targets`]), so whatever the
//! register persisted is what a later read reports and what the deregister removes.
//!
//! A target belongs to ONE maintenance window of one Region — a window is reached through
//! the endpoint of the Region it was created in — so the store is keyed by
//! `(region, window target id)`, with the window it belongs to on the record: the id is
//! unique across the Region, which is what lets `DeregisterTargetFromMaintenanceWindow`
//! address one by `(WindowId, WindowTargetId)` and what makes a target of another window
//! no target of this one.
//!
//! The id is the SERVICE's to choose: the register takes no identifier for the target and
//! answers the `WindowTargetId` it minted, which the model fixes as the 8-4-4-4-12
//! hexadecimal UUID (`MaintenanceWindowTargetId`'s pattern). A window id that names no
//! window of the Region names nothing to register a target with, which is what the modelled
//! `DoesNotExistException` reports
//! ([`super::maintenance_windows::maintenance_window_not_found_message`]).

use std::time::SystemTime;

use crate::ops::types::{MaintenanceWindowTarget, Target};

use super::ComAmazonawsSsmAmazonssmState;

/// The filter keys a describe of a window's targets may narrow by: "the supported filter
/// keys are `Type`, `WindowTargetId`, and `OwnerInformation`"
/// (`DescribeMaintenanceWindowTargetsRequest$Filters`). What a refusal of another key lists,
/// and the one place that set is written.
pub const TARGET_FILTER_KEYS: &[&str] = &["Type", "WindowTargetId", "OwnerInformation"];

/// The page size a describe of a window's targets serves when the request names none: the
/// modelled maximum of `MaintenanceWindowMaxResults` (`range 10..100`).
pub const MAX_TARGET_PAGE_SIZE: i32 = 100;

/// The smallest page that read serves, from the same modelled range.
pub const MIN_TARGET_PAGE_SIZE: i32 = 10;

/// How many targets this store holds for ONE maintenance window.
///
/// This is the EMULATOR's own ceiling rather than a quota Systems Manager publishes: a
/// register is the only thing that admits a target, callers may run as many as they like,
/// and the store is held in memory for as long as the process runs. A register past it is
/// refused through the modelled `ResourceLimitExceededException` and writes nothing.
pub const MAX_TARGETS_PER_WINDOW: usize = 50;

/// What a register refused by [`MAX_TARGETS_PER_WINDOW`] reports: the emulator's own
/// ceiling, named as the emulator's, with the figure it stands at.
#[must_use]
pub fn target_ceiling_message(window_id: &str) -> String {
    format!(
        "This emulator holds at most {MAX_TARGETS_PER_WINDOW} targets for the maintenance \
         window {window_id}"
    )
}

/// How this service reports a `WindowTargetId` that names no target of the window the
/// request addressed, through `DoesNotExistException` — whose own account of its class is
/// "the specified resource, such as a maintenance window or patch baseline, doesn't exist".
///
/// RECORDED BEHAVIOUR (corpus `simparity-behavioral-aws-v1`, unit
/// `ssm.maintenance-window-target-rejects-unknown-id-after-deregister`): the sentence names
/// the TARGET and nothing else — `Maintenance window target <WindowTargetId> does not
/// exist` — so the window the request addressed is not in it. The missing entity is the
/// target, which is what the refusal says; a window the Region holds none of is the separate
/// [`super::maintenance_windows::maintenance_window_not_found_message`], and the two
/// conditions stay distinguishable because each names the entity that is missing.
#[must_use]
pub fn target_not_found_message(window_target_id: &str) -> String {
    format!("Maintenance window target {window_target_id} does not exist")
}

/// The page size a target describe serves (see `super::page_size`).
pub fn target_page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(max_results, MIN_TARGET_PAGE_SIZE, MAX_TARGET_PAGE_SIZE)
}

/// One target registered with a maintenance window, as the store holds it.
///
/// Every member a read reports is here, so a read projects this record rather than
/// inventing anything.
#[derive(Debug, Clone)]
pub struct MaintenanceWindowTargetRecord {
    /// The server-minted id this record is stored under.
    pub window_target_id: String,
    /// The Region whose endpoint the target was registered through.
    pub region: String,
    /// "The ID of the maintenance window the target should be registered with" — required
    /// on register.
    pub window_id: String,
    /// "The type of target being registered with the maintenance window"
    /// (`MaintenanceWindowResourceType`) — required on register.
    pub resource_type: String,
    /// "The targets to register with the maintenance window", exactly as the register
    /// stated them.
    pub targets: Vec<Target>,
    /// "User-provided value that will be included in any Amazon CloudWatch Events events
    /// raised while running tasks for these targets in this maintenance window", or `None`
    /// for a register that carried none.
    pub owner_information: Option<String>,
    /// "An optional name for the target", or `None`.
    pub name: Option<String>,
    /// "An optional description for the target", or `None`.
    pub description: Option<String>,
    /// The "user-provided idempotency token" the register carried, or `None`: what makes a
    /// retry of that register report the target it already minted rather than a second one.
    pub client_token: Option<String>,
    /// When the register ran, so a listing of one window's targets walks them in the order
    /// they were registered where their ids do not order them.
    pub registered_at: SystemTime,
}

impl MaintenanceWindowTargetRecord {
    /// The modelled `MaintenanceWindowTarget` for this record: every member either echoed
    /// from the register as it was received or the id this service minted.
    #[must_use]
    pub fn to_maintenance_window_target(&self) -> MaintenanceWindowTarget {
        MaintenanceWindowTarget {
            window_id: Some(self.window_id.clone()),
            window_target_id: Some(self.window_target_id.clone()),
            resource_type: Some(self.resource_type.clone()),
            targets: Some(self.targets.clone()),
            owner_information: self.owner_information.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
        }
    }

    /// Whether this registration is one the criteria `criteria` name: "the managed node ID or
    /// key-value pair to retrieve information about"
    /// (`DescribeMaintenanceWindowScheduleRequest$Targets`), which is what makes a window one
    /// of the windows a node is associated with.
    ///
    /// A criterion names this registration when the two state the SAME key and share a value
    /// under it, since that is the key-value pair the criterion asks about; a criterion
    /// stating a key this registration does not, or that key under none of its values, names
    /// other nodes entirely. Criteria UNION, the way a filter's values do: a read asking about
    /// several targets asks for the windows ANY of them is registered with. A criterion
    /// stating no key names nothing.
    #[must_use]
    pub fn covers(&self, criteria: &[Target]) -> bool {
        criteria.iter().any(|criterion| {
            let key = criterion.key.as_deref().unwrap_or_default();
            let wanted = criterion.values.as_deref().unwrap_or_default();
            !key.is_empty()
                && self.targets.iter().any(|registered| {
                    registered.key.as_deref().unwrap_or_default() == key
                        && registered
                            .values
                            .as_deref()
                            .unwrap_or_default()
                            .iter()
                            .any(|value| wanted.contains(value))
                })
        })
    }

    /// Whether the filter `key`=`values` selects this target.
    ///
    /// Each key narrows by one property of the target, and a filter's values UNION: a
    /// target is selected when one of that filter's values is its value for the key. A key
    /// outside [`TARGET_FILTER_KEYS`] selects nothing — its caller refuses such a key
    /// before anything is asked about the targets.
    #[must_use]
    pub fn matches_filter(&self, key: &str, values: &[String]) -> bool {
        let held = match key {
            "Type" => Some(self.resource_type.clone()),
            "WindowTargetId" => Some(self.window_target_id.clone()),
            "OwnerInformation" => self.owner_information.clone(),
            _ => return false,
        };
        held.is_some_and(|held| values.contains(&held))
    }

    /// The members a register SUPPLIED, rendered as one canonical string.
    ///
    /// This is what "the parameters don't match the original call to the API with the same
    /// idempotency token" (`IdempotentParameterMismatch`) is decided on: two registers
    /// carrying one idempotency token are the same call when their supplied members render
    /// the same way. The minted id, the Region and the token itself are not supplied
    /// members, so they are not in it.
    #[must_use]
    pub fn registration_fingerprint(&self) -> String {
        let targets: Vec<String> = self
            .targets
            .iter()
            .map(|target| {
                format!(
                    "{}={}",
                    target.key.as_deref().unwrap_or_default(),
                    target.values.clone().unwrap_or_default().join(",")
                )
            })
            .collect();
        format!(
            "window={}\ntype={}\nowner={}\nname={}\ndescription={}\ntargets=[{}]\n",
            self.window_id,
            self.resource_type,
            self.owner_information.as_deref().unwrap_or_default(),
            self.name.as_deref().unwrap_or_default(),
            self.description.as_deref().unwrap_or_default(),
            targets.join("|"),
        )
    }
}

impl ComAmazonawsSsmAmazonssmState {
    /// The target `window_target_id` names among `window_id`'s targets in `region`, or
    /// `None` when that window holds none under that id — which is what a not-found refusal
    /// reports.
    ///
    /// The window is part of the question: a target id names one record of the Region, and a
    /// request addressing it through ANOTHER window names no target of that window.
    pub fn maintenance_window_target(
        &self,
        region: &str,
        window_id: &str,
        window_target_id: &str,
    ) -> Option<&MaintenanceWindowTargetRecord> {
        self.maintenance_window_targets
            .get(&(region.to_string(), window_target_id.to_string()))
            .filter(|held| held.window_id == window_id)
    }

    /// Every target `window_id` holds in `region`, in the order they were registered — the
    /// order a describe reports them in, with the minted id breaking a tie so the order is
    /// total and a page can resume at an id.
    pub fn maintenance_window_targets_of(
        &self,
        region: &str,
        window_id: &str,
    ) -> Vec<&MaintenanceWindowTargetRecord> {
        let mut held: Vec<&MaintenanceWindowTargetRecord> = self
            .maintenance_window_targets
            .iter()
            .filter(|((region_of, _), target)| region_of == region && target.window_id == window_id)
            .map(|(_, target)| target)
            .collect();
        held.sort_by(|left, right| {
            left.registered_at
                .cmp(&right.registered_at)
                .then_with(|| left.window_target_id.cmp(&right.window_target_id))
        });
        held
    }

    /// Store `record` as a target of its own Region, keyed by its minted id.
    pub fn insert_maintenance_window_target(&mut self, record: MaintenanceWindowTargetRecord) {
        let key = (record.region.clone(), record.window_target_id.clone());
        self.maintenance_window_targets.insert(key, record);
    }

    /// Remove the target `window_target_id` names among `window_id`'s, answering whether
    /// there was one to remove.
    pub fn remove_maintenance_window_target(
        &mut self,
        region: &str,
        window_id: &str,
        window_target_id: &str,
    ) -> bool {
        if self
            .maintenance_window_target(region, window_id, window_target_id)
            .is_none()
        {
            return false;
        }
        self.maintenance_window_targets
            .remove(&(region.to_string(), window_target_id.to_string()))
            .is_some()
    }

    /// Whether `window_id` can take another target ([`MAX_TARGETS_PER_WINDOW`]).
    pub fn can_hold_another_maintenance_window_target(
        &self,
        region: &str,
        window_id: &str,
    ) -> bool {
        self.maintenance_window_targets_of(region, window_id).len() < MAX_TARGETS_PER_WINDOW
    }

    /// The target `region` holds under the idempotency token `client_token`, or `None` when
    /// no register carrying that token has run there.
    pub fn maintenance_window_target_with_client_token(
        &self,
        region: &str,
        client_token: &str,
    ) -> Option<&MaintenanceWindowTargetRecord> {
        self.maintenance_window_targets
            .iter()
            .filter(|((held, _), _)| held == region)
            .map(|(_, record)| record)
            .find(|record| record.client_token.as_deref() == Some(client_token))
    }
}

/// Whether `key` is one of the filter keys a target describe takes
/// ([`TARGET_FILTER_KEYS`]).
#[must_use]
pub fn is_target_filter_key(key: &str) -> bool {
    TARGET_FILTER_KEYS.contains(&key)
}
