// SPDX-License-Identifier: CC0-1.0

//! The Amazon SimpleDB 2009-04-15 AWS Query API, driven through the WHOLE assembled
//! emulator — including by a client that names no service at all.
//!
//! RULE: a request whose resolved routing key names NO service — it carries no SigV4
//! credential scope, no `X-Amz-Target`, and reaches the emulator on a loopback host, which
//! names nothing — is served by the service whose own API the request's form-encoded body
//! names in its `Action` and `Version` fields; and a peer that claims the same query wire
//! form keeps the requests that name ITS API.
//!
//! A conforming client of this API is exactly that case. Amazon SimpleDB 2009-04-15 is
//! signed with Signature Version 2, which carries the caller's credential among the
//! request's own parameters instead of in an `Authorization` header, so such a request
//! brings no credential scope for a routing key to come from — and the client is pointed
//! at this emulator's own address, so the host tier names nothing either. All that is left
//! naming the API is the body, which only the operation-resolution seam sees.
//!
//! Why this suite lives here rather than in the simpledbv2 crate: every per-service suite
//! builds a router holding its OWN service alone, where a request nobody claims and a
//! request a PEER claims look alike — nothing is mounted to claim it. The defect pinned
//! here is of the second kind: on the assembled router another service also claims the
//! shared query wire form, and it answered these calls `400 InvalidAction` in its own
//! envelope, so the whole 2009-04-15 surface was unreachable from a conforming client even
//! though the service that implements it was mounted beside it. The assembled router is
//! the only place that is visible.
//!
//! The request plumbing is `zz_sdb_probe.rs`; the rows below are the rule.

use serde_json::json;

use crate::query_wire::{elements, opt_element};
use crate::zz_sdb_probe::{
    emulator, emulator_verifying, modelled, on_a_sized_thread, query, served, Addressed,
    API_VERSION, SDB_NAMESPACE,
};

/// The router's own refusals: nobody claimed the request, and several did. No case below
/// may be answered either one.
const UNKNOWN_SERVICE: &str = "UnknownService";
const AMBIGUOUS_SERVICE: &str = "AmbiguousService";

/// STS's own query API version, which its anonymous calls name.
const STS_API_VERSION: &str = "2011-06-15";

/// The bucket the export case names — a name the modelled `S3BucketName` pattern admits.
const BUCKET: &str = "sdb-unattributed-export-bucket";

/// The published bounds on a 2009-04-15 domain name, read from the service that publishes
/// them rather than restated here.
const MAX_DOMAIN_NAME_LEN: usize =
    service_com_amazonaws_simpledbv2_simpledbv2::state::simpledb_domains::MAX_DOMAIN_NAME_LEN;
const MIN_DOMAIN_NAME_LEN: usize =
    service_com_amazonaws_simpledbv2_simpledbv2::state::simpledb_domains::MIN_DOMAIN_NAME_LEN;

/// The domain names one addressing works on: the published minimum length, the published
/// maximum length, every character class the API admits, and a mixed-case name — the
/// dimensions a domain name varies along — each prefixed so no two addressings share a
/// domain.
fn domain_names(addressed: Addressed) -> Vec<(&'static str, String)> {
    let tag = addressed.tag();
    vec![
        ("the published minimum length", format!("{tag}ab")),
        (
            "the published maximum length",
            format!("{tag}{}", "d".repeat(MAX_DOMAIN_NAME_LEN - tag.len())),
        ),
        (
            "every character class the API admits",
            format!("{tag}.name-with_every.admitted-char0"),
        ),
        ("a mixed-case name", format!("{tag}MixedCaseDomain")),
    ]
}

