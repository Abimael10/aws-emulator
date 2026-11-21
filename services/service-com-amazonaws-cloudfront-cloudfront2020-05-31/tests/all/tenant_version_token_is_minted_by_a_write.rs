// SPDX-License-Identifier: CC0-1.0

//! A distribution tenant's version token is minted by a WRITE, never by a read or by the
//! clock, end to end over HTTP.
//!
//! RULE: a version token identifies the configuration a resource currently runs under —
//! `DistributionTenantSummary$ETag` is "the current version of the distribution tenant", the
//! same value the tenant's own read reports in its `ETag` header and the only value its
//! `If-Match` is satisfied by — so it is minted by the call that CHANGES the tenant and by
//! nothing else: every reader answers with that one token, however often it is asked, and the
//! transition from `InProgress` to `Deployed` that the clock owes the tenant (materialised by
//! whichever read arrives after it is due) leaves the token exactly as the last write left
//! it, still the tenant's precondition.
//!
//! [`crate::tenant_listing_version_token`] pins the half a WRITE owns: that a listing reports
//! the token the create, an update and a web ACL association handed back. What this suite adds
//! is the half the CLOCK owns, which no suite covered: a token a read or a propagation moved
//! is a token no client can hold on to — the listing a client discovers a tenant through would
//! hand out a version the tenant's `If-Match` has already stopped accepting, so a conditional
//! update or delete of a tenant that nobody changed is refused `PreconditionFailed`.
//!
//! The rows span what the rule generalises over: what last versioned the tenant (its create
//! alone, or an `UpdateDistributionTenant`), the customizations it was created with (the
//! starting states [`crate::tenant_web_acl_association`] spans), and which reader is asked —
//! the tenant's own read, an unfiltered listing, a listing filtered by the tenant's
//! distribution, and a walk of one-entry pages.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s, the tenants are created and waited
//! for through [`crate::tenant_web_acl_association`]'s helpers, the listing is
//! [`crate::distribution_tenants_list`]'s and the update [`crate::distribution_tenant_update`]'s,
//! so nothing here grows a second copy of any of them.

use std::collections::BTreeMap;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    STATUS_DEPLOYED, STATUS_IN_PROGRESS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{emulator, is_version_token, Answer};
use crate::distribution_tenant_lifecycle::multi_tenant_distribution;
use crate::distribution_tenant_update::{update_tenant, update_xml};
use crate::distribution_tenants_list::{list_body, list_tenants, summaries};
use crate::tenant_web_acl_association::{
    awaited_tenant_deployment, create_tenant, read_tenant, Start, STARTS,
};

// ---------------------------------------------------------------------------
// The tenants under test
// ---------------------------------------------------------------------------

/// What last versioned a tenant before any of it is read.
#[derive(Debug, Clone, Copy)]
enum Versioned {
    /// Nothing since the create: the token the create handed back is the current one.
    TheCreate,
    /// An `UpdateDistributionTenant`, which mints a fresh version.
    AnUpdate,
}

/// One tenant a case holds: what it was created with, and what last versioned it.
struct Row {
    case: &'static str,
    start: Start,
    versioned: Versioned,
}

/// The rows every assertion below runs over: each way a tenant comes to carry the version
/// every reader has to report, crossed with a starting state of its own, so the tenants read
/// together differ in their customizations as well as in what versioned them.
fn rows() -> Vec<Row> {
    [
        ("the create alone", Versioned::TheCreate),
        ("an update", Versioned::AnUpdate),
    ]
    .iter()
    .enumerate()
    .map(|(index, (case, versioned))| Row {
        case,
        start: STARTS[index % STARTS.len()],
        versioned: *versioned,
    })
    .collect()
}

/// One tenant as this suite tracks it: its id and the version token the last write to it
/// minted.
struct Tracked {
    case: &'static str,
    id: String,
    current: String,
}

/// Create one tenant per row under `distribution_id`, apply what each row says versioned it,
/// and report them in row order.
async fn tracked_tenants(
    router: &MultiServiceRouter,
    distribution_id: &str,
    rows: &[Row],
    slug_prefix: &str,
) -> Vec<Tracked> {
    let mut tenants: Vec<Tracked> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let slug = format!("{slug_prefix}{index}");
        let (id, created) =
            create_tenant(router, distribution_id, &slug, row.case, row.start).await;
        assert!(
            is_version_token(&created),
            "case {}: expected a minted version token in the create's ETag header, got {created:?}",
            row.case
        );
        let current = match row.versioned {
            Versioned::TheCreate => created,
            Versioned::AnUpdate => {
                let updated = update_tenant(
                    router,
                    &id,
                    &update_xml("<Enabled>false</Enabled>"),
                    Some(&created),
                )
                .await;
                assert_eq!(
                    updated.status, 200,
                    "case {}: expected the update gated on the tenant's own version to be served, \
                     got {} with {}",
                    row.case, updated.status, updated.raw
                );
                let fresh = updated.header("etag").to_string();
                assert!(
                    is_version_token(&fresh),
                    "case {}: expected a minted version token in the update's ETag header, got \
                     {fresh:?}",
                    row.case
                );
                assert_ne!(
                    fresh, created,
                    "case {}: expected the update to mint a version of its own, got the create's \
                     {created:?} back",
                    row.case
                );
                fresh
            }
        };
        tenants.push(Tracked {
            case: row.case,
            id,
            current,
        });
    }
    tenants
}

