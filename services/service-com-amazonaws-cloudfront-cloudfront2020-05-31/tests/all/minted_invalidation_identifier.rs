// SPDX-License-Identifier: CC0-1.0

//! The identifier this service mints for an invalidation request, end to end over HTTP.
//!
//! RULE: the identifier CloudFront mints for an invalidation request is a 26-character token
//! of its uppercase alphanumeric alphabet — a leading `I` followed by 25 more characters — on
//! every surface that creates an invalidation, so it is a token of its OWN width rather than
//! one drawn from the 14-character `E`-prefixed space a distribution id and a version token
//! come from, and a client that validates or stores it never has to tell two identifier spaces
//! of the same width apart. That identifier is what addresses the invalidation afterwards: it
//! is the last segment of the `Location` the create announces, and the value a read of the
//! invalidation answers to.
//!
//! The rows span what the rule generalises over: the surface the batch was submitted on (a
//! distribution's invalidations and a distribution tenant's, the two surfaces that mint one),
//! how many objects the batch names (one object, a wildcard, several objects) and how many
//! invalidations one resource already holds — so the ids several batches are minted in a row
//! are compared against each other and against the token space they must stay out of.
//!
//! The wire harness (the router, the request helpers and the element scanner) is the one
//! [`crate::distribution_lifecycle`] builds; the tenant surface is reached through
//! [`crate::tenant_invalidations`]'s helpers, so nothing here grows a second copy of either.

use std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    batch_xml, config, create_distribution, create_invalidation, emulator, get_invalidation,
    is_distribution_id, is_invalidation_id, is_version_token, Answer, API_VERSION,
};
use crate::tenant_invalidations::{
    batch_paths, create_invalidation_for_tenant, distribution_with_tenant,
    get_invalidation_for_tenant,
};

/// How wide an invalidation id is, prefix included: the width CloudFront's own invalidation
/// ids take, written out here rather than read off the crate's constant so a wrong constant
/// cannot make this suite pass.
const INVALIDATION_ID_LEN: usize = 26;

// ---------------------------------------------------------------------------
// The surfaces that mint an invalidation id
// ---------------------------------------------------------------------------

/// The surfaces an invalidation batch is submitted on: one per resource kind that holds
/// invalidations, which is what the rule is asserted across.
#[derive(Debug, Clone, Copy)]
enum Surface {
    /// `CreateInvalidation` — the batch names objects of a standard distribution.
    Distribution,
    /// `CreateInvalidationForDistributionTenant` — the batch names objects of a tenant.
    Tenant,
}

impl Surface {
    /// The operation this surface creates an invalidation through, for the case labels.
    fn label(self) -> &'static str {
        match self {
            Surface::Distribution => "CreateInvalidation",
            Surface::Tenant => "CreateInvalidationForDistributionTenant",
        }
    }

    /// The collection path the create posts to, from the operation's own `http` binding:
    /// `/2020-05-31/distribution/{DistributionId}/invalidation` and
    /// `/2020-05-31/distribution-tenant/{Id}/invalidation`.
    fn collection_path(self, holder: &str) -> String {
        match self {
            Surface::Distribution => {
                format!("/{API_VERSION}/distribution/{holder}/invalidation")
            }
            Surface::Tenant => {
                format!("/{API_VERSION}/distribution-tenant/{holder}/invalidation")
            }
        }
    }
}

/// The resource the invalidations of one case are created under, reported by its id: a
/// distribution of its own, or a tenant of a multi-tenant distribution of its own.
async fn holder(router: &MultiServiceRouter, surface: Surface, slug: &str) -> String {
    match surface {
        Surface::Distribution => {
            let created = create_distribution(router, &config("minted-id", slug).xml()).await;
            assert_eq!(
                created.status, 201,
                "case {slug}: expected HTTP 201 from CreateDistribution, got {} with {}",
                created.status, created.raw
            );
            created
                .root()
                .text_at("Id")
                .unwrap_or_else(|| panic!("case {slug}: the create carries no Id"))
                .to_string()
        }
        Surface::Tenant => distribution_with_tenant(router, slug).await,
    }
}

/// Submit `batch` on `surface`, against the resource `holder` names.
async fn create(
    router: &MultiServiceRouter,
    surface: Surface,
    holder: &str,
    batch: &str,
) -> Answer {
    match surface {
        Surface::Distribution => create_invalidation(router, holder, batch).await,
        Surface::Tenant => create_invalidation_for_tenant(router, holder, batch).await,
    }
}