/// However the request names this endpoint — by a credential scope, by the endpoint host,
/// or by nothing at all — the 2009-04-15 API is served from the one store, so what one
/// call writes is what a later call reads.
#[test]
fn the_2009_04_15_api_is_served_however_a_request_names_this_endpoint() {
    on_a_sized_thread("sdb-query-api-claim", || {
        let (router, runtime) = emulator();

        for addressed in [
            Addressed::Nothing,
            Addressed::CredentialScope,
            Addressed::EndpointHost,
        ] {
            for (case, domain) in domain_names(addressed) {
                assert!(
                    (MIN_DOMAIN_NAME_LEN..=MAX_DOMAIN_NAME_LEN).contains(&domain.chars().count()),
                    "case {case} addressed {addressed:?}: the case's own domain name must be one \
                     the API admits, got {} characters",
                    domain.chars().count()
                );

                // CREATE, and read the domain back through this API's own listing.
                served(
                    &router,
                    &runtime,
                    addressed,
                    "CreateDomain",
                    &[("DomainName", &domain)],
                );
                let listed = served(&router, &runtime, addressed, "ListDomains", &[]);
                assert!(
                    elements(&listed, "DomainName").contains(&domain),
                    "case {case} addressed {addressed:?}: the created domain must be listed, got \
                     {listed}"
                );

                // WRITE an item, and read it back.
                served(
                    &router,
                    &runtime,
                    addressed,
                    "PutAttributes",
                    &[
                        ("DomainName", &domain),
                        ("ItemName", "item-widget"),
                        ("Attribute.0.Name", "colour"),
                        ("Attribute.0.Value", "blue"),
                    ],
                );
                let fetched = served(
                    &router,
                    &runtime,
                    addressed,
                    "GetAttributes",
                    &[("DomainName", &domain), ("ItemName", "item-widget")],
                );
                assert_eq!(
                    (
                        opt_element(&fetched, "Name").as_deref(),
                        opt_element(&fetched, "Value").as_deref()
                    ),
                    (Some("colour"), Some("blue")),
                    "case {case} addressed {addressed:?}: the written pair must be read back, got \
                     {fetched}"
                );

                // The written item is part of what this domain's metadata counts.
                let metadata = served(
                    &router,
                    &runtime,
                    addressed,
                    "DomainMetadata",
                    &[("DomainName", &domain)],
                );
                assert_eq!(
                    opt_element(&metadata, "ItemCount").as_deref(),
                    Some("1"),
                    "case {case} addressed {addressed:?}: the metadata must count the written \
                     item, got {metadata}"
                );

                // DELETE, and the domain is gone from the listing.
                served(
                    &router,
                    &runtime,
                    addressed,
                    "DeleteDomain",
                    &[("DomainName", &domain)],
                );
                let after = served(&router, &runtime, addressed, "ListDomains", &[]);
                assert!(
                    !elements(&after, "DomainName").contains(&domain),
                    "case {case} addressed {addressed:?}: the deleted domain must be gone from \
                     the listing, got {after}"
                );
            }
        }
    });
}

/// A request that names no service is answered by the API its BODY names: this API's own
/// action by this API, a peer's own action by that peer, and neither by the router's
/// refusals.
#[test]
fn a_request_that_names_no_service_reaches_the_api_its_body_names() {
    on_a_sized_thread("sdb-query-api-peers", || {
        let (router, runtime) = emulator();

        // This API's own request: served, in this API's envelope.
        let created = query(
            &router,
            &runtime,
            Addressed::Nothing,
            &[
                ("Action", "CreateDomain"),
                ("Version", API_VERSION),
                ("DomainName", "named-by-its-body"),
            ],
        );
        assert_eq!(
            (created.status, created.body.contains(SDB_NAMESPACE)),
            (200, true),
            "case this API's own action: expected a 200 in the 2009-04-15 namespace, got \
             {created:?}"
        );

        // An action this API publishes, under a version no Amazon SimpleDB API declares:
        // still this API's request — it names one of this API's own actions — so it is this
        // API's own refusal of that version, not a peer's refusal of the action.
        let versioned = query(
            &router,
            &runtime,
            Addressed::Nothing,
            &[("Action", "ListDomains"), ("Version", "2007-11-07")],
        );
        assert_eq!(
            (
                versioned.status,
                opt_element(&versioned.body, "Code").as_deref()
            ),
            (400, Some("NoSuchVersion")),
            "case an action of this API under a version it does not declare: expected this API's \
             own refusal, got {versioned:?}"
        );

        // A PEER's unattributed query request, in the same wire form: STS's anonymous
        // `AssumeRoleWithWebIdentity`, which names STS's API in its own `Action` and
        // `Version`. Claiming the shared wire form may not take it away from that peer —
        // with its version stated, and with no version stated at all, since this API is
        // named only by POSITIVE signals of its own.
        for (case, version) in [
            ("a peer's own action under its own version", Some(STS_API_VERSION)),
            ("a peer's own action under no version at all", None),
        ] {
            let mut parameters: Vec<(&str, &str)> = vec![
                ("Action", "AssumeRoleWithWebIdentity"),
                ("RoleArn", "arn:aws:iam::000000000000:role/unattributed-peer"),
                ("RoleSessionName", "peer-session"),
                ("WebIdentityToken", "not-a-jwt"),
            ];
            if let Some(version) = version {
                parameters.push(("Version", version));
            }
            let peer = query(&router, &runtime, Addressed::Nothing, &parameters);
            assert!(
                !peer.body.contains(SDB_NAMESPACE),
                "case {case}: it must not be answered in the 2009-04-15 namespace, got {peer:?}"
            );
            assert_eq!(
                opt_element(&peer.body, "Code").as_deref(),
                Some("InvalidIdentityToken"),
                "case {case}: expected the peer's own modelled refusal, got {peer:?}"
            );
            for router_refusal in [UNKNOWN_SERVICE, AMBIGUOUS_SERVICE] {
                assert!(
                    !peer.body.contains(router_refusal),
                    "case {case}: exactly one service may claim it, got {peer:?}"
                );
            }
        }
    });
}

