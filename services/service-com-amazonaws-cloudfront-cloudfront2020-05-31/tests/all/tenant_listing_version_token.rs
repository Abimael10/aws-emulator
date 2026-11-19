// SPDX-License-Identifier: CC0-1.0

//! The version token a LISTING reports for a distribution tenant, end to end over HTTP.
//!
//! RULE: `DistributionTenantSummary$ETag` is "the current version of the distribution tenant"
//! — the very token the tenant's own create and read report in their `ETag` header, and the
//! only token an `If-Match` against that tenant is satisfied by — so a listing never mints a
//! version of its own, and a client that takes the token out of a summary can modify or delete
//! the tenant it summarises with it. A summary carrying anything else makes list-then-modify
//! impossible: the write is refused `PreconditionFailed` even though nothing changed the
//! tenant.
//!
//! The rows span what the rule generalises over: how the tenant was last versioned (by its
//! create alone, by an `UpdateDistributionTenant` and by an
//! `AssociateDistributionTenantWebACL`, the calls that mint a tenant a fresh version), the
//! customizations it was created with, and several tenants listed together, so each summary is
//! matched against its OWN tenant rather than against whichever token the account minted last.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s, the listing
//! [`crate::distribution_tenants_list`]'s, and the mutations are reached through the suites
//! that own them, so nothing here grows a second copy of either.

use std::collections::BTreeMap;

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, is_version_token, Answer, API_VERSION};
use crate::distribution_tenant_lifecycle::multi_tenant_distribution;
use crate::distribution_tenant_update::{update_tenant, update_xml};
use crate::distribution_tenants_list::{list_body, list_tenants, summaries};
use crate::tenant_web_acl_association::{associate, create_tenant, Start, STARTS};

// ---------------------------------------------------------------------------
// The tenants under test
// ---------------------------------------------------------------------------

/// What last versioned a tenant before its summary is read.
#[derive(Debug, Clone, Copy)]
enum Versioned {
    /// Nothing since the create: the token the create handed back is still current.
    TheCreate,
    /// An `UpdateDistributionTenant`, which mints a fresh version.
    AnUpdate,
    /// An `AssociateDistributionTenantWebACL`, which versions the tenant the same way.
    AWebAclAssociation,
}

/// One tenant a case holds: how it was created, and what last versioned it.
struct Row {
    case: &'static str,
    /// The starting state the create puts the tenant in, from the shared table.
    start: Start,
    versioned: Versioned,
}

/// The rows every assertion below runs over: each starting state the shared table spans,
/// crossed with the three ways a tenant comes to carry the version a listing has to report.
fn rows() -> Vec<Row> {
    let cases: [(&'static str, Versioned); 3] = [
        ("the create alone", Versioned::TheCreate),
        ("an update", Versioned::AnUpdate),
        ("a web ACL association", Versioned::AWebAclAssociation),
    ];
    cases
        .iter()
        .enumerate()
        .map(|(index, (case, versioned))| Row {
            case,
            // A starting state of its own per row, so the summaries read together differ in
            // the customizations they carry as well as in what versioned them.
            start: STARTS[index % STARTS.len()],
            versioned: *versioned,
        })
        .collect()
}

/// One tenant as this suite tracks it: its id and the version token that is current for it.
struct Tracked {
    case: &'static str,
    id: String,
    current: String,
}

/// Apply the mutation `versioned` names to the tenant `id`, gated on `current`, and report
/// the version token that is current afterwards.
async fn apply(
    router: &MultiServiceRouter,
    versioned: Versioned,
    id: &str,
    current: &str,
    case: &str,
) -> String {
    let answer = match versioned {
        Versioned::TheCreate => return current.to_string(),
        Versioned::AnUpdate => {
            update_tenant(
                router,
                id,
                &update_xml("<Enabled>false</Enabled>"),
                Some(current),
            )
            .await
        }
        Versioned::AWebAclAssociation => {
            // The association resolves the web ACL, so the one this row associates is created
            // in WAF first, under a name of this tenant's own so no two rows collide.
            let web_acl = crate::waf_control_plane::create_web_acl(
                router,
                &format!("version-token-{id}"),
                crate::waf_control_plane::SCOPE_CLOUDFRONT,
                "{\"Allow\":{}}",
                "",
            )
            .await
            .arn;
            associate(router, id, &web_acl, Some(current)).await
        }
    };
    assert_eq!(
        answer.status, 200,
        "case {case}: expected the mutation gated on the tenant's own version to be served, \
         got {} with {}",
        answer.status, answer.raw
    );
    let fresh = answer.header("etag").to_string();
    assert!(
        is_version_token(&fresh),
        "case {case}: expected a minted version token in the ETag header, got {fresh:?}"
    );
    fresh
}

/// Create one tenant per row under `distribution_id`, apply what each row says versioned it,
/// and report them in row order.
async fn tracked_tenants(
    router: &MultiServiceRouter,
    distribution_id: &str,
    rows: &[Row],
) -> Vec<Tracked> {
    let mut tenants: Vec<Tracked> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let slug = format!("version-token-{index}");
        let (id, created_etag) =
            create_tenant(router, distribution_id, &slug, row.case, row.start).await;
        assert!(
            is_version_token(&created_etag),
            "case {}: expected a minted version token in the create's ETag header, got {:?}",
            row.case,
            created_etag
        );
        let current = apply(router, row.versioned, &id, &created_etag, row.case).await;
        tenants.push(Tracked {
            case: row.case,
            id,
            current,
        });
    }
    tenants
}

