// SPDX-License-Identifier: CC0-1.0

//! The health of a billing view: what every read of one reports and what every
//! writer of one consults before it acts.
//!
//! A billing view aggregates OTHER billing views, and it is only as sound as the
//! state it depends on — the standing of the account that owns it and the views
//! it draws from. So `BillingViewHealthStatus` is DERIVED here, on every read,
//! from exactly that state ([`ComAmazonawsBillingAwsbillingState::view_health`]):
//!
//! * a `CUSTOM` view's definition is one only the organization's management
//!   account may make (`CreateBillingView`'s own gate), so while the account that
//!   owns such a view is not the management account of the organization it
//!   belongs to, the view carries `VIEW_OWNER_NOT_MANAGEMENT_ACCOUNT` and is
//!   `UNHEALTHY`;
//! * a change to a source view's DEFINITION has to reach the views that
//!   aggregate it, which takes [`SOURCE_VIEW_PROPAGATION`]: until it has, each of
//!   them carries `SOURCE_VIEW_UPDATING` and is `UPDATING`;
//! * a source view that is itself `UNHEALTHY` leaves nothing sound to aggregate,
//!   so the view drawing from it carries `SOURCE_VIEW_UNHEALTHY` and is
//!   `UNHEALTHY` too, and a source that is settling propagates its `UPDATING` the
//!   same way — one event therefore reaches every view down the chain from where
//!   it happened.
//!
//! Nothing here writes: the health a read reports is computed from the records
//! and the clock, so it needs no writer to keep it current and cannot go stale.
//! A view whose own health the service RECORDED
//! ([`BillingView::recorded_health`]) reports that record instead, which is the
//! condition that belongs to the view itself rather than to anything it depends
//! on.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

use super::{
    BillingView, ComAmazonawsBillingAwsbillingState, BILLING_VIEW_STATUS_HEALTHY,
};
use crate::ops::types::BillingViewHealthStatus;

/// `UNHEALTHY`, a `BillingViewStatus` value (`BILLING_VIEW_STATUS_VALUES` in
/// `ops::types`): the view cannot serve its data, and the reasons say why.
pub const BILLING_VIEW_STATUS_UNHEALTHY: &str = "UNHEALTHY";

/// `UPDATING`, a `BillingViewStatus` value: a change the view's data depends on
/// is still being applied.
pub const BILLING_VIEW_STATUS_UPDATING: &str = "UPDATING";

/// `SOURCE_VIEW_UNHEALTHY`, a `BillingViewStatusReason` value
/// (`BILLING_VIEW_STATUS_REASON_VALUES` in `ops::types`): a billing view this
/// one draws from is itself unhealthy.
pub const STATUS_REASON_SOURCE_VIEW_UNHEALTHY: &str = "SOURCE_VIEW_UNHEALTHY";

/// `SOURCE_VIEW_UPDATING`, a `BillingViewStatusReason` value: a billing view
/// this one draws from has a definition change that has not reached it yet.
pub const STATUS_REASON_SOURCE_VIEW_UPDATING: &str = "SOURCE_VIEW_UPDATING";

/// `VIEW_OWNER_NOT_MANAGEMENT_ACCOUNT`, a `BillingViewStatusReason` value: the
/// account that owns the view is not the management account of the organization
/// it belongs to.
pub const STATUS_REASON_VIEW_OWNER_NOT_MANAGEMENT_ACCOUNT: &str =
    "VIEW_OWNER_NOT_MANAGEMENT_ACCOUNT";

/// How long a change to one billing view's definition takes to reach the views
/// that aggregate it.
///
/// A span this emulator chooses — AWS publishes none for it — kept short enough
/// that a caller can watch a derived view settle and long enough that a caller
/// acting straight after the write still observes the `UPDATING` the modeled
/// `BillingViewStatus` declares. The instant a view's definition last changed is
/// stored ([`BillingView::view_definition_last_updated_at`]) and the span is read
/// off it, so nothing waits for the clock to pass.
pub const SOURCE_VIEW_PROPAGATION: Duration = Duration::from_secs(3);

/// The health of one billing view: the `BillingViewStatus` it carries and the
/// `BillingViewStatusReason` values that explain it.
///
/// The reasons are EMPTY for a status that needs no explaining — a `HEALTHY`
/// view — rather than absent, exactly as [`BillingView::health_status_reasons`]
/// holds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewHealth {
    /// A `BillingViewStatus` value, e.g. [`BILLING_VIEW_STATUS_HEALTHY`].
    pub status_code: String,
    /// The `BillingViewStatusReason` values explaining [`Self::status_code`].
    pub status_reasons: Vec<String>,
}

