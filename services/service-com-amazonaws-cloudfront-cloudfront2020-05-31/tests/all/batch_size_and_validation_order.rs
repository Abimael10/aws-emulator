// SPDX-License-Identifier: CC0-1.0

//! Which CONDITION a refusal reports when more than one rule could refuse the request, end
//! to end over HTTP. Two rules, each about a refusal a caller branches on:
//!
//! * the SIZE of an invalidation batch is a property of the request alone, so a batch naming
//!   more invalidation objects than one request may is refused with the modelled
//!   `BatchTooLarge` (HTTP 413) — on every create that takes an invalidation batch, and
//!   whatever invalidations the resource already holds. The quota over the invalidations a
//!   resource HOLDS is the other condition and keeps its own code, so the two never answer
//!   for each other: one is permanent and asks the caller to split the batch, the other is
//!   transient and asks it to wait. A document larger than this API READS is a third
//!   condition, decided on its bytes alone and BEFORE either of them: the transport's own
//!   `413`, carrying no error document, since nothing parsed the request far enough to name
//!   an operation or one of its modelled errors — which is the answer the recorded batch of
//!   60,000 real-looking paths gets, whichever surface it is submitted to.
//! * `ListDomainConflicts` evaluates the `DomainControlValidationResource` — the caller's
//!   proof that it controls the domain — BEFORE the `Domain` string's own shape, so a
//!   validation resource holding no certificate that covers the domain is refused in the
//!   sentence stating what the account has to hold, whatever the domain submitted spells.
//!   The domain's shape is what is left to refuse once that proof stands.
//!
//! The rows span what each rule generalises over: for the first, the surface the batch is
//! submitted on (a distribution and a distribution tenant), how many paths it names (at the
//! published bound, one past it, far past it) and whether the resource already holds an
//! invalidation; for the second, the validation resource (a distribution on CloudFront's own
//! default certificate, a tenant whose certificate covers another name, and — as the control
//! — a distribution on a custom certificate) and the form of the domain (well formed, a
//! wildcard, upper case, a single label, an empty label, whitespace and punctuation, a label
//! past the length a label may carry, and the empty value).
//!
//! The harness is [`crate::distribution_lifecycle`]'s; the tenant invalidation surface is
//! reached through [`crate::tenant_invalidations`]'s helpers and the domain-conflict surface
//! through [`crate::domain_conflicts`]'s, rather than through second copies of them.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::service::envelope::MAX_REQUEST_BODY_BYTES;
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    BATCH_TOO_LARGE_MESSAGE, INVALID_DOMAIN_NAME_MESSAGE, MAX_DOMAIN_LABEL_LEN,
    MAX_INVALIDATION_PATHS, MISSING_CERTIFICATE_FOR_DOMAIN_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    batch_xml, call, config, create_distribution, create_invalidation, emulator, Answer,
    API_VERSION,
};
use crate::domain_conflicts::{list_domain_conflicts, Resource};
use crate::tenant_invalidations::{
    create_invalidation_for_tenant, distribution_with_tenant, list_invalidations_for_tenant,
    summaries,
};

// ---------------------------------------------------------------------------
// The size of an invalidation batch
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
    /// What this surface is called in an assertion message.
    fn label(self) -> &'static str {
        match self {
            Surface::Distribution => "CreateInvalidation",
            Surface::Tenant => "CreateInvalidationForDistributionTenant",
        }
    }
}

/// The resource one surface's batches are submitted against, created through the API.
async fn resource_for(router: &MultiServiceRouter, surface: Surface, case: &str) -> String {
    match surface {
        Surface::Distribution => {
            let created =
                create_distribution(router, &config("batch-size", &format!("ref-{case}")).xml())
                    .await;
            assert_eq!(
                created.status, 201,
                "case {case}: expected the distribution to be created, got {} with {}",
                created.status, created.raw
            );
            created
                .root()
                .text_at("Id")
                .unwrap_or_else(|| panic!("case {case}: the create carries no Id: {}", created.raw))
                .to_string()
        }
        Surface::Tenant => distribution_with_tenant(router, case).await,
    }
}

