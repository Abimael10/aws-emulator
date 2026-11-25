// SPDX-License-Identifier: CC0-1.0

use super::*;

/// `ServiceUpdateStatus`'s values (`smithy_query(query="shape",
/// shape="ServiceUpdateStatus")`: `available | cancelled | expired`). A filter
/// naming anything else is refused by the boundary validator before a handler
/// runs, because the member is a modelled enum.
pub const SERVICE_UPDATE_AVAILABLE: &str = "available";
/// See [`SERVICE_UPDATE_AVAILABLE`].
pub const SERVICE_UPDATE_CANCELLED: &str = "cancelled";
/// See [`SERVICE_UPDATE_AVAILABLE`].
pub const SERVICE_UPDATE_EXPIRED: &str = "expired";

/// The only value `ServiceUpdateType` declares
/// (`smithy_query(query="shape", shape="ServiceUpdateType")`:
/// `security-update`).
pub const SERVICE_UPDATE_TYPE: &str = "security-update";

/// The type an END-OF-LIFE update carries: the update that asks for an engine
/// major version to be left before it stops being served.
///
/// `ServiceUpdateType`'s enum lists only [`SERVICE_UPDATE_TYPE`], while
/// ElastiCache publishes this second type beside it; `ServiceUpdate$Type` is a
/// plain string on the wire, so an update reports the type of its own kind
/// rather than the one value the enum lists.
pub const SERVICE_UPDATE_TYPE_ENGINE_MAJOR_VERSION: &str = "engine-major-version-update";

/// The `EstimatedUpdateTime` every published update carries: "The estimated
/// length of time the service update will take".
///
/// Nothing in the model or the docs states what that length is for a given
/// update, so the emulator publishes one estimate for all of them rather than
/// inventing a per-update one.
pub const SERVICE_UPDATE_ESTIMATED_TIME: &str = "15 minutes";

/// The instant the published catalog date `stamp` names.
///
/// Every date in this catalog is written as the RFC-3339 instant AWS reports it
/// as (`ServiceUpdateReleaseDate` and its siblings are `TStamp` members), and
/// read through the same timestamp implementation the rest of the service
/// serialises them with. A literal in this module that is not an instant is a
/// defect in the module, not anything a request can ask for, which is why it
/// panics rather than resolving to some other date.
fn published_date(stamp: &str) -> SystemTime {
    let parsed = ::aws_smithy_types::DateTime::from_str(
        stamp,
        ::aws_smithy_types::date_time::Format::DateTime,
    )
    .unwrap_or_else(|error| panic!("the published date {stamp} is an RFC-3339 instant: {error}"));
    SystemTime::try_from(parsed)
        .unwrap_or_else(|_| panic!("the published date {stamp} is an instant after the epoch"))
}

/// The recommended apply-by date and the end date of the entries this catalog
/// publishes as still OPEN (`ServiceUpdateRecommendedApplyByDate`: "the
/// recommendend date to apply the service update in order to ensure
/// compliance"; `ServiceUpdateEndDate`: "the date after which the service
/// update is no longer available").
///
/// A published catalog is a fixed set of dated entries, so an entry's window is
/// stated once here rather than recomputed from the clock the store came up on.
/// These two are deliberately far out: they are the dates of the entries the
/// catalog publishes as OPEN, so those entries stay available and still ahead of
/// their recommended date — and the catalog keeps publishing both an update
/// inside its window and one the clock has carried past its end date — for as
/// long as this emulator is in service.
const CATALOG_OPEN_APPLY_BY: &str = "2039-01-01T00:00:00Z";
/// See [`CATALOG_OPEN_APPLY_BY`].
const CATALOG_OPEN_END: &str = "2040-01-01T00:00:00Z";