/// The credential parameters a client of this API carries: the Signature Version 2 signing
/// process names the key id, its own version, the method, the timestamp and the signature
/// among the REQUEST'S PARAMETERS, with no `Authorization` header. The values are a
/// caller's to choose, which is the point of the row that sends them.
const SIGNATURE_V2_CREDENTIAL: [(&str, &str); 5] = [
    ("AWSAccessKeyId", "AKIDSDBTESTKEY"),
    ("SignatureVersion", "2"),
    ("SignatureMethod", "HmacSHA256"),
    ("Timestamp", "2026-01-01T00:00:00Z"),
    ("Signature", "YmFzZTY0c2lnbmF0dXJl"),
];

/// One row of [`claiming_the_query_wire_form_opens_no_door`]: how the request is addressed
/// and which credential parameters it carries.
struct CredentialCase<'a> {
    case: &'a str,
    addressed: Addressed,
    credential: Vec<(&'a str, &'a str)>,
}

/// Claiming the query wire form opens no door: with request authentication ON, a request to
/// this surface that the router cannot verify is refused before any handler runs, and is
/// never answered a 2009-04-15 document.
///
/// The surface declares the one scheme this router verifies, SigV4, exactly as the modelled
/// operations do, so the claim added for a request that names no service does not make the
/// API callable by a caller the router cannot authenticate.
#[test]
fn claiming_the_query_wire_form_opens_no_door() {
    on_a_sized_thread("sdb-query-api-auth", || {
        let (router, runtime) = emulator_verifying();

        let rows = vec![
            CredentialCase {
                case: "no credential at all",
                addressed: Addressed::Nothing,
                credential: Vec::new(),
            },
            CredentialCase {
                case: "the parameters this API's own signing process carries, which this \
                       router holds no authenticator for",
                addressed: Addressed::Nothing,
                credential: SIGNATURE_V2_CREDENTIAL.to_vec(),
            },
            CredentialCase {
                case: "a SigV4 credential whose signature the router cannot verify",
                addressed: Addressed::CredentialScope,
                credential: Vec::new(),
            },
        ];

        for row in rows {
            let mut parameters: Vec<(&str, &str)> = vec![
                ("Action", "CreateDomain"),
                ("Version", API_VERSION),
                ("DomainName", "never-admitted"),
            ];
            parameters.extend_from_slice(&row.credential);
            let refused = query(&router, &runtime, row.addressed, &parameters);
            assert_eq!(
                refused.status, 403,
                "case {}: expected the authentication refusal, got {refused:?}",
                row.case
            );
            assert!(
                !refused.body.contains(SDB_NAMESPACE),
                "case {}: a request the router did not verify must not be answered a \
                 2009-04-15 document, got {refused:?}",
                row.case
            );
        }
    });
}

/// The domain a client created on the 2009-04-15 wire is the domain the modelled `/v2/*`
/// export operations resolve `domainName` against — and a domain nobody created is still
/// refused, so claiming that wire opened no door.
#[test]
fn a_domain_created_by_a_client_that_names_no_service_is_exportable() {
    on_a_sized_thread("sdb-query-api-export", || {
        let (router, runtime) = emulator();
        let domain = "export-source-domain";

        served(
            &router,
            &runtime,
            Addressed::Nothing,
            "CreateDomain",
            &[("DomainName", domain)],
        );
        for item in ["item-one", "item-two"] {
            served(
                &router,
                &runtime,
                Addressed::Nothing,
                "PutAttributes",
                &[
                    ("DomainName", domain),
                    ("ItemName", item),
                    ("Attribute.0.Name", "colour"),
                    ("Attribute.0.Value", "blue"),
                ],
            );
        }

        let (status, started) = modelled(
            &router,
            &runtime,
            "StartDomainExport",
            &json!({"domainName": domain, "s3Bucket": BUCKET}),
        );
        assert_eq!(
            status, 200,
            "case a client-created domain: StartDomainExport must accept it, got {status} with \
             {started:?}"
        );
        for member in ["clientToken", "exportArn", "requestedAt"] {
            assert!(
                started.get(member).is_some(),
                "case a client-created domain: the output must report {member}, got {started:?}"
            );
        }
        let export_arn = started["exportArn"]
            .as_str()
            .expect("StartDomainExport reports an exportArn")
            .to_string();

        let (status, listed) = modelled(
            &router,
            &runtime,
            "ListExports",
            &json!({"domainName": domain}),
        );
        assert_eq!(
            status, 200,
            "case a client-created domain: ListExports must enumerate its exports, got {status} \
             with {listed:?}"
        );
        let summaries = listed["exportSummaries"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            summaries.iter().any(|summary| {
                summary["exportArn"].as_str() == Some(export_arn.as_str())
                    && summary["domainName"].as_str() == Some(domain)
            }),
            "case a client-created domain: the started export must be summarised for it, got \
             {listed:?}"
        );

        let (status, refused) = modelled(
            &router,
            &runtime,
            "StartDomainExport",
            &json!({"domainName": "never-created-domain", "s3Bucket": BUCKET}),
        );
        assert_eq!(
            status, 400,
            "case a domain nobody created: the export must be refused, got {status} with \
             {refused:?}"
        );
    });
}
