// SPDX-License-Identifier: CC0-1.0

//! The resiliency groups the ACCOUNT holds — the `ResiliencyGroup` shape's data, owned
//! by this service rather than rebuilt per response.
//!
//! A group is minted by `CreateResiliencyGroup`, read by `GetResiliencyGroup` and
//! `ListResiliencyGroups`, renamed by `UpdateResiliencyGroup` and torn down by
//! `DeleteResiliencyGroup`, and every one of them renders the stored record through
//! [`ResiliencyGroup::output`] / [`ResiliencyGroup::summary`], so a create followed by a
//! read answers the same bytes.
//!
//! The ARN is derived from the id rather than stored beside it
//! ([`resiliency_group_arn`]), so the two can never disagree:
//! `ResiliencyGroupArn`'s own pattern
//! (`^arn:(aws|aws-cn|aws-us-gov):directconnect::[0-9]{12}:dx-resiliency-group/dxrg-[0-9a-z]{17}$`)
//! builds the ARN out of the partition, the account and the group's own id, and carries
//! no Region at all.

use super::{ComAmazonawsDirectconnectOvertureserviceState, DueTransition};
use crate::ops::types::Tag;

/// `ResiliencyGroupState`'s `pending`: the state a group is in between the create that
/// asked for it and the moment it is ready to group connections.
pub const RESILIENCY_GROUP_STATE_PENDING: &str = "pending";

/// `ResiliencyGroupState`'s `available`: the state a group that is ready to be used is
/// reported in, which is the move the clock owes a `pending` group.
pub const RESILIENCY_GROUP_STATE_AVAILABLE: &str = "available";

/// `ResiliencyGroupState`'s `deleting`: the state a group is in between the delete that
/// asked for it and the moment it is gone.
pub const RESILIENCY_GROUP_STATE_DELETING: &str = "deleting";

/// `ResiliencyGroupState`'s `deleted`: the TERMINAL state, which the clock owes a
/// `deleting` group.
///
/// A group that reaches it stays READABLE under its own id rather than being removed:
/// recorded behaviour has a deleted group still reported, in this state, rather than
/// answering that the group does not exist. The record is reclaimed only when the store
/// needs the room ([`ComAmazonawsDirectconnectOvertureserviceState::reclaim_deleted`]).
pub const RESILIENCY_GROUP_STATE_DELETED: &str = "deleted";

/// The one `ResiliencyGroupType` the model declares: "The type of the resiliency group.
/// The valid value is `Managed`."
pub const RESILIENCY_GROUP_TYPE_MANAGED: &str = "Managed";

/// The prefix a `resiliencyGroupId` carries, and how many characters follow it:
/// `ResiliencyGroupId` is `@length 22` with the pattern
/// `^(dxrg|DXRG)-[0-9a-zA-Z]{17}$`, so an id is the prefix, a hyphen and 17 more
/// characters. The minted ones are drawn from the LOWERCASE alphabet, because
/// `ResiliencyGroupArn`'s own pattern admits only `[0-9a-z]{17}` after the prefix.
pub const RESILIENCY_GROUP_ID_PREFIX: &str = "dxrg";
pub const RESILIENCY_GROUP_ID_DIGITS: usize = 17;

/// The resource path a resiliency group's ARN carries before the group's own id, from
/// `ResiliencyGroupArn`'s pattern.
pub const RESILIENCY_GROUP_ARN_PATH: &str = "dx-resiliency-group";

/// The longest `resiliencyGroupName` the model admits (`@length 1..255`), which is also
/// its own `@pattern`'s alphabet (`^[a-zA-Z0-9_\-]+$`): both are MODELLED constraints,
/// so a name that fails either is refused before any handler runs and no handler checks
/// them again. Stated here for the callers that have to name the bound — a test that
/// drives a name AT it, the catalogue that documents it.
pub const MAX_RESILIENCY_GROUP_NAME_LEN: usize = 255;

/// A stored resiliency group — the `ResiliencyGroup` shape's data.
///
/// The group holds no Region of its own: its ARN carries none, so like a Direct Connect
/// gateway it is not a record of one endpoint's Region.
#[derive(Debug, Clone)]
pub struct ResiliencyGroup {
    /// `ResiliencyGroup$resiliencyGroupId`, in the `dxrg-…` form this service mints.
    pub resiliency_group_id: String,
    /// `ResiliencyGroup$resiliencyGroupName`.
    pub resiliency_group_name: String,
    /// The `ResiliencyModel` the group "is intended to meet", which
    /// `CreateResiliencyGroup` requires. The `ResiliencyGroup` shape publishes no member
    /// for it — the group reports its TYPE, not the model it targets — so it is held
    /// here as what the group was created for rather than rendered into a response.
    pub intended_resiliency_model: String,
    /// `ResiliencyGroup$ownerAccount`.
    pub owner_account: String,
    /// One of the four `RESILIENCY_GROUP_STATE_*` members above.
    pub state: String,
    /// `ResiliencyGroup$tags`.
    pub tags: Vec<Tag>,
    /// The `clientToken` the create that minted this group carried, if any — "a
    /// unique, case-sensitive identifier that you provide to ensure the idempotency of
    /// the request", so a create repeated under it answers this group rather than
    /// minting a second one.
    pub client_token: Option<String>,
    /// The state change the clock owes this group.
    pub due: Option<DueTransition>,
}