// ---------------------------------------------------------------------------
// Asking every reader
// ---------------------------------------------------------------------------

/// Which reader a version token is read through.
#[derive(Debug, Clone, Copy)]
enum Reader {
    /// `GetDistributionTenant` — the token travels in the `ETag` response header.
    TheTenantsOwnRead,
    /// `ListDistributionTenants` with no `AssociationFilter`.
    AnUnfilteredListing,
    /// `ListDistributionTenants` filtered by the distribution the tenants were created under.
    AListingFilteredByTheDistribution,
    /// `ListDistributionTenants` walked one entry per page, so a summary's token is read off
    /// a page the listing resumed into as well as off the first one.
    AWalkOfOneEntryPages,
}

/// Every reader a case asks, so a token that only one of them reports cannot pass.
const READERS: [Reader; 4] = [
    Reader::TheTenantsOwnRead,
    Reader::AnUnfilteredListing,
    Reader::AListingFilteredByTheDistribution,
    Reader::AWalkOfOneEntryPages,
];

/// The `ETag` every summary of one listing reports, keyed by the tenant's id, and the document
/// it was read out of.
fn listed_versions(answer: &Answer) -> BTreeMap<String, String> {
    summaries(answer)
        .iter()
        .map(|summary| {
            (
                summary.text_at("Id").unwrap_or_default().to_string(),
                summary.text_at("ETag").unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Walk `ListDistributionTenants` at `page_size` entries a page, following the `NextMarker`
/// each page carries, and report the version token every summary reported.
async fn walked_versions(
    router: &MultiServiceRouter,
    page_size: i32,
    case: &str,
) -> BTreeMap<String, String> {
    let mut versions: BTreeMap<String, String> = BTreeMap::new();
    let mut marker: Option<String> = None;
    loop {
        let body = list_body(None, marker.as_deref(), Some(page_size));
        let answer = list_tenants(router, &body).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected HTTP 200 from a {page_size}-entry page of \
             ListDistributionTenants, got {} with {}",
            answer.status, answer.raw
        );
        versions.extend(listed_versions(&answer));
        match answer.root().text_at("NextMarker") {
            Some(next) if !next.is_empty() => marker = Some(next.to_string()),
            _ => return versions,
        }
    }
}

/// The version token `reader` reports for every tenant it reaches, keyed by tenant id.
async fn versions_through(
    router: &MultiServiceRouter,
    reader: Reader,
    distribution_id: &str,
    tenants: &[Tracked],
    case: &str,
) -> BTreeMap<String, String> {
    match reader {
        Reader::TheTenantsOwnRead => {
            let mut versions: BTreeMap<String, String> = BTreeMap::new();
            for tenant in tenants {
                let read = read_tenant(router, &tenant.id, case).await;
                versions.insert(tenant.id.clone(), read.header("etag").to_string());
            }
            versions
        }
        Reader::AnUnfilteredListing => {
            let answer = list_tenants(router, &list_body(None, None, None)).await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected HTTP 200 from ListDistributionTenants, got {} with {}",
                answer.status, answer.raw
            );
            listed_versions(&answer)
        }
        Reader::AListingFilteredByTheDistribution => {
            let body = list_body(Some((Some(distribution_id), None)), None, None);
            let answer = list_tenants(router, &body).await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected HTTP 200 from the filtered ListDistributionTenants, got {} \
                 with {}",
                answer.status, answer.raw
            );
            listed_versions(&answer)
        }
        Reader::AWalkOfOneEntryPages => walked_versions(router, 1, case).await,
    }
}

/// Assert that every reader reports `tenant.current` for every tracked tenant.
async fn assert_every_reader_reports_the_current_version(
    router: &MultiServiceRouter,
    distribution_id: &str,
    tenants: &[Tracked],
    moment: &str,
) {
    for reader in READERS {
        let case = format!("{moment}/{reader:?}");
        let versions = versions_through(router, reader, distribution_id, tenants, &case).await;
        for tenant in tenants {
            assert_eq!(
                versions.get(&tenant.id).map(String::as_str),
                Some(tenant.current.as_str()),
                "case {case} (tenant versioned by {}): expected {} to report the version its \
                 last write minted ({:?}), got {:?}",
                tenant.case,
                tenant.id,
                tenant.current,
                versions.get(&tenant.id)
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

/// Reading a tenant moves nothing: every reader reports the token the last write minted, as
/// often as it is asked, while the tenant is still propagating AND once the clock has owed it
/// its deployment.
#[tokio::test]
async fn no_read_and_no_propagation_moves_the_version_a_write_minted() {
    let router = emulator();
    let distribution_id = multi_tenant_distribution(&router, "ref-tenant-version-clock").await;
    let rows = rows();
    let tenants = tracked_tenants(&router, &distribution_id, &rows, "vclock").await;

    // Straight after the write the tenant is still reaching the edge locations, so the
    // version under test is one a client holds while the resource is in flight.
    for tenant in &tenants {
        let read = read_tenant(&router, &tenant.id, tenant.case).await;
        assert_eq!(
            read.root().text_at("Status"),
            Some(STATUS_IN_PROGRESS),
            "case {}: expected {STATUS_IN_PROGRESS} straight after the write, got {}",
            tenant.case,
            read.raw
        );
    }

    // Asked twice over, so a reader that minted a token per call could not pass.
    assert_every_reader_reports_the_current_version(
        &router,
        &distribution_id,
        &tenants,
        "while propagating",
    )
    .await;
    assert_every_reader_reports_the_current_version(
        &router,
        &distribution_id,
        &tenants,
        "while propagating, asked again",
    )
    .await;

    // The transition is the CLOCK's and the read is only what materialises it: the condition
    // is what is waited for, never a span of the host's clock.
    for tenant in &tenants {
        let read = awaited_tenant_deployment(&router, tenant.case, &tenant.id).await;
        assert_eq!(
            read.root().text_at("Status"),
            Some(STATUS_DEPLOYED),
            "case {}: expected {STATUS_DEPLOYED} once the propagation was owed, got {}",
            tenant.case,
            read.raw
        );
        assert_eq!(
            read.header("etag"),
            tenant.current,
            "case {}: expected the deployed tenant to report the version its last write minted \
             ({:?}), got {:?}",
            tenant.case,
            tenant.current,
            read.header("etag")
        );
    }

    assert_every_reader_reports_the_current_version(
        &router,
        &distribution_id,
        &tenants,
        "once deployed",
    )
    .await;
}

/// The version a listing of a DEPLOYED tenant hands out is still that tenant's precondition:
/// the conditional update it gates is served, the token it supersedes is then refused, and the
/// next listing reports the fresh one.
#[tokio::test]
async fn a_deployed_tenants_listed_version_is_still_its_precondition() {
    let router = emulator();
    let distribution_id = multi_tenant_distribution(&router, "ref-tenant-version-settled").await;
    let rows = rows();
    let tenants = tracked_tenants(&router, &distribution_id, &rows, "vsettled").await;
    for tenant in &tenants {
        awaited_tenant_deployment(&router, tenant.case, &tenant.id).await;
    }

    let answer = list_tenants(&router, &list_body(None, None, None)).await;
    assert_eq!(
        answer.status, 200,
        "case the settled listing: expected HTTP 200 from ListDistributionTenants, got {} with {}",
        answer.status, answer.raw
    );
    let listed = listed_versions(&answer);

    for tenant in &tenants {
        let case = tenant.case;
        let from_listing = listed
            .get(&tenant.id)
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: the listing reports no summary for {}: {}",
                    tenant.id, answer.raw
                )
            })
            .clone();

        let updated = update_tenant(
            &router,
            &tenant.id,
            &update_xml("<Enabled>true</Enabled>"),
            Some(&from_listing),
        )
        .await;
        assert_eq!(
            updated.status, 200,
            "case {case}: expected the update gated on the version the deployed tenant's listing \
             handed out ({from_listing:?}) to be served, got {} with {}",
            updated.status, updated.raw
        );
        let fresh = updated.header("etag").to_string();
        assert!(
            is_version_token(&fresh),
            "case {case}: expected a minted version token in the ETag header, got {fresh:?}"
        );

        // And that token, and only that token, is now the tenant's version: the one the
        // update superseded no longer satisfies the precondition.
        let stale = update_tenant(
            &router,
            &tenant.id,
            &update_xml("<Enabled>false</Enabled>"),
            Some(&from_listing),
        )
        .await;
        assert_eq!(
            stale.status, 412,
            "case {case}: expected HTTP 412 for the superseded version {from_listing:?}, got {} \
             with {}",
            stale.status, stale.raw
        );
        assert_eq!(
            stale.error_code(),
            "PreconditionFailed",
            "case {case}: expected PreconditionFailed, got {:?}",
            stale.error_code()
        );

        let after = list_tenants(&router, &list_body(None, None, None)).await;
        assert_eq!(
            listed_versions(&after).get(&tenant.id).map(String::as_str),
            Some(fresh.as_str()),
            "case {case}: expected the next listing to report the version the update minted \
             ({fresh:?}), got {}",
            after.raw
        );
    }
}