impl ViewHealth {
    /// The health of a view that depends on nothing unsound: `HEALTHY`, with no
    /// reason to report.
    pub fn healthy() -> Self {
        Self {
            status_code: BILLING_VIEW_STATUS_HEALTHY.to_string(),
            status_reasons: Vec::new(),
        }
    }

    /// Whether this is [`BILLING_VIEW_STATUS_HEALTHY`], the one status in which
    /// an operation may act on the view: every other value is a state the
    /// modeled `BillingViewHealthStatusException` names — see
    /// [`super::billing_view_not_healthy`].
    pub fn is_healthy(&self) -> bool {
        self.status_code == BILLING_VIEW_STATUS_HEALTHY
    }
}

impl BillingView {
    /// The health the service RECORDED on this view itself, or `None` when its
    /// health is the one derived from what it depends on.
    ///
    /// A recorded condition is one about THIS view rather than about its owner's
    /// standing or its sources — so it answers for the view whatever those say,
    /// and a view carrying any recorded reason reports that reason list even
    /// beside a `HEALTHY` code.
    pub fn recorded_health(&self) -> Option<ViewHealth> {
        let recorded = self.health_status_code != BILLING_VIEW_STATUS_HEALTHY
            || !self.health_status_reasons.is_empty();
        recorded.then(|| ViewHealth {
            status_code: self.health_status_code.clone(),
            status_reasons: self.health_status_reasons.clone(),
        })
    }

    /// Whether this view's definition changed so recently that the change has
    /// not reached the views that aggregate it yet — see
    /// [`SOURCE_VIEW_PROPAGATION`].
    ///
    /// A view whose definition never changed has nothing propagating, and an
    /// instant that is not yet in the past (a stored instant ahead of this
    /// reading of the clock) is a change that has not even begun to propagate.
    pub fn definition_settling(&self, now: SystemTime) -> bool {
        self.view_definition_last_updated_at.is_some_and(|changed| {
            now.duration_since(changed)
                .map(|since| since < SOURCE_VIEW_PROPAGATION)
                .unwrap_or(true)
        })
    }

    /// Whether this view's definition is one only the organization's management
    /// account may make, so the owner's standing in that organization is part of
    /// the view's health.
    ///
    /// A `CUSTOM` view is: `CreateBillingView`, the only way one comes to exist,
    /// admits no other caller (see
    /// [`super::is_organization_management_account`]). Every other
    /// `BillingViewType` names a view the service maintains for the account that
    /// owns it ([`BillingView::is_managed`]), which no organization membership
    /// can invalidate.
    pub fn owner_must_manage_organization(&self) -> bool {
        !self.is_managed()
    }
}

impl ComAmazonawsBillingAwsbillingState {
    /// The health of the billing view stored under `arn`, or `None` when this
    /// store holds no view under it.
    ///
    /// `management_account` is the management account of the organization this
    /// emulated account belongs to, as
    /// [`super::organization_management_account_id`] reads it off the
    /// Organizations store — resolved by the caller BEFORE this store is locked,
    /// so serving one request never holds two services' stores at once.
    pub fn health_of(
        &self,
        arn: &str,
        management_account: Option<&str>,
        now: SystemTime,
    ) -> Option<ViewHealth> {
        let view = self.billing_views.get(arn)?;
        Some(self.view_health(view, management_account, now))
    }

