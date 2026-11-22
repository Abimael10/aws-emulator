// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ListDomains`.
//!
//! The operation's own doc states the rule: "Returns the caller's domains: the
//! account-scoped domain and the organization-scoped domain, if either exists. At most
//! two domains are returned." It is `@readonly` and `@paginated`, so the suite asserts
//! over a TABLE rather than one recorded exchange:
//!
//! * an account holding no domain gets HTTP 200 with `items` present and EMPTY, and no
//!   `nextToken` — not a failure;
//! * an account holding its domain gets exactly that domain, projected to
//!   `DomainSummary`: every member read from the record `GetDomain` answers with, and
//!   `identityCenterInstanceArn` present only for a domain configured with an Identity
//!   Center instance ("Absent for IAM-only domains");
//! * the listing reports no `nextToken` while nothing remains beyond the page, at every
//!   published bound of `maxResults`'s `@range(1..100)`;
//! * a `nextToken` this service did not mint for a domain listing is the modelled
//!   `ValidationException` (400) rather than an empty page — a token minted for another
//!   collection included;
//! * reading the listing changes nothing: two reads in a row report the same domain, and
//!   a deleted domain is gone from it.
//!
//! Every call goes through a real operation handler holding the SAME shared engine the
//! router hands every service, and each assertion names its case and the actual value.

use std::sync::Arc;

use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError, Service,
};

use service_com_amazonaws_cloudwatchomni_cloudwatchomnifrontend as omni;

use omni::ops::create_domain::CreateDomain;
use omni::ops::create_space::CreateSpace;
use omni::ops::delete_domain::DeleteDomain;
use omni::ops::get_domain::GetDomain;
use omni::ops::list_domains::{ListDomains, ListDomainsError};
use omni::ops::list_spaces::ListSpaces;
use omni::ops::types::{
    CreateDomainInput, CreateSpaceInput, DeleteDomainInput, Domain, GetDomainInput,
    IdentityCenterConfiguration, IdentityProviderConfiguration, ListDomainsInput,
    ListDomainsOutput, ListSpacesInput,
};

/// The Regions a space create is signed for, from the suite that owns the space rule: an
/// account holds at most one space per Region.
use super::domain_space_lifecycle::SPACE_REGIONS;

/// A role ARN satisfying `Arn`'s `^arn:.+$`, which `CreateSpace` requires.
const DATA_ACCESS_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/OmniData";

/// An Identity Center instance ARN satisfying `IdentityCenterInstanceArn`'s modelled
/// `^arn:aws[a-z-]*:sso:::instance/[a-zA-Z0-9-]+$`.
const IDC_INSTANCE_ARN: &str = "arn:aws:sso:::instance/ssoins-1234567890abcdef";

/// The request metadata a call signed for `region` carries.
fn signed_request_parts_in(region: &str) -> http::request::Parts {
    let (parts, ()) = http::Request::builder()
        .method("POST")
        .uri("/service/CloudWatchOmniFrontend/operation/Unused")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{region}/cloudwatch/aws4_request, \
                 SignedHeaders=host, Signature=emulator"
            ),
        )
        .body(())
        .expect("a well-formed request")
        .into_parts();
    parts
}

/// One emulator: one shared engine and the service mounted on a router.
struct Omni {
    engine: Arc<engine::InMemoryEngine>,
    service: omni::EmulatedService<engine::InMemoryEngine>,
    router: MultiServiceRouter,
    parts: http::request::Parts,
    /// How many spaces this emulator has created, which picks each create's Region out of
    /// [`SPACE_REGIONS`].
    spaces_created: std::sync::atomic::AtomicUsize,
}

impl Omni {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(omni::EmulatedService::new(Arc::clone(&engine)));
        Self {
            service: omni::EmulatedService::new(Arc::clone(&engine)),
            engine,
            router,
            parts: signed_request_parts_in(SPACE_REGIONS[0]),
            spaces_created: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn ctx(&self) -> OperationContext<'_> {
        OperationContext::new(
            &self.parts,
            self.service.definition().identity(),
            EmbeddedEmulator::from_router(self.router.clone()),
        )
    }