/// One row of the service-update catalog this emulator publishes.
///
/// AWS publishes ElastiCache service updates itself — the API has no operation
/// that creates, cancels or expires one — and neither the model nor the docs
/// name any particular update, so this table is the emulator's OWN published
/// catalog rather than a claim about what AWS has released. Every column of it,
/// the DATES and the DESCRIPTION included, is fixed by the catalog: a published
/// update is the same entry on every store this build brings up, so a client
/// that read a name, a severity, a window or a description once reads the same
/// one on the next call and on the next boot. Nothing about a row is composed
/// when it is READ either — `ServiceUpdate$ServiceUpdateDescription` ("provides
/// details of the service update") is the update's own published text, not a
/// sentence assembled from the columns beside it.
struct PublishedUpdate {
    /// The tail of this row's `ServiceUpdateName`: an update's identity is part
    /// of what the service publishes, so it is the same string on every store
    /// this build brings up. See [`published_service_updates`] for the whole
    /// name.
    slug: &'static str,
    /// One of `ServiceUpdateSeverity`'s values (`critical | important | medium |
    /// low`).
    severity: &'static str,
    /// This update's own `ServiceUpdateDescription`.
    description: &'static str,
    /// "The date when the service update is initially available."
    release_date: &'static str,
    /// "The recommendend date to apply the service update in order to ensure
    /// compliance."
    recommended_apply_by_date: &'static str,
    /// "The date after which the service update is no longer available."
    end_date: &'static str,
    /// `AutoUpdateAfterRecommendedApplyByDate`: "whether the service update
    /// will be automatically applied once the recommended apply-by date has
    /// expired".
    auto_update_after_recommended_apply_by_date: bool,
}

/// The catalog's security-update rows. Every engine gets one update per row, so
/// the set spans both the availability window a published update is inside and
/// the one it has fallen out of.
const PUBLISHED_UPDATES: [PublishedUpdate; 2] = [
    PublishedUpdate {
        slug: "security-update-001",
        severity: "critical",
        description: "Addresses a critical security issue in the cache engine. Applying this \
                      update replaces the engine binary the nodes run; it does not change the \
                      engine version the cluster is configured with.",
        release_date: "2024-02-13T00:00:00Z",
        // Past: this is the entry the catalog publishes as no longer available.
        end_date: "2024-06-12T00:00:00Z",
        recommended_apply_by_date: "2024-03-14T00:00:00Z",
        auto_update_after_recommended_apply_by_date: true,
    },
    PublishedUpdate {
        slug: "security-update-002",
        severity: "important",
        description: "Addresses an important security issue in the cache engine. Applying this \
                      update replaces the engine binary the nodes run; it does not change the \
                      engine version the cluster is configured with.",
        release_date: "2025-09-09T00:00:00Z",
        recommended_apply_by_date: CATALOG_OPEN_APPLY_BY,
        end_date: CATALOG_OPEN_END,
        auto_update_after_recommended_apply_by_date: false,
    },
];

/// The engine major versions ElastiCache publishes an END-OF-LIFE update for,
/// as `(engine, how that engine is spelled in an update's name, the major
/// version segment of the name, the engine version LINE the update applies
/// to)`.
///
/// Redis OSS 6 is the line ElastiCache has announced the end of life of; the
/// engine's name in an update's name is its published product name, which is
/// why Redis OSS carries the `-oss` of it. A line added here is published the
/// moment it is announced, one update per [`EOL_RESOURCE_KINDS`] entry, so the
/// catalog states each end of life once instead of once per resource kind.
const PUBLISHED_EOL_UPDATES: [(&str, &str, &str, &str); 1] =
    [(DEFAULT_ENGINE, "redis-oss", "v6", "6.x")];

/// The KINDS of resource an end-of-life update is published for, as the tail of
/// its `ServiceUpdateName` and as the words its description names them by.
///
/// One update per kind, because the resources that run an engine version are
/// carried onto a later major version by different operations — a cache cluster
/// by `ModifyCacheCluster` and a replication group by `ModifyReplicationGroup` —
/// so a client applies the update that names the kind of resource it holds.
const EOL_RESOURCE_KINDS: [(&str, &str); 2] = [
    ("cache-clusters", "cache clusters"),
    ("replication-groups", "replication groups"),
];