/// Submit `batch` on `surface` against `id`.
async fn submit(
    router: &MultiServiceRouter,
    surface: Surface,
    id: &str,
    batch: &str,
) -> Answer {
    match surface {
        Surface::Distribution => create_invalidation(router, id, batch).await,
        Surface::Tenant => create_invalidation_for_tenant(router, id, batch).await,
    }
}

/// How many invalidations `surface` reports for `id`.
async fn held(router: &MultiServiceRouter, surface: Surface, id: &str) -> usize {
    let listed = match surface {
        Surface::Distribution => {
            call(
                router,
                "GET",
                &format!("/{API_VERSION}/distribution/{id}/invalidation"),
                None,
                None,
            )
            .await
        }
        Surface::Tenant => list_invalidations_for_tenant(router, id, "").await,
    };
    assert_eq!(
        listed.status,
        200,
        "case {}: expected the invalidation listing to be served, got {} with {}",
        surface.label(),
        listed.status,
        listed.raw
    );
    summaries(listed.root()).len()
}

/// A batch of `count` distinct paths, each naming one object.
fn paths(count: usize) -> Vec<String> {
    (0..count).map(|index| format!("/{index:06}.html")).collect()
}

/// An oversized batch is refused for its SIZE — `BatchTooLarge` (413) — on every surface
/// that takes an invalidation batch, whatever that resource already holds, and persists
/// nothing; a batch AT the published bound is accepted.
#[tokio::test]
async fn an_oversized_invalidation_batch_is_refused_for_its_size_on_every_surface() {
    let router = emulator();

    // Each row: how many paths the batch names, and whether the service accepts it.
    // `MAX_INVALIDATION_PATHS` is the service's published per-request object bound, so the
    // rows sit at it, one past it and far past it.
    let rows: &[(&str, usize, bool)] = &[
        ("at the bound", MAX_INVALIDATION_PATHS, true),
        ("one past the bound", MAX_INVALIDATION_PATHS + 1, false),
        ("far past the bound", MAX_INVALIDATION_PATHS * 2, false),
    ];

    for surface in [Surface::Distribution, Surface::Tenant] {
        let id = resource_for(&router, surface, &format!("size-{}", surface.label())).await;

        // One accepted batch FIRST, so every refusal below is answered on a resource that
        // already holds an invalidation: the size of the batch submitted is the condition,
        // not what the resource holds.
        let seeded = submit(
            &router,
            surface,
            &id,
            &batch_xml("size-seed", &["/seed.html"]),
        )
        .await;
        assert_eq!(
            seeded.status,
            201,
            "case {}/seed: expected HTTP 201, got {} with {}",
            surface.label(),
            seeded.status,
            seeded.raw.chars().take(400).collect::<String>()
        );

        for (case, count, is_accepted) in rows {
            let owned = paths(*count);
            let batch: Vec<&str> = owned.iter().map(String::as_str).collect();
            let before = held(&router, surface, &id).await;
            let answer = submit(
                &router,
                surface,
                &id,
                &batch_xml(&format!("size-{case}-{count}"), &batch),
            )
            .await;
            let after = held(&router, surface, &id).await;
            let label = format!("{}/{case}", surface.label());

            if *is_accepted {
                assert_eq!(
                    answer.status, 201,
                    "case {label}: expected HTTP 201 for {count} paths, got {} with {}",
                    answer.status,
                    answer.raw.chars().take(400).collect::<String>()
                );
                assert_eq!(
                    after,
                    before + 1,
                    "case {label}: an accepted batch is one more invalidation, got {after} \
                     after {before}"
                );
                continue;
            }

            // "Invalidation batch specified is too large." — the modelled `BatchTooLarge`,
            // whose status the model binds to 413.
            assert_eq!(
                answer.status, 413,
                "case {label}: expected HTTP 413 for {count} paths, got {} with {}",
                answer.status,
                answer.raw.chars().take(400).collect::<String>()
            );
            assert_eq!(
                answer.error_code(),
                "BatchTooLarge",
                "case {label}: expected BatchTooLarge for {count} paths, got {:?}",
                answer.error_code()
            );
            assert_eq!(
                answer.root().find_text("Message"),
                Some(BATCH_TOO_LARGE_MESSAGE),
                "case {label}: expected the batch-size sentence, got {:?}",
                answer.root().find_text("Message")
            );
            assert_eq!(
                after, before,
                "case {label}: a refused batch persists nothing, so the resource still holds \
                 {before} invalidations, got {after}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// A document larger than this API reads
// ---------------------------------------------------------------------------

/// A batch document longer than `floor` bytes, naming `count` paths: the path LENGTH is
/// derived from the ceiling, so the count stays the dimension a row varies.
fn batch_over(reference: &str, count: usize, floor: usize) -> String {
    let length = floor / count + 16;
    let owned: Vec<String> = (0..count)
        .map(|index| format!("/{}-{index:06}.html", "a".repeat(length)))
        .collect();
    let batch: Vec<&str> = owned.iter().map(String::as_str).collect();
    let document = batch_xml(reference, &batch);
    assert!(
        document.len() > floor,
        "case {reference}: expected a document past {floor} bytes, built one of {}",
        document.len()
    );
    document
}

/// A request whose DOCUMENT is larger than this API reads is refused on its bytes alone: the
/// transport's 413 carrying no error document at all, on every surface, whatever the batch
/// itself describes — while a document the API DOES read and that names more objects than one
/// request may is the modelled `BatchTooLarge`, which carries its code under the same status.
#[tokio::test]
async fn a_document_larger_than_the_api_reads_is_refused_with_no_error_document() {
    let router = emulator();

    // Each row: how many paths the document names, whether its bytes are past the API's
    // document ceiling, and so which of the two refusals answers it. The ceiling bounds
    // BYTES and the object bound bounds COUNT, so the rows cross the two.
    let rows: &[(&str, usize, bool)] = &[
        // A count the object bound admits, in a document too large to read.
        ("a path count the bound admits", MAX_INVALIDATION_PATHS / 4, true),
        // A count past the object bound as well, so the two rules are told apart by the
        // bytes and not by the count.
        ("a path count past the bound", MAX_INVALIDATION_PATHS + 1, true),
        // The same count, in a document the API reads: the modelled refusal.
        ("a readable document past the bound", MAX_INVALIDATION_PATHS + 1, false),
    ];

    for surface in [Surface::Distribution, Surface::Tenant] {
        let id = resource_for(&router, surface, &format!("payload-{}", surface.label())).await;

        for (case, count, past_ceiling) in rows {
            let label = format!("{}/{case}", surface.label());
            let reference = format!("payload-{case}-{count}");
            let document = if *past_ceiling {
                batch_over(&reference, *count, MAX_REQUEST_BODY_BYTES)
            } else {
                let owned = paths(*count);
                let batch: Vec<&str> = owned.iter().map(String::as_str).collect();
                let document = batch_xml(&reference, &batch);
                assert!(
                    document.len() <= MAX_REQUEST_BODY_BYTES,
                    "case {label}: expected a document this API reads, built one of {} \
                     bytes",
                    document.len()
                );
                document
            };
            let before = held(&router, surface, &id).await;
            let answer = submit(&router, surface, &id, &document).await;
            let after = held(&router, surface, &id).await;

            if *past_ceiling {
                // Nothing read the request, so there is nothing for the answer to say: a
                // client has only the status to classify it by.
                assert_eq!(
                    answer.status, 413,
                    "case {label}: expected HTTP 413, got {} with {}",
                    answer.status,
                    answer.raw.chars().take(400).collect::<String>()
                );
                assert!(
                    answer.raw.trim().is_empty(),
                    "case {label}: expected no error document at all, got {}",
                    answer.raw.chars().take(400).collect::<String>()
                );
            } else {
                assert_eq!(
                    answer.status, 413,
                    "case {label}: expected HTTP 413, got {} with {}",
                    answer.status,
                    answer.raw.chars().take(400).collect::<String>()
                );
                assert_eq!(
                    answer.error_code(),
                    "BatchTooLarge",
                    "case {label}: expected the modelled BatchTooLarge, got {:?} in {}",
                    answer.error_code(),
                    answer.raw.chars().take(400).collect::<String>()
                );
            }
            assert_eq!(
                after, before,
                "case {label}: a refused request persists nothing, so the resource still \
                 holds {before} invalidations, got {after}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The two recorded oversized-batch conditions, side by side
// ---------------------------------------------------------------------------

/// How long a path of the RECORDED oversized batch is, in characters: ~60, which is what a
/// client naming real object keys sends.
const RECORDED_PATH_CHARS: usize = 60;

/// How many paths that recorded batch names.
const RECORDED_PATH_COUNT: usize = 60_000;

/// A batch of [`RECORDED_PATH_COUNT`] paths of [`RECORDED_PATH_CHARS`] characters each — the
/// document the recorded oversized-batch call submits.
fn recorded_oversized_document(reference: &str) -> String {
    let owned: Vec<String> = (0..RECORDED_PATH_COUNT)
        .map(|index| {
            let stem = format!("{index:06}");
            format!(
                "/{}{stem}.html",
                "o".repeat(RECORDED_PATH_CHARS - stem.len() - "/.html".len())
            )
        })
        .collect();
    let batch: Vec<&str> = owned.iter().map(String::as_str).collect();
    batch_xml(reference, &batch)
}

/// The two oversized-batch conditions are told apart by the document's BYTES, which is what
/// reconciles the two recordings of this surface: a batch of 60,000 real-looking paths is a
/// document past what this API reads, so it is refused unread with a bare `413` carrying no
/// error document (a client's SDK reports that refusal's code as the status string), while a
/// document the API DOES read and that names more objects than one request may is the
/// modelled `BatchTooLarge` the object bound answers for a document the API does read.
#[tokio::test]
async fn the_recorded_oversized_batch_is_past_the_document_ceiling_on_every_surface() {
    let router = emulator();

    for surface in [Surface::Distribution, Surface::Tenant] {
        let label = surface.label();
        let id = resource_for(&router, surface, &format!("recorded-{label}")).await;
        let document = recorded_oversized_document(&format!("recorded-oversized-{label}"));
        assert!(
            document.len() > MAX_REQUEST_BODY_BYTES,
            "case {label}: the recorded batch of {RECORDED_PATH_COUNT} paths of \
             {RECORDED_PATH_CHARS} characters must be past the {MAX_REQUEST_BODY_BYTES}-byte \
             document ceiling, built {} bytes",
            document.len()
        );

        let before = held(&router, surface, &id).await;
        let answer = submit(&router, surface, &id, &document).await;
        let after = held(&router, surface, &id).await;

        assert_eq!(
            answer.status, 413,
            "case {label}: expected HTTP 413, got {} with {}",
            answer.status,
            answer.raw.chars().take(400).collect::<String>()
        );
        assert!(
            answer.raw.trim().is_empty(),
            "case {label}: a document past the ceiling is refused unread, so there is no \
             error document to read, got {}",
            answer.raw.chars().take(400).collect::<String>()
        );
        assert_eq!(
            after, before,
            "case {label}: a refused request persists nothing, so the resource still holds \
             {before} invalidations, got {after}"
        );
    }
}

// ---------------------------------------------------------------------------
// Which refusal a domain-conflict listing reports
// ---------------------------------------------------------------------------

/// What a row expects of the answer.
#[derive(Debug, Clone, Copy)]
enum Expected {
    /// HTTP 200: the request named a resource that validates the domain, and a domain this
    /// service can read.
    Served,
    /// HTTP 400 `InvalidArgument` carrying this sentence.
    Refused(&'static str),
}

/// The forms a `Domain` takes, each with whether this service can read it as a domain name.
///
/// `DomainItem$Domain` is a plain string, so the shape of a DNS name is the service's own
/// check: at least two non-empty labels, each at most `MAX_DOMAIN_LABEL_LEN` characters and
/// carrying no whitespace. The rows sit on both sides of that rule — including one at the
/// label-length bound and one past it.
fn domain_forms(zone: &str) -> Vec<(&'static str, String, bool)> {
    let at_label_bound = "a".repeat(MAX_DOMAIN_LABEL_LEN);
    let past_label_bound = "a".repeat(MAX_DOMAIN_LABEL_LEN + 1);
    vec![
        ("a well-formed name", format!("fine.{zone}"), true),
        ("a wildcard name", format!("*.{zone}"), true),
        (
            "a well-formed name in upper case",
            format!("FINE.{zone}").to_uppercase(),
            true,
        ),
        (
            "a label at the length a label may carry",
            format!("{at_label_bound}.{zone}"),
            true,
        ),
        (
            "a label past the length a label may carry",
            format!("{past_label_bound}.{zone}"),
            false,
        ),
        ("a single label", "localhost".to_string(), false),
        ("an empty label", format!("empty..{zone}"), false),
        (
            "whitespace and punctuation",
            "not a valid domain !!".to_string(),
            false,
        ),
        ("the empty value", String::new(), false),
    ]
}

/// The validation resource is evaluated before the `Domain` string: a resource holding no
/// certificate that covers the domain is refused in the certificate sentence whatever the
/// domain spells, and the domain's own shape is refused only once that proof stands.
#[tokio::test]
async fn a_domain_conflict_listing_evaluates_the_validation_resource_before_the_domain() {
    let router = emulator();
    let zone = "validation-order.example.com";

    // A distribution declaring an alternate domain name carries a CUSTOM viewer certificate;
    // one declaring none is served on CloudFront's own default certificate, which covers no
    // alternate domain name at all.
    let custom_certificate = crate::domain_conflicts::create_standard_distribution(
        &router,
        "ref-order-custom",
        &[&format!("cert.{zone}")],
    )
    .await;
    let default_certificate =
        crate::domain_conflicts::create_standard_distribution(&router, "ref-order-default", &[])
            .await;
    // A tenant whose managed certificate was requested for a name in ANOTHER zone: no
    // certificate of it covers any domain the rows ask about, whether the clock has made its
    // issuance due yet or not.
    let elsewhere_tenant = distribution_with_tenant(&router, "validation-order").await;

    // Each resource row: what it is, how the request names it, and the sentence a domain-less
    // proof is refused with — or `None` where the resource DOES validate the domain, which is
    // the control that the domain's own shape is still read.
    let resources: Vec<(&str, Resource<'_>, Option<&'static str>)> = vec![
        (
            "a distribution on CloudFront's own default certificate",
            Resource::Distribution(&default_certificate),
            Some(MISSING_CERTIFICATE_FOR_DOMAIN_MESSAGE),
        ),
        (
            "a tenant whose certificate was requested for another name",
            Resource::Tenant(&elsewhere_tenant),
            Some(MISSING_CERTIFICATE_FOR_DOMAIN_MESSAGE),
        ),
        (
            "a distribution on a custom certificate",
            Resource::Distribution(&custom_certificate),
            None,
        ),
    ];

    for (resource_case, resource, missing_certificate) in resources {
        for (domain_case, domain, is_readable) in domain_forms(zone) {
            let expected = match (missing_certificate, is_readable) {
                // The proof the caller has to carry is what the refusal reports, whatever
                // the domain spells.
                (Some(sentence), _) => Expected::Refused(sentence),
                // The proof stands, so the domain's own shape is what is left to refuse.
                (None, false) => Expected::Refused(INVALID_DOMAIN_NAME_MESSAGE),
                (None, true) => Expected::Served,
            };
            let answer = list_domain_conflicts(&router, &domain, resource, None, None).await;
            let label = format!("{resource_case} / {domain_case}");
            assert_refusal(&label, &domain, &answer, expected);
        }
    }
}

/// Assert one answer is what the row expects of it.
fn assert_refusal(label: &str, domain: &str, answer: &Answer, expected: Expected) {
    match expected {
        Expected::Served => assert_eq!(
            answer.status, 200,
            "case {label:?} ({domain:?}): expected HTTP 200, got {} with {}",
            answer.status, answer.raw
        ),
        Expected::Refused(sentence) => {
            assert_eq!(
                answer.status, 400,
                "case {label:?} ({domain:?}): expected HTTP 400, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                "InvalidArgument",
                "case {label:?} ({domain:?}): expected InvalidArgument, got {:?} in {}",
                answer.error_code(),
                answer.raw
            );
            let message = answer.root().find_text("Message").unwrap_or_default();
            assert_eq!(
                message, sentence,
                "case {label:?} ({domain:?}): expected the sentence {sentence:?}, got \
                 {message:?}"
            );
        }
    }
}