    /// The health of `view`, derived from the standing of its owner and from the
    /// views it draws from, down the whole chain of them.
    ///
    /// The walk expands each view at most once and keeps what it resolved, so a
    /// chain this store holds costs one visit per view and per association
    /// however many views share a source; it holds no recursion, so no stored
    /// chain can size the stack. A source on the walk that is not resolved yet —
    /// which is only a view whose own source chain leads back to it, the state
    /// every writer of a source list refuses to create (see
    /// [`ComAmazonawsBillingAwsbillingState::draws_from`]) — contributes nothing
    /// rather than looping.
    pub fn view_health(
        &self,
        view: &BillingView,
        management_account: Option<&str>,
        now: SystemTime,
    ) -> ViewHealth {
        let mut resolved: BTreeMap<&str, ViewHealth> = BTreeMap::new();
        let mut expanded: BTreeSet<&str> = BTreeSet::new();
        // Each entry is a view to visit and whether the views it draws from have
        // been pushed already: a view is finalised only once they have, so every
        // source it reads is one this walk has resolved.
        let mut pending: Vec<(&str, bool)> = vec![(view.arn.as_str(), false)];
        expanded.insert(view.arn.as_str());
        while let Some((arn, sources_pushed)) = pending.pop() {
            let Some(current) = self.billing_views.get(arn) else {
                continue;
            };
            if let Some(recorded) = current.recorded_health() {
                resolved.insert(current.arn.as_str(), recorded);
                continue;
            }
            if !sources_pushed {
                pending.push((current.arn.as_str(), true));
                for source in &current.source_views {
                    if expanded.insert(source.as_str()) {
                        pending.push((source.as_str(), false));
                    }
                }
                continue;
            }
            let health = self.own_health(current, management_account, now, &resolved);
            resolved.insert(current.arn.as_str(), health);
        }
        resolved
            .remove(view.arn.as_str())
            .unwrap_or_else(ViewHealth::healthy)
    }

    /// The health of one view, given the health already resolved for the views it
    /// draws from.
    fn own_health(
        &self,
        view: &BillingView,
        management_account: Option<&str>,
        now: SystemTime,
        resolved: &BTreeMap<&str, ViewHealth>,
    ) -> ViewHealth {
        // The owner's standing: an account that belongs to no organization at all
        // owns its own views — it is nobody's member account — so the reason bites
        // only while an organization exists that some OTHER account manages.
        let owner_not_management_account = view.owner_must_manage_organization()
            && management_account.is_some_and(|managing| managing != view.owner_account_id);
        let mut source_unhealthy = false;
        let mut source_updating = false;
        for source_arn in &view.source_views {
            let Some(source) = self.billing_views.get(source_arn) else {
                // Every writer of a source list resolves each ARN against this
                // store and a delete takes the deleted view out of every list
                // that named it (`Self::forget_view`), so no stored source names
                // a view this store does not hold.
                continue;
            };
            if source.definition_settling(now) {
                source_updating = true;
            }
            match resolved
                .get(source.arn.as_str())
                .map(|health| health.status_code.as_str())
            {
                Some(BILLING_VIEW_STATUS_UNHEALTHY) => source_unhealthy = true,
                Some(BILLING_VIEW_STATUS_HEALTHY) | None => {}
                // Every other `BillingViewStatus` (`UPDATING`, `CREATING`) is a
                // source still settling, whichever of them it is.
                Some(_) => source_updating = true,
            }
        }
        // The reasons in the order the modeled `BillingViewStatusReason` enum
        // declares them, so one view's health reads the same whichever read
        // reported it.
        let mut status_reasons = Vec::new();
        if source_unhealthy {
            status_reasons.push(STATUS_REASON_SOURCE_VIEW_UNHEALTHY.to_string());
        }
        if source_updating {
            status_reasons.push(STATUS_REASON_SOURCE_VIEW_UPDATING.to_string());
        }
        if owner_not_management_account {
            status_reasons.push(STATUS_REASON_VIEW_OWNER_NOT_MANAGEMENT_ACCOUNT.to_string());
        }
        // A view that cannot serve its data is UNHEALTHY whatever else is also
        // true of it; one that only waits for a change to arrive is UPDATING.
        let status_code = if owner_not_management_account || source_unhealthy {
            BILLING_VIEW_STATUS_UNHEALTHY
        } else if source_updating {
            BILLING_VIEW_STATUS_UPDATING
        } else {
            BILLING_VIEW_STATUS_HEALTHY
        };
        ViewHealth {
            status_code: status_code.to_string(),
            status_reasons,
        }
    }
}

/// The `BillingViewHealthStatus` a read reports for a view whose health is
/// `health`.
///
/// Both members are always present: the status code and the reason list, which
/// is EMPTY for a status that needs no explaining rather than absent. Shared by
/// every read that projects a billing view, so `GetBillingView` and
/// `ListBillingViews` cannot report one view's health two ways.
pub fn health_status(health: &ViewHealth) -> BillingViewHealthStatus {
    BillingViewHealthStatus {
        status_code: Some(health.status_code.clone()),
        status_reasons: Some(health.status_reasons.clone()),
    }
}
