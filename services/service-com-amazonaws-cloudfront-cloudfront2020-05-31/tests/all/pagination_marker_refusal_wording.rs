// SPDX-License-Identifier: CC0-1.0

//! The sentence this service refuses a listing's `Marker` with when the value is no resume
//! point that listing hands out, end to end over HTTP.
//!
//! The rule: what such a refusal states is the CONSTRAINT on the parameter — which kind of
//! identifier a marker of that listing has to be — as FIXED text under `InvalidArgument`
//! (HTTP 400). The constraint is a property of the parameter rather than of the value
//! submitted, so the sentence carries nothing read off the request (the marker itself) and
//! nothing read off the store (an identifier of the resource the listing is scoped to): two
//! runs of the same mistake are refused in the same words, and a caller matching the sentence
//! recognises the refusal.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the RESOURCE the listing enumerates, which is what the stated constraint names — a
//!   distribution, an invalidation, an origin access control, an origin request policy, a
//!   continuous deployment policy, a distribution tenant;
//! * whether the listing is SCOPED to a parent resource the request names (the invalidations
//!   of one distribution, the distributions attached to one web ACL) or enumerates a whole
//!   collection — a scoped listing holds an identifier the refusal could have leaked;
//! * the BINDING the marker travelled in: a query parameter of a collection route, a query
//!   parameter beside a path label, and a member of a request document;
//! * the FORM of the unusable marker (another word, a hyphenated name, a bare number, a long
//!   run of one character), which the rule never reports back.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call, config, create_distribution, emulator, Answer, API_VERSION,
};

/// The code every one of these refusals carries: "an argument is invalid".
const INVALID_ARGUMENT: &str = "InvalidArgument";

/// The forms of a value that is no resume point of ANY of the listings below — each is
/// ill-formed as an identifier of every resource kind under test, so the row that follows is
/// about the sentence rather than about which resource the value could have named.
const UNUSABLE_MARKERS: [(&str, &str); 4] = [
    ("another word", "not-an-id"),
    ("a hyphenated name", "tenant-distribution"),
    ("a bare number", "12345"),
    (
        "a long run of one character",
        "ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ",
    ),
];

/// Which listing a row drives, and what it needs to reach its marker check.
enum Listing {
    /// `ListDistributions`: a query parameter of a collection route.
    Distributions,
    /// `ListDistributionsByWebACLId`: a query parameter beside a path label, scoped to the
    /// web ACL the path names.
    DistributionsByWebAclId,
    /// `ListInvalidations`: a query parameter beside the id of the distribution the
    /// invalidations belong to, which the store holds.
    Invalidations(String),
    /// `ListOriginAccessControls`: a query parameter of a collection route.
    OriginAccessControls,
    /// `ListOriginRequestPolicies`: a query parameter of a collection route.
    OriginRequestPolicies,
    /// `ListContinuousDeploymentPolicies`: a query parameter of a collection route.
    ContinuousDeploymentPolicies,
    /// `ListDistributionTenants`: a member of the request DOCUMENT.
    DistributionTenants,
}

impl Listing {
    /// Ask this listing to resume at `marker`.
    async fn resume_at(&self, router: &MultiServiceRouter, marker: &str) -> Answer {
        match self {
            Self::Distributions => {
                get(router, &format!("/{API_VERSION}/distribution?Marker={marker}")).await
            }
            Self::DistributionsByWebAclId => {
                get(
                    router,
                    &format!("/{API_VERSION}/distributionsByWebACLId/{WEB_ACL_ID}?Marker={marker}"),
                )
                .await
            }
            Self::Invalidations(distribution_id) => {
                get(
                    router,
                    &format!(
                        "/{API_VERSION}/distribution/{distribution_id}/invalidation?Marker={marker}"
                    ),
                )
                .await
            }
            Self::OriginAccessControls => {
                get(
                    router,
                    &format!("/{API_VERSION}/origin-access-control?Marker={marker}"),
                )
                .await
            }
            Self::OriginRequestPolicies => {
                get(
                    router,
                    &format!("/{API_VERSION}/origin-request-policy?Marker={marker}"),
                )
                .await
            }
            Self::ContinuousDeploymentPolicies => {
                get(
                    router,
                    &format!("/{API_VERSION}/continuous-deployment-policy?Marker={marker}"),
                )
                .await
            }
            Self::DistributionTenants => {
                let body = format!(
                    "<ListDistributionTenantsRequest \
                     xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
                     <Marker>{marker}</Marker></ListDistributionTenantsRequest>"
                );
                call(
                    router,
                    "POST",
                    &format!("/{API_VERSION}/distribution-tenants"),
                    Some(&body),
                    None,
                )
                .await
            }
        }
    }

    /// The identifier of the resource this listing is SCOPED to, which the refusal must not
    /// report back, or `None` for a listing that enumerates a whole collection.
    fn scope(&self) -> Option<&str> {
        match self {
            Self::DistributionsByWebAclId => Some(WEB_ACL_ID),
            Self::Invalidations(distribution_id) => Some(distribution_id.as_str()),
            _ => None,
        }
    }
}

/// The web ACL a scoped distribution listing filters by: any identifier, since what the row
/// asserts is that it stays out of the refusal.
const WEB_ACL_ID: &str = "0f4d8c1a-8e3e-4f1b-9a3c-1b2c3d4e5f60";