/// The window an end-of-life update is published over and the severity it
/// carries.
///
/// Leaving a major version is a planned upgrade of the resources that run it
/// rather than a patch, so an end-of-life update is published well before the
/// date it is recommended to be applied by, and its window is still open — see
/// [`CATALOG_OPEN_APPLY_BY`].
const EOL_RELEASE_DATE: &str = "2025-04-15T00:00:00Z";
/// See [`EOL_RELEASE_DATE`]; one of `ServiceUpdateSeverity`'s values.
const EOL_SEVERITY: &str = "important";

/// One published service update, as this emulator holds it.
///
/// `ServiceUpdateStatus` is NOT stored: it follows from the update's dates and
/// the clock, so [`ServiceUpdateRecord::status`] derives it on every read
/// instead of letting a stored copy drift from the dates beside it.
#[derive(Debug, Clone)]
pub struct ServiceUpdateRecord {
    /// "The unique ID of the service update", and the identity
    /// `ServiceUpdateName` names it by.
    pub service_update_name: String,
    /// "The Elasticache engine to which the update applies."
    pub engine: String,
    /// "The Elasticache engine version to which the update applies."
    pub engine_version: String,
    /// One of `ServiceUpdateSeverity`'s values.
    pub severity: String,
    /// One of `ServiceUpdateType`'s values — see [`SERVICE_UPDATE_TYPE`].
    pub update_type: String,
    /// "Provides details of the service update."
    pub description: String,
    /// "The date when the service update is initially available."
    pub release_date: SystemTime,
    /// "The recommendend date to apply the service update in order to ensure
    /// compliance."
    pub recommended_apply_by_date: SystemTime,
    /// "The date after which the service update is no longer available."
    pub end_date: SystemTime,
    /// See [`PublishedUpdate::auto_update_after_recommended_apply_by_date`].
    pub auto_update_after_recommended_apply_by_date: bool,
    /// See [`SERVICE_UPDATE_ESTIMATED_TIME`].
    pub estimated_update_time: String,
}

impl ServiceUpdateRecord {
    /// This update's `ServiceUpdateStatus` at `now`.
    ///
    /// `ServiceUpdateEndDate` is "the date after which the service update is no
    /// longer available", so an update the clock has carried past it is
    /// `expired` and one still inside its window is `available`. The third
    /// status, `cancelled`, is the status of an update AWS withdrew before its
    /// end date; this emulator publishes none, and no operation in this service
    /// withdraws one, so no record ever reports it.
    pub fn status(&self, now: SystemTime) -> &'static str {
        if now >= self.end_date {
            SERVICE_UPDATE_EXPIRED
        } else {
            SERVICE_UPDATE_AVAILABLE
        }
    }

    /// This record as the modelled `ServiceUpdate`, with its status taken from
    /// the clock at `now`.
    pub fn to_modeled(&self, now: SystemTime) -> ServiceUpdate {
        ServiceUpdate {
            service_update_name: Some(self.service_update_name.clone()),
            service_update_release_date: Some(::aws_smithy_types::DateTime::from(self.release_date)),
            service_update_end_date: Some(::aws_smithy_types::DateTime::from(self.end_date)),
            service_update_severity: Some(self.severity.clone()),
            service_update_recommended_apply_by_date: Some(::aws_smithy_types::DateTime::from(
                self.recommended_apply_by_date,
            )),
            service_update_status: Some(self.status(now).to_string()),
            service_update_description: Some(self.description.clone()),
            service_update_type: Some(self.update_type.clone()),
            engine: Some(self.engine.clone()),
            engine_version: Some(self.engine_version.clone()),
            auto_update_after_recommended_apply_by_date: Some(
                self.auto_update_after_recommended_apply_by_date,
            ),
            estimated_update_time: Some(self.estimated_update_time.clone()),
        }
    }
}

