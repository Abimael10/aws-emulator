// SPDX-License-Identifier: CC0-1.0

//! `ListDistributionTenants` called with NO ARGUMENT AT ALL, in every request-document form a
//! client's own serializer produces for such a call, end to end over HTTP.
//!
//! The rule under test: every input member of this paginated listing is optional
//! (`smithy_query(query="operation",operation="ListDistributionTenants")` reports
//! `AssociationFilter`, `Marker` and `MaxItems` and marks none of them `@required`), so a call
//! that sets no member asks for no narrowing and no resume point and is served the account's
//! whole stored set — HTTP 200 with a `DistributionTenantList`, empty when the account holds no
//! tenant. A client that sets no body member sends no body members: the request can therefore
//! arrive with no document at all, which is as much a request with no narrowing as a document
//! whose root element is empty.
//!
//! [`crate::distribution_tenants_unfiltered_listing`] states the same rule over the filter and
//! the page size; what this suite adds is the WIRE FORM of the argument-less call, which is the
//! one an SDK puts on the wire when the caller passes nothing: a request carrying no body
//! bytes. The rows differ along that form (absent, empty, whitespace, a self-closing root, a
//! root with no child, a root without the service's namespace) and along how many tenants the
//! store holds (none, then one, then two), because a listing that reports the whole set has to
//! report it at every size.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s and the tenants are created through
//! [`crate::distribution_tenant_lifecycle`]'s helpers, so every assertion travels the path a
//! client takes.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, Answer, API_VERSION};
use crate::distribution_tenant_lifecycle::{
    create_distribution_tenant, escape, managed_certificate_request, multi_tenant_distribution,
};
use crate::distribution_tenants_list::{summaries, summary_ids};
use crate::response_headers_policy_lifecycle::{assert_absent, text};

/// Every request-document form an argument-less call can arrive as.
///
/// The first is the one a client's serializer really sends — a request whose body carries no
/// bytes, because no body member was set — and the rest are the forms a hand-built or
/// differently configured client sends for the same call. None of them names a member, so all
/// of them ask for the same listing.
const FORMS: [(&str, Option<&str>); 6] = [
    ("no body at all", None),
    ("an empty body", Some("")),
    ("a body of whitespace", Some("\n  ")),
    (
        "a self-closing request element",
        Some(
            "<ListDistributionTenantsRequest \
             xmlns=\"http://cloudfront.amazonaws.com/doc/2020-05-31/\"/>",
        ),
    ),
    (
        "a request element with no child",
        Some(
            "<ListDistributionTenantsRequest \
             xmlns=\"http://cloudfront.amazonaws.com/doc/2020-05-31/\">\
             </ListDistributionTenantsRequest>",
        ),
    ),
    (
        "a request element without the namespace",
        Some("<ListDistributionTenantsRequest/>"),
    ),
];

/// `POST /2020-05-31/distribution-tenants` carrying `body` — `None` for a request that carries
/// no body bytes at all, which is what a client sends when it sets no body member.
async fn list(router: &MultiServiceRouter, body: Option<&str>) -> Answer {
    call(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution-tenants"),
        body,
        None,
    )
    .await
}

/// Create one tenant named `slug` under `distribution` and answer the id the create minted.
///
/// The create names no connection group, which is the ordinary form: a tenant that names none
/// is routed through the account's default group.
async fn tenant(router: &MultiServiceRouter, slug: &str, distribution: &str) -> String {
    let body = format!(
        "<CreateDistributionTenantRequest \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <DistributionId>{}</DistributionId><Name>{slug}</Name>\
         <Domains><member><Domain>{slug}.example.com</Domain></member></Domains>\
         <Parameters><member><Name>tenantName</Name><Value>{slug}</Value></member></Parameters>\
         {}</CreateDistributionTenantRequest>",
        escape(distribution),
        managed_certificate_request()
    );
    let created = create_distribution_tenant(router, &body).await;
    assert_eq!(
        created.status, 201,
        "case {slug}: expected HTTP 201 from CreateDistributionTenant, got {} with {}",
        created.status, created.raw
    );
    text(created.root(), "Id", slug, "the created tenant", &created.raw)
}

/// Assert that one argument-less call was served the whole stored set.
fn assert_lists(answer: &Answer, case: &str, expected: &[String]) {
    assert_eq!(
        answer.status, 200,
        "case {case}: expected HTTP 200, got {} with {}",
        answer.status, answer.raw
    );
    let got = summary_ids(answer);
    assert_eq!(
        got, expected,
        "case {case}: expected every stored tenant ({expected:?}), got {got:?} with {}",
        answer.raw
    );
    assert!(
        summaries(answer).len() == expected.len(),
        "case {case}: expected {} summaries, got {} with {}",
        expected.len(),
        summaries(answer).len(),
        answer.raw
    );
    // Nothing remains to resume from, so the listing hands out no resume point.
    assert_absent(
        answer.root(),
        "NextMarker",
        case,
        "a listing that reported every stored tenant",
    );
}

/// An argument-less call is served the account's whole stored set, in every form such a call
/// arrives as and at every stored-set size — never a refusal.
#[tokio::test]
async fn a_call_that_sets_no_input_member_lists_every_stored_tenant() {
    let router = emulator();

    // Nothing stored yet: the empty page is the answer, not an error. An account that has
    // created no tenant is exactly the account a client enumerates first.
    let mut expected: Vec<String> = Vec::new();
    for (form, body) in FORMS {
        let answer = list(&router, body).await;
        assert_lists(&answer, &format!("{form} with no tenant stored"), &expected);
    }

    let distribution = multi_tenant_distribution(&router, "ref-default-call-tenants").await;
    for slug in ["default-call-a", "default-call-b"] {
        expected.push(tenant(&router, slug, &distribution).await);
        // The listing reports the tenants in id order, which is the order the walk serves.
        expected.sort();
        for (form, body) in FORMS {
            let answer = list(&router, body).await;
            assert_lists(
                &answer,
                &format!("{form} with {} stored", expected.len()),
                &expected,
            );
        }
    }
}