/// `GET uri`, the one request path every query-bound row goes down.
async fn get(router: &MultiServiceRouter, uri: &str) -> Answer {
    call(router, "GET", uri, None, None).await
}

/// One row: a listing, and the sentence its invalid-`Marker` refusal states.
///
/// The sentences are written out as LITERALS rather than read back off the crate's constants,
/// so a change to one of those constants fails this suite.
fn rows(distribution_id: String) -> Vec<(&'static str, Listing, &'static str)> {
    vec![
        (
            "a distribution listing",
            Listing::Distributions,
            "Marker must be a valid Distribution Id",
        ),
        (
            "a distribution listing scoped to a web ACL",
            Listing::DistributionsByWebAclId,
            "Marker must be a valid Distribution Id",
        ),
        (
            "the invalidations of one distribution",
            Listing::Invalidations(distribution_id),
            "Marker must be a valid Invalidation Id",
        ),
        (
            "an origin access control listing",
            Listing::OriginAccessControls,
            "Marker must be a valid CF origin access control Id",
        ),
        (
            "an origin request policy listing",
            Listing::OriginRequestPolicies,
            "Marker must be a valid CF origin request policy Id",
        ),
        (
            "a continuous deployment policy listing",
            Listing::ContinuousDeploymentPolicies,
            "Marker must be a valid CF continuous deployment policy Id",
        ),
        (
            "a distribution tenant listing, the marker in a document",
            Listing::DistributionTenants,
            "Marker must be a valid CF distribution tenant Id",
        ),
    ]
}

/// One distribution, created through the API, so the invalidation listing reaches its marker
/// check with a distribution that really exists and an id only the store knows.
async fn one_distribution(router: &MultiServiceRouter) -> String {
    let row = config("marker-refusal-wording", "marker-refusal-wording");
    let created = create_distribution(router, &row.xml()).await;
    assert_eq!(
        created.status, 201,
        "creating the distribution under test: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );
    created
        .root()
        .text_at("Id")
        .unwrap_or_default()
        .to_string()
}

/// A `Marker` that is no resume point is refused 400 `InvalidArgument` under the one sentence
/// stating the constraint on the parameter — the same sentence whatever the submitted value
/// was, in every listing and in whatever binding the marker travelled.
#[tokio::test]
async fn an_unusable_marker_is_refused_with_the_constraint_on_the_parameter() {
    let router = emulator();
    let distribution_id = one_distribution(&router).await;

    for (case, listing, expected) in rows(distribution_id) {
        for (form, marker) in UNUSABLE_MARKERS {
            let label = format!("{case} / {form}");
            let answer = listing.resume_at(&router, marker).await;
            assert_eq!(
                answer.status, 400,
                "case {label:?} ({marker:?}): expected HTTP 400, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                INVALID_ARGUMENT,
                "case {label:?} ({marker:?}): expected {INVALID_ARGUMENT}, got {:?} in {}",
                answer.error_code(),
                answer.raw
            );
            let actual = answer
                .root()
                .find_text("Message")
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                actual, expected,
                "case {label:?} ({marker:?}): expected the sentence {expected:?}, got {actual:?}"
            );
        }
    }
}

/// The refusal carries NOTHING the request or the store supplied: neither the marker
/// submitted nor the identifier of the resource the listing is scoped to — so two runs of the
/// same mistake, over stores holding different resources, are refused in the same words.
#[tokio::test]
async fn an_unusable_marker_refusal_reports_no_value_of_the_request_or_the_store() {
    let router = emulator();
    let distribution_id = one_distribution(&router).await;

    for (case, listing, _) in rows(distribution_id) {
        let scope = listing.scope().map(str::to_string);
        let mut said: Vec<String> = Vec::new();
        for (form, marker) in UNUSABLE_MARKERS {
            let label = format!("{case} / {form}");
            let answer = listing.resume_at(&router, marker).await;
            let message = answer
                .root()
                .find_text("Message")
                .unwrap_or_default()
                .to_string();
            assert!(
                !message.contains(marker),
                "case {label:?}: the refusal must not report the submitted marker {marker:?} \
                 back, got {message:?}"
            );
            if let Some(scope) = &scope {
                assert!(
                    !message.contains(scope.as_str()),
                    "case {label:?}: the refusal must not report the scope {scope:?} the store \
                     holds, got {message:?}"
                );
            }
            said.push(message);
        }
        let first = said.first().cloned().unwrap_or_default();
        assert!(
            said.iter().all(|message| *message == first),
            "case {case:?}: every unusable marker must be refused in the same words, got {said:?}"
        );
    }
}

/// The control: a `Marker` each listing DOES hand out is read rather than refused, so every
/// refusal above is about the value and not about the parameter being carried at all.
#[tokio::test]
async fn a_marker_a_listing_handed_out_is_not_refused() {
    let router = emulator();
    let distribution_id = one_distribution(&router).await;

    // The distribution just created is the only resume point this store can offer, and it is
    // one `ListDistributions` hands out as `NextMarker`.
    let answer = get(
        &router,
        &format!("/{API_VERSION}/distribution?Marker={distribution_id}"),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "a distribution id the store holds must resume the listing, got {} with {}",
        answer.status, answer.raw
    );
}