    /// The account's domain, with the identity providers the row asks for.
    async fn create_domain(
        &self,
        case: &str,
        name: &str,
        providers: &[&str],
        idc_instance: Option<&str>,
    ) -> Domain {
        CreateDomain::new(Arc::clone(&self.engine))
            .handle(
                CreateDomainInput {
                    name: name.to_string(),
                    identity_providers: providers.iter().map(|p| (*p).to_string()).collect(),
                    client_token: Some(format!("token-{name}")),
                    identity_provider_configuration: idc_instance.map(|arn| {
                        IdentityProviderConfiguration {
                            identity_center_configuration: Some(IdentityCenterConfiguration {
                                identity_center_instance_arn: Some(arn.to_string()),
                            }),
                        }
                    }),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateDomain failed: {error:?}"))
            .domain
    }

    /// The domain `domain_id` names, through the reader a caller resolves it with.
    async fn read_domain(&self, case: &str, domain_id: &str) -> Domain {
        GetDomain::new(Arc::clone(&self.engine))
            .handle(
                GetDomainInput {
                    domain_id: domain_id.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: GetDomain failed: {error:?}"))
            .domain
    }

    async fn delete_domain(&self, case: &str, domain_id: &str) {
        DeleteDomain::new(Arc::clone(&self.engine))
            .handle(
                DeleteDomainInput {
                    domain_id: domain_id.to_string(),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: DeleteDomain failed: {error:?}"));
    }

    async fn list_domains(
        &self,
        max_results: Option<i32>,
        next_token: Option<String>,
    ) -> Result<ListDomainsOutput, ListDomainsError> {
        ListDomains::new(Arc::clone(&self.engine))
            .handle(
                ListDomainsInput {
                    max_results,
                    next_token,
                },
                &self.ctx(),
            )
            .await
    }

    /// A space of `domain_id`, created in a Region of its own.
    async fn space(&self, case: &str, domain_id: &str, name: &str) {
        let index = self
            .spaces_created
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let region = SPACE_REGIONS
            .get(index)
            .copied()
            .expect("the suite signs each space it creates for a Region of its own");
        let parts = signed_request_parts_in(region);
        let ctx = OperationContext::new(
            &parts,
            self.service.definition().identity(),
            EmbeddedEmulator::from_router(self.router.clone()),
        );
        CreateSpace::new(Arc::clone(&self.engine))
            .handle(
                CreateSpaceInput {
                    name: name.to_string(),
                    domain_id: domain_id.to_string(),
                    data_access_role_arn: DATA_ACCESS_ROLE_ARN.to_string(),
                    ..Default::default()
                },
                &ctx,
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateSpace failed: {error:?}"));
    }

    /// A `nextToken` this service minted for a SPACE listing, which is not a domain
    /// listing's resume point.
    async fn space_page_token(&self, case: &str, domain_id: &str) -> String {
        ListSpaces::new(Arc::clone(&self.engine))
            .handle(
                ListSpacesInput {
                    domain_id: Some(domain_id.to_string()),
                    max_results: Some(1),
                    next_token: None,
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: ListSpaces failed: {error:?}"))
            .next_token
            .unwrap_or_else(|| panic!("case {case:?}: a bounded page of two spaces hands a token"))
    }
}

/// Require a modelled failure with the status and error code the model declares for it.
fn expect_modelled_error<O: std::fmt::Debug, E: OperationError + std::fmt::Debug>(
    case: &str,
    result: Result<O, E>,
    expected_status: u16,
    expected_code: &str,
) {
    match result {
        Ok(output) => panic!(
            "case {case:?}: expected {expected_code} ({expected_status}), got Ok({output:?})"
        ),
        Err(error) => {
            assert_eq!(
                (error.shape_name(), error.http_status()),
                (expected_code, expected_status),
                "case {case:?}: expected {expected_code} ({expected_status}), got {error:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_listing_reports_the_account_s_domain_as_the_record_holds_it() {
    // Rows differ along the identity-provider set (`IdentityProvider` is exactly
    // IAM | IDC, `@length(1..2)` `@uniqueItems`) and along whether an Identity Center
    // instance is configured, because `identityCenterInstanceArn` is "Absent for
    // IAM-only domains". Each row runs against an account of its own: an account holds
    // at most one domain.
    for (case, name, providers, idc_instance) in [
        ("an IAM-only domain", "omni-listed-iam", vec!["IAM"], None),
        (
            "an IDC domain with no instance configured",
            "omni-listed-idc",
            vec!["IDC"],
            None,
        ),
        (
            "a domain with both providers and an Identity Center instance",
            "omni-listed-both",
            vec!["IAM", "IDC"],
            Some(IDC_INSTANCE_ARN),
        ),
    ] {
        let omni = Omni::new();

        // An account holding no domain lists nothing and hands back no token.
        let empty = omni
            .list_domains(None, None)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: ListDomains failed: {error:?}"));
        assert!(
            empty.items.is_empty() && empty.next_token.is_none(),
            "case {case:?}: an account with no domain must list nothing, got {empty:?}"
        );

        let created = omni.create_domain(case, name, &providers, idc_instance).await;
        let stored = omni.read_domain(case, &created.domain_id).await;

        let page = omni
            .list_domains(None, None)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: ListDomains failed: {error:?}"));
        assert_eq!(
            page.items.len(),
            1,
            "case {case:?}: the account holds one domain, got {:?}",
            page.items
        );
        assert!(
            page.next_token.is_none(),
            "case {case:?}: nothing remains beyond the page, got token {:?}",
            page.next_token
        );
        let summary = &page.items[0];
        assert_eq!(
            (
                summary.domain_id.as_str(),
                summary.domain_arn.as_deref(),
                summary.name.as_deref(),
                summary.region.as_deref(),
                summary.status.as_str(),
                summary.created_at,
                summary.updated_at,
            ),
            (
                stored.domain_id.as_str(),
                Some(stored.domain_arn.as_str()),
                stored.name.as_deref(),
                Some(stored.region.as_str()),
                stored.status.as_str(),
                stored.created_at,
                stored.updated_at,
            ),
            "case {case:?}: the summary is not the stored domain: {summary:?} against \
             {stored:?}"
        );
        assert_eq!(
            summary.identity_center_instance_arn.as_deref(),
            idc_instance,
            "case {case:?}: identityCenterInstanceArn must be the configured instance, got \
             {:?}",
            summary.identity_center_instance_arn
        );

        // The listing reports, it does not change: a second read answers the same page.
        let again = omni
            .list_domains(None, None)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: ListDomains failed: {error:?}"));
        assert_eq!(
            format!("{:?}", again.items),
            format!("{:?}", page.items),
            "case {case:?}: a second listing answered {:?}",
            again.items
        );
    }
}

#[tokio::test]
async fn the_listing_honours_every_published_bound_of_max_results() {
    let omni = Omni::new();
    let domain = omni
        .create_domain("bounds", "omni-listed-bounds", &["IAM"], None)
        .await;

    // Rows span `maxResults`'s `@range(1..100)` at both published bounds and its
    // documented default (absent). The account holds one domain, so every bound reports
    // it in one page with no resume token.
    for max_results in [None, Some(1), Some(100), Some(50)] {
        let case = format!("maxResults={max_results:?}");
        let page = omni
            .list_domains(max_results, None)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: ListDomains failed: {error:?}"));
        assert_eq!(
            (
                page.items
                    .iter()
                    .map(|item| item.domain_id.as_str())
                    .collect::<Vec<&str>>(),
                page.next_token.clone()
            ),
            (vec![domain.domain_id.as_str()], None),
            "case {case:?}: expected the account's one domain in one page, got {page:?}"
        );
    }
}

#[tokio::test]
async fn a_token_this_listing_did_not_mint_is_the_modelled_validation_failure() {
    let omni = Omni::new();
    let domain = omni
        .create_domain("tokens", "omni-listed-tokens", &["IAM"], None)
        .await;
    // Two spaces, each in a Region of its own, so a bounded space listing mints a token
    // of ANOTHER collection to offer this one.
    omni.space("tokens", &domain.domain_id, "listed-space-one")
        .await;
    omni.space("tokens", &domain.domain_id, "listed-space-two")
        .await;
    let space_token = omni.space_page_token("tokens", &domain.domain_id).await;

    // Rows span what a token that is not this listing's resume point can be: not hex at
    // all, hex that decodes to other text, and a token this service minted for the space
    // listing.
    for (case, token) in [
        ("not hex", "not-a-token".to_string()),
        ("hex that decodes to other text", "6e6f70652d31".to_string()),
        ("a token minted for the space listing", space_token),
    ] {
        expect_modelled_error(
            case,
            omni.list_domains(None, Some(token)).await,
            400,
            "ValidationException",
        );
    }

    // The refusals left the listing as it was.
    let page = omni
        .list_domains(None, None)
        .await
        .expect("the listing still answers");
    assert_eq!(
        page.items
            .iter()
            .map(|item| item.domain_id.as_str())
            .collect::<Vec<&str>>(),
        vec![domain.domain_id.as_str()],
        "the refused tokens changed the listing: {page:?}"
    );
}

#[tokio::test]
async fn a_deleted_domain_is_gone_from_the_listing() {
    let omni = Omni::new();
    let domain = omni
        .create_domain("deleted", "omni-listed-deleted", &["IAM"], None)
        .await;
    omni.delete_domain("deleted", &domain.domain_id).await;

    let page = omni
        .list_domains(None, None)
        .await
        .expect("the listing answers for an account with no domain");
    assert!(
        page.items.is_empty() && page.next_token.is_none(),
        "a deleted domain must be gone from the listing, got {page:?}"
    );

    // The account's domain slot is free again, and the new domain is what the listing
    // reports.
    let recreated = omni
        .create_domain("deleted", "omni-listed-again", &["IAM"], None)
        .await;
    let page = omni
        .list_domains(None, None)
        .await
        .expect("the listing answers after the recreate");
    assert_eq!(
        page.items
            .iter()
            .map(|item| item.domain_id.as_str())
            .collect::<Vec<&str>>(),
        vec![recreated.domain_id.as_str()],
        "the listing must report the domain the account holds now, got {page:?}"
    );
}