/// The published service-update catalog, keyed by `ServiceUpdateName`.
///
/// Two kinds of update make it up, which is what gives
/// `BatchApplyUpdateAction` / `DescribeUpdateActions` the names a client reads
/// off this catalog:
///
/// * a SECURITY update per engine per row of [`PUBLISHED_UPDATES`], named
///   `elasticache-<engine>-<slug>`: the service, the engine the update applies
///   to and the row's own [`PublishedUpdate::slug`];
/// * an END-OF-LIFE update per resource kind per row of
///   [`PUBLISHED_EOL_UPDATES`], named
///   `elasticache-<engine-name>-<major>-eol-<resource-kind>`.
///
/// Every entry is fixed by the catalog — its name, its engine, its severity, its
/// type AND its three dates — so the set a store publishes does not depend on
/// the instant that store came up, and a client that reads it twice, or reads it
/// from two stores, sees one catalog. A `BTreeMap` is what gives
/// `DescribeServiceUpdates` the stable total order its `Marker` indexes into.
pub fn published_service_updates() -> BTreeMap<String, ServiceUpdateRecord> {
    let mut catalog = BTreeMap::new();
    for row in &PUBLISHED_UPDATES {
        for engine in ENGINES {
            let name = format!("{ARN_NAMESPACE}-{engine}-{}", row.slug);
            let engine_version = default_engine_version(engine);
            catalog.insert(
                name.clone(),
                ServiceUpdateRecord {
                    service_update_name: name,
                    engine: engine.to_string(),
                    engine_version: engine_version.to_string(),
                    severity: row.severity.to_string(),
                    update_type: SERVICE_UPDATE_TYPE.to_string(),
                    description: row.description.to_string(),
                    release_date: published_date(row.release_date),
                    recommended_apply_by_date: published_date(row.recommended_apply_by_date),
                    end_date: published_date(row.end_date),
                    auto_update_after_recommended_apply_by_date: row
                        .auto_update_after_recommended_apply_by_date,
                    estimated_update_time: SERVICE_UPDATE_ESTIMATED_TIME.to_string(),
                },
            );
        }
    }
    for (engine, engine_in_name, major_in_name, version_line) in PUBLISHED_EOL_UPDATES {
        for (kind_in_name, kind_words) in EOL_RESOURCE_KINDS {
            let name =
                format!("{ARN_NAMESPACE}-{engine_in_name}-{major_in_name}-eol-{kind_in_name}");
            catalog.insert(
                name.clone(),
                ServiceUpdateRecord {
                    service_update_name: name,
                    engine: engine.to_string(),
                    // "The Elasticache engine version to which the update
                    // applies": an end-of-life update applies to a whole
                    // version LINE rather than to one release, so the version
                    // it reports is the engine and that line.
                    engine_version: format!("{engine} {version_line}"),
                    severity: EOL_SEVERITY.to_string(),
                    update_type: SERVICE_UPDATE_TYPE_ENGINE_MAJOR_VERSION.to_string(),
                    description: format!(
                        "{engine} {version_line} is reaching end of life: upgrade your \
                         {kind_words} to a later major version."
                    ),
                    release_date: published_date(EOL_RELEASE_DATE),
                    recommended_apply_by_date: published_date(CATALOG_OPEN_APPLY_BY),
                    end_date: published_date(CATALOG_OPEN_END),
                    // A major version upgrade changes what the resource runs,
                    // so it is applied when the customer asks for it rather
                    // than automatically once the apply-by date has passed.
                    auto_update_after_recommended_apply_by_date: false,
                    estimated_update_time: SERVICE_UPDATE_ESTIMATED_TIME.to_string(),
                },
            );
        }
    }
    catalog
}