/// Read the invalidation `id` of the resource `holder` names, on `surface`.
async fn read(
    router: &MultiServiceRouter,
    surface: Surface,
    holder: &str,
    id: &str,
) -> Answer {
    match surface {
        Surface::Distribution => get_invalidation(router, holder, id).await,
        Surface::Tenant => get_invalidation_for_tenant(router, holder, id).await,
    }
}

// ---------------------------------------------------------------------------
// The batches under test
// ---------------------------------------------------------------------------

/// One batch a case submits: what it is, and the objects it names.
struct Batch {
    case: &'static str,
    paths: &'static [&'static str],
}

/// The batches every surface below is driven with, differing in how many objects they name
/// and whether those are single objects or wildcards.
const BATCHES: &[Batch] = &[
    Batch {
        case: "one object",
        paths: &["/index.html"],
    },
    Batch {
        case: "every object",
        paths: &["/*"],
    },
    Batch {
        case: "several objects",
        paths: &["/a.html", "/assets/b.css", "/images/*"],
    },
];

// ---------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------

/// Every invalidation id this service mints is a 26-character `I`-prefixed token of the
/// uppercase alphanumeric alphabet, distinct from every other id it has minted, and is the
/// identifier the invalidation is addressed by afterwards — on both surfaces that create one.
#[tokio::test]
async fn an_invalidation_id_is_minted_in_the_form_cloudfront_identifies_one_by() {
    let router = emulator();
    // Every id this suite saw, across both surfaces: one mint serves them all, so no two
    // invalidations may carry the same identifier however they were created.
    let mut minted: BTreeSet<String> = BTreeSet::new();

    for surface in [Surface::Distribution, Surface::Tenant] {
        let label = surface.label();
        let holder_id = holder(&router, surface, "minted-invalidation-id").await;
        for batch in BATCHES {
            let case = format!("{label}/{}", batch.case);
            let reference = format!("minted-{}", batch.case.replace(' ', "-"));
            let created = create(
                &router,
                surface,
                &holder_id,
                &batch_xml(&reference, batch.paths),
            )
            .await;
            assert_eq!(
                created.status, 201,
                "case {case}: expected HTTP 201 from the create, got {} with {}",
                created.status, created.raw
            );
            let id = created
                .root()
                .text_at("Id")
                .unwrap_or_else(|| panic!("case {case}: the create carries no Id: {}", created.raw))
                .to_string();

            // The form: a leading `I` and 25 more characters of the uppercase alphanumeric
            // alphabet, every one of them carrying mint state rather than padding.
            assert!(
                is_invalidation_id(&id),
                "case {case}: expected an `I`+25 uppercase alphanumeric invalidation id, got \
                 {id:?}"
            );
            assert_eq!(
                id.chars().count(),
                INVALIDATION_ID_LEN,
                "case {case}: expected a {INVALIDATION_ID_LEN}-character invalidation id, got \
                 {id:?}"
            );
            // And so NOT a token of the distribution-id/version-token space, which a client
            // that validates either would otherwise accept this id as.
            assert!(
                !is_distribution_id(&id) && !is_version_token(&id),
                "case {case}: an invalidation id must not be a token of the distribution-id \
                 space, got {id:?}"
            );
            assert!(
                minted.insert(id.clone()),
                "case {case}: the mint repeated the invalidation id {id:?}"
            );

            // "The location for the invalidation": the identifier is the last segment of the
            // announced URI, under the collection the batch was submitted to.
            let location = created.header("location");
            let path = format!("{}/{id}", surface.collection_path(&holder_id));
            assert!(
                location.ends_with(&path),
                "case {case}: expected the Location to address the new invalidation at {path}, \
                 got {location:?}"
            );

            // And the identifier is what the invalidation ANSWERS to: the read reports the
            // same id and the batch that was submitted under it.
            let stored = read(&router, surface, &holder_id, &id).await;
            assert_eq!(
                stored.status, 200,
                "case {case}: expected HTTP 200 reading {id}, got {} with {}",
                stored.status, stored.raw
            );
            assert_eq!(
                stored.root().text_at("Id"),
                Some(id.as_str()),
                "case {case}: expected the read to report {id}, got {}",
                stored.raw
            );
            assert_eq!(
                batch_paths(stored.root()),
                batch
                    .paths
                    .iter()
                    .map(|path| (*path).to_string())
                    .collect::<Vec<String>>(),
                "case {case}: expected the read to report the submitted objects, got {}",
                stored.raw
            );
        }
    }

    assert_eq!(
        minted.len(),
        2 * BATCHES.len(),
        "case every surface: expected one minted id per batch, got {minted:?}"
    );
}