impl ResiliencyGroup {
    /// This group's ARN, derived from its id ([`resiliency_group_arn`]).
    pub fn arn(&self) -> String {
        resiliency_group_arn(&self.resiliency_group_id)
    }

    /// Render this record into the generated `ResiliencyGroup` output shape: the ONE
    /// rendering of a stored group, so every operation answers the same field set.
    pub fn output(&self) -> crate::ops::types::ResiliencyGroup {
        crate::ops::types::ResiliencyGroup {
            resiliency_group_id: Some(self.resiliency_group_id.clone()),
            resiliency_group_arn: Some(self.arn()),
            resiliency_group_name: Some(self.resiliency_group_name.clone()),
            resiliency_group_type: Some(RESILIENCY_GROUP_TYPE_MANAGED.to_string()),
            owner_account: Some(self.owner_account.clone()),
            state: Some(self.state.clone()),
            // `TagList` is `@length 1..`, so a group nobody tagged publishes no list at
            // all rather than an empty one.
            tags: (!self.tags.is_empty()).then(|| self.tags.clone()),
        }
    }

    /// Render this record into the generated `ResiliencyGroupSummary` shape a listing
    /// carries — the output shape's members LESS the tags, which `ResiliencyGroupSummary`
    /// does not publish.
    pub fn summary(&self) -> crate::ops::types::ResiliencyGroupSummary {
        crate::ops::types::ResiliencyGroupSummary {
            resiliency_group_id: Some(self.resiliency_group_id.clone()),
            resiliency_group_arn: Some(self.arn()),
            resiliency_group_name: Some(self.resiliency_group_name.clone()),
            resiliency_group_type: Some(RESILIENCY_GROUP_TYPE_MANAGED.to_string()),
            owner_account: Some(self.owner_account.clone()),
            state: Some(self.state.clone()),
        }
    }
}

/// The ARN of the resiliency group `resiliency_group_id`, as `ResiliencyGroupArn`'s own
/// pattern builds one: this service's ARN namespace, NO Region, the account and the
/// `dx-resiliency-group/<id>` resource path.
pub fn resiliency_group_arn(resiliency_group_id: &str) -> String {
    ::emulator_server::emulator_arn(
        super::DIRECT_CONNECT_ARN_NAMESPACE,
        "",
        &format!("{RESILIENCY_GROUP_ARN_PATH}/{resiliency_group_id}"),
    )
}

impl ComAmazonawsDirectconnectOvertureserviceState {
    /// Mint the next `resiliencyGroupId`: the `dxrg-` prefix and
    /// [`RESILIENCY_GROUP_ID_DIGITS`] lowercase base36 characters derived from this
    /// store's own monotonic counter, so every minted id is unique within a run and
    /// satisfies both `ResiliencyGroupId`'s pattern and the narrower alphabet
    /// `ResiliencyGroupArn` admits.
    pub fn next_resiliency_group_id(&mut self) -> String {
        self.id_counter += 1;
        // 17 base36 digits exceed what one `u64` spans, so the id is two derived runs:
        // the counter itself in the low digits, and a mix of it above, which is what
        // keeps the whole label unique rather than only its tail.
        let low = super::base36(self.id_counter, RESILIENCY_GROUP_ID_DIGITS - 8);
        let high = super::base36(super::fnv1a64(&self.id_counter.to_be_bytes()), 8);
        format!("{RESILIENCY_GROUP_ID_PREFIX}-{high}{low}")
    }

    /// The stored resiliency groups in creation order, newest last.
    pub fn resiliency_groups_in_order(&self) -> Vec<&ResiliencyGroup> {
        self.resiliency_group_order
            .iter()
            .filter_map(|id| self.resiliency_groups.get(id))
            .collect()
    }

    /// Apply every resiliency-group state change the clock has already made due — the
    /// group half of
    /// [`ComAmazonawsDirectconnectOvertureserviceState::settle_due_transitions`].
    ///
    /// Reaching [`RESILIENCY_GROUP_STATE_DELETED`] leaves the record in place: a deleted
    /// group stays readable in that state, and the room it holds is reclaimed only when
    /// the store needs it.
    pub fn settle_due_resiliency_groups(&mut self) {
        let now = ::std::time::Instant::now();
        for group in self.resiliency_groups.values_mut() {
            if let Some(due) = &group.due {
                if now >= due.at {
                    group.state = due.to.clone();
                    group.due = None;
                }
            }
        }
    }
}
