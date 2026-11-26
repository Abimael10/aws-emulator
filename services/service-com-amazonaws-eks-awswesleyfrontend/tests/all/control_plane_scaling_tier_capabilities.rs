// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the CAPABILITY FIGURES the provisioned control-plane
//! scaling tiers publish on every row of the Kubernetes version catalog.
//!
//! `cluster_version_catalog.rs` covers the tier LADDER — that every row publishes
//! the modelled tier names, bounded by the modelled list length, with capabilities
//! that grow from tier to tier. The RULE under test here is about the figures
//! themselves: each tier the published tier table states figures for reports THOSE
//! figures, on every row of the calendar, rather than figures computed from the
//! tier below it — so a caller sizing a control plane reads the published capacity
//! of the tier it is about to ask for.
//!
//! The assertions run over a TABLE of the tiers the published table states figures
//! for — `tier-xl` and `tier-2xl`, which the issue this suite comes with does not
//! cite, beside `tier-4xl`, which it does — crossed with every row of the
//! published calendar, because the tier catalog is published by the service rather
//! than per Kubernetes version. A tier whose figures the published table cannot be
//! cited for is asserted as part of the ladder instead, not against a literal.

use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::REGION;
use crate::wire::{call, emulator, Answer};

/// The tiers the published provisioned control-plane tier table states figures
/// for, as `(tierName, apiRequestConcurrency, podSchedulingRatePerSecond,
/// clusterDatabaseSizeGb)`.
///
/// SOURCE: the provisioned control-plane tier table the Amazon EKS User Guide
/// publishes for its provisioned control planes ("Provisioned control plane", the
/// scaling-tier table that states each tier's API-request concurrency,
/// pod-scheduling rate and cluster database size). `tier-xl` is the smallest
/// provisioned tier, `tier-2xl` doubles all three of its figures, and `tier-4xl`
/// is published at 8,001 concurrent requests rather than at a doubling of the tier
/// below. The largest tier, `tier-8xl`, is deliberately NOT a row here: the
/// published table cannot be cited for it, so this suite holds it to the ladder
/// below rather than to a figure.
const PUBLISHED_TIERS: &[(&str, i64, i64, i64)] = &[
    ("tier-xl", 2_000, 167, 16),
    ("tier-2xl", 4_000, 334, 32),
    ("tier-4xl", 8_001, 668, 64),
];

/// The tier whose published figures this suite cannot cite; see
/// [`PUBLISHED_TIERS`].
const UNCITED_TIER: &str = "tier-8xl";

async fn describe_versions(router: &MultiServiceRouter, query: &str) -> Answer {
    let uri = if query.is_empty() {
        "/cluster-versions".to_string()
    } else {
        format!("/cluster-versions?{query}")
    };
    call(router, REGION, "GET", &uri, "").await
}

/// Every row of the published calendar, read with `includeAll` so the table spans
/// the whole publication rather than the rows a cluster can be created on today.
async fn whole_calendar(router: &MultiServiceRouter) -> Vec<serde_json::Value> {
    let answer = describe_versions(router, "includeAll=true&maxResults=100").await;
    assert_eq!(
        answer.status, 200,
        "setup DescribeClusterVersions: expected HTTP 200, got {answer:?}"
    );
    let rows = answer.body["clusterVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a clusterVersions list, got {:?}", answer.body))
        .clone();
    assert!(
        !rows.is_empty(),
        "setup DescribeClusterVersions: the calendar must publish at least one row, got {:?}",
        answer.body
    );
    rows
}

/// One tier of one row, by name.
fn tier_of<'row>(
    tiers: &'row [serde_json::Value],
    tier_name: &str,
) -> Option<&'row serde_json::Value> {
    tiers
        .iter()
        .find(|tier| tier["tierName"].as_str() == Some(tier_name))
}

/// One tier's three published capabilities, in the order the published table
/// states them.
fn capabilities(case: &str, tier: &serde_json::Value) -> (i64, i64, i64) {
    let read = |member: &str| {
        tier[member].as_i64().unwrap_or_else(|| {
            panic!("case {case}: expected an integer {member}, got {tier:?}")
        })
    };
    (
        read("apiRequestConcurrency"),
        read("podSchedulingRatePerSecond"),
        read("clusterDatabaseSizeGb"),
    )
}

// ---------------------------------------------------------------------------
// RULE: each tier the published tier table states figures for reports exactly
// those figures, on every row of the calendar.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_cited_tier_reports_its_published_capabilities() {
    let router = emulator();

    for row in whole_calendar(&router).await {
        let version = row["clusterVersion"]
            .as_str()
            .unwrap_or_else(|| panic!("expected a clusterVersion, got {row:?}"))
            .to_string();
        let tiers = row["controlPlaneScalingTiers"]
            .as_array()
            .unwrap_or_else(|| {
                panic!("case {version}: expected a controlPlaneScalingTiers list, got {row:?}")
            })
            .clone();

        for (tier_name, concurrency, pods, database) in PUBLISHED_TIERS {
            let case = format!("{version}/{tier_name}");
            let published = tier_of(&tiers, tier_name).unwrap_or_else(|| {
                panic!("case {case}: the row must publish this tier, got {tiers:?}")
            });
            assert_eq!(
                capabilities(&case, published),
                (*concurrency, *pods, *database),
                "case {case}: expected the published figures ({concurrency}, {pods}, \
                 {database}), got {published:?}"
            );
        }

        // The tier whose figures the published table cannot be cited for is held
        // to the ladder instead: it is published, and every capability of it is
        // above the largest tier the table does state figures for.
        let case = format!("{version}/{UNCITED_TIER}");
        let largest_cited = PUBLISHED_TIERS
            .last()
            .map(|(tier_name, _, _, _)| *tier_name)
            .unwrap_or_default();
        let below = tier_of(&tiers, largest_cited).unwrap_or_else(|| {
            panic!("case {case}: the row must publish {largest_cited}, got {tiers:?}")
        });
        let uncited = tier_of(&tiers, UNCITED_TIER).unwrap_or_else(|| {
            panic!("case {case}: the row must publish the largest tier, got {tiers:?}")
        });
        let (below_concurrency, below_pods, below_database) = capabilities(&case, below);
        let (concurrency, pods, database) = capabilities(&case, uncited);
        assert!(
            concurrency > below_concurrency && pods > below_pods && database > below_database,
            "case {case}: every capability of the largest tier is above {largest_cited}'s \
             ({below_concurrency}, {below_pods}, {below_database}), got ({concurrency}, {pods}, \
             {database})"
        );
    }
}