// ---------------------------------------------------------------------------
// Reading the listing
// ---------------------------------------------------------------------------

/// The `ETag` every summary of an unfiltered listing reports, keyed by the tenant's id.
async fn listed_versions(router: &MultiServiceRouter) -> (BTreeMap<String, String>, String) {
    let answer = list_tenants(router, &list_body(None, None, None)).await;
    assert_eq!(
        answer.status, 200,
        "case the listing: expected HTTP 200 from ListDistributionTenants, got {} with {}",
        answer.status, answer.raw
    );
    let mut versions: BTreeMap<String, String> = BTreeMap::new();
    for summary in summaries(&answer) {
        let id = summary.text_at("Id").unwrap_or_default().to_string();
        let etag = summary.text_at("ETag").unwrap_or_default().to_string();
        versions.insert(id, etag);
    }
    (versions, answer.raw.clone())
}

/// `GET /2020-05-31/distribution-tenant/{Id}`.
async fn get_tenant(router: &MultiServiceRouter, id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/{API_VERSION}/distribution-tenant/{id}"),
        None,
        None,
    )
    .await
}

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

/// Every summary reports the version its own tenant's read reports, whatever last versioned
/// that tenant.
#[tokio::test]
async fn a_summary_reports_the_version_the_tenants_own_read_reports() {
    let router = emulator();
    let distribution_id = multi_tenant_distribution(&router, "ref-tenant-version-token").await;
    let rows = rows();
    let tenants = tracked_tenants(&router, &distribution_id, &rows).await;

    let (listed, raw) = listed_versions(&router).await;
    for tenant in &tenants {
        let case = tenant.case;
        let summary_etag = listed.get(&tenant.id).unwrap_or_else(|| {
            panic!(
                "case {case}: the listing reports no summary for {}: {raw}",
                tenant.id
            )
        });
        assert_eq!(
            summary_etag, &tenant.current,
            "case {case}: expected the summary of {} to report the tenant's current version \
             {:?}, got {summary_etag:?}",
            tenant.id, tenant.current
        );
        let read = get_tenant(&router, &tenant.id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected HTTP 200 from GetDistributionTenant, got {} with {}",
            read.status, read.raw
        );
        assert_eq!(
            read.header("etag"),
            summary_etag.as_str(),
            "case {case}: the summary and the read disagree about the tenant's version: \
             {summary_etag:?} against {:?}",
            read.header("etag")
        );
    }
}

/// A version token taken out of a summary is the tenant's own precondition: a mutation gated
/// on it is served, the token it supersedes is then refused, and the next listing reports the
/// fresh one.
#[tokio::test]
async fn a_version_token_read_from_a_summary_satisfies_the_tenants_if_match() {
    let router = emulator();
    let distribution_id = multi_tenant_distribution(&router, "ref-list-then-modify").await;
    let rows = rows();
    let tenants = tracked_tenants(&router, &distribution_id, &rows).await;

    let (listed, raw) = listed_versions(&router).await;
    for tenant in &tenants {
        let case = tenant.case;
        let from_listing = listed
            .get(&tenant.id)
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: the listing reports no summary for {}: {raw}",
                    tenant.id
                )
            })
            .clone();

        // The token the listing handed out is the one the tenant's `If-Match` accepts.
        let updated = update_tenant(
            &router,
            &tenant.id,
            &update_xml("<Enabled>true</Enabled>"),
            Some(&from_listing),
        )
        .await;
        assert_eq!(
            updated.status, 200,
            "case {case}: expected the update gated on the listed version {from_listing:?} to \
             be served, got {} with {}",
            updated.status, updated.raw
        );
        let fresh = updated.header("etag").to_string();
        assert!(
            is_version_token(&fresh),
            "case {case}: expected a minted version token in the ETag header, got {fresh:?}"
        );

        // And it is spent: the version the update superseded no longer satisfies the
        // precondition.
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
    }

    // The next listing reports what each tenant's read now reports, so the walk can be
    // repeated against the state the mutations left.
    let (after, raw_after) = listed_versions(&router).await;
    for tenant in &tenants {
        let case = tenant.case;
        let read = get_tenant(&router, &tenant.id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected HTTP 200 from GetDistributionTenant, got {} with {}",
            read.status, read.raw
        );
        assert_eq!(
            after.get(&tenant.id).map(String::as_str),
            Some(read.header("etag")),
            "case {case}: expected the listing to report the version {} now carries ({:?}), got \
             {raw_after}",
            tenant.id,
            read.header("etag")
        );
    }
}
