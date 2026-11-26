// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the address an app authorization reports for its OAuth
//! flow (`AppAuthorization$authUrl`), driven over the wire (restJson1) through
//! the same router the emulator boots.
//!
//! The RULE every test below states over a TABLE of inputs: an address a
//! response reports for a caller to FOLLOW is composed ONCE, by the call that
//! mints the resource, out of the endpoint THIS emulator is served at plus that
//! resource's own path, and is then stored — so every later read serializes the
//! same bytes, no read re-composes it, and a fetch of it reaches this emulator
//! rather than a host it does not answer on. An `authType` that carries its
//! credential already has no consent step, so it reports no address at all.
//!
//! The harness (the router, `Answer`, `Fetched`, `encode`, `arn_of`, `uuid_of`)
//! is the one `app_bundle_ingestion_lifecycle` already builds.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_appfabric_fabricfrontendservice::state::{
    oauth_consent_path, OAUTH_CONSENT_MEDIA_TYPE,
};

use crate::app_authorization_deletion::delete_authorization;
use crate::app_bundle_ingestion_lifecycle::{arn_of, encode, uuid_of, Answer, Emulator, Fetched};

/// The two values the model's `AuthType` enum carries
/// (`smithy_query(query="shape", shape="AuthType")` —
/// `shared_types::AUTH_TYPE_VALUES` = `apiKey` | `oauth2`), each with whether an
/// authorization of that type is granted through an OAuth consent step and
/// therefore reports an `authUrl`.
const AUTH_TYPES: &[(&str, bool)] = &[("apiKey", false), ("oauth2", true)];

/// The API key an `apiKey` authorization is created with — a recognisable string,
/// so a test can assert the reported address carries no credential material.
const API_KEY: &str = "secret-api-key-4d2f";

/// The OAuth2 client id an `oauth2` authorization is created with.
const CLIENT_ID: &str = "secret-client-id-8b31";

/// The OAuth2 client secret an `oauth2` authorization is created with.
const CLIENT_SECRET: &str = "secret-client-secret-c07e";

/// The credential body one `authType` takes — the pairing the model's own
/// documentation of `credential` requires ("Specify credentials that match the
/// authorization type for your request").
fn credential_for(auth_type: &str) -> Value {
    if auth_type == "apiKey" {
        json!({"apiKeyCredential": {"apiKey": API_KEY}})
    } else {
        json!({"oauth2Credential": {"clientId": CLIENT_ID, "clientSecret": CLIENT_SECRET}})
    }
}

/// The base URL this emulator is served at, as a handler reads it
/// (`ctx.endpoint()`): the data port's URL when the emulator is served, and
/// [`smithy_server_runtime::UNSERVED_URL_BASE`] in a test, which mounts the
/// service on no port.
fn served_base() -> &'static str {
    smithy_server_runtime::UNSERVED_URL_BASE
}

/// The address an `oauth2` authorization's consent step is expected to be
/// performed at: the endpoint THIS emulator is served at, followed by that
/// authorization's own consent path.
fn expected_auth_url(bundle_arn: &str, authorization_arn: &str) -> String {
    format!(
        "{}{}",
        served_base().trim_end_matches('/'),
        oauth_consent_path(&uuid_of(bundle_arn), &uuid_of(authorization_arn))
    )
}

/// One app bundle in `region`, as its ARN. AppFabric holds ONE bundle per
/// account per Region, so a fixture that needs its own Region gets its own
/// emulator.
async fn app_bundle(emulator: &Emulator, region: &str) -> String {
    let created = emulator
        .create_app_bundle_in_region(region, json!({}))
        .await;
    assert_eq!(
        created.status, 201,
        "region {region:?}: the set-up app bundle must be created, got HTTP {} with body {}",
        created.status, created.body
    );
    arn_of(&created.body, "appBundle")
}

/// `CreateAppAuthorization`, signed for `region`.
async fn create_authorization(
    emulator: &Emulator,
    region: &str,
    bundle: &str,
    app: &str,
    auth_type: &str,
    tenant: &str,
) -> Answer {
    emulator
        .call_in_region(
            region,
            "POST",
            &format!("/appbundles/{}/appauthorizations", encode(bundle)),
            Some(json!({
                "app": app,
                "authType": auth_type,
                "credential": credential_for(auth_type),
                "tenant": {
                    "tenantIdentifier": tenant,
                    "tenantDisplayName": format!("{tenant} display"),
                },
            })),
        )
        .await
}

/// `GetAppAuthorization` for either identifier form, signed for `region`.
async fn get_authorization(
    emulator: &Emulator,
    region: &str,
    bundle: &str,
    authorization: &str,
) -> Answer {
    emulator
        .call_in_region(
            region,
            "GET",
            &format!(
                "/appbundles/{}/appauthorizations/{}",
                encode(bundle),
                encode(authorization)
            ),
            None,
        )
        .await
}

/// `UpdateAppAuthorization` (PATCH), signed for `region`, carrying `body`.
async fn update_authorization(
    emulator: &Emulator,
    region: &str,
    bundle: &str,
    authorization: &str,
    body: Value,
) -> Answer {
    emulator
        .call_in_region(
            region,
            "PATCH",
            &format!(
                "/appbundles/{}/appauthorizations/{}",
                encode(bundle),
                encode(authorization)
            ),
            Some(body),
        )
        .await
}

/// The `authUrl` of an `appAuthorization` body, or `None` when the member is
/// absent.
fn auth_url(answer: &Answer) -> Option<String> {
    answer
        .body
        .get("appAuthorization")
        .unwrap_or_else(|| panic!("expected an appAuthorization in {}", answer.body))
        .get("authUrl")
        .map(|url| {
            url.as_str()
                .unwrap_or_else(|| panic!("authUrl must be a string, got {url}"))
                .to_string()
        })
}

/// The `appAuthorizationArn` of a success body.
fn authorization_arn(answer: &Answer) -> String {
    answer
        .body
        .get("appAuthorization")
        .and_then(|authorization| authorization.get("appAuthorizationArn"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "expected appAuthorization.appAuthorizationArn in {}",
                answer.body
            )
        })
        .to_string()
}

/// The path half of a reported address: everything after the endpoint this
/// emulator is served at, which is what a fetch of the address addresses.
fn served_path(case: &str, url: &str) -> String {
    url.strip_prefix(served_base().trim_end_matches('/'))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the address to name the endpoint this emulator is \
                 served at ({:?}), got {url:?}",
                served_base()
            )
        })
        .to_string()
}

/// RULE: an app authorization granted through an OAuth consent step (`authType`
/// `oauth2`) reports the address that step is performed at — this emulator's own
/// endpoint plus the authorization's own consent path — and one that carries its
/// credential already (`apiKey`) reports no address at all. The address is
/// stored on the record by the create call, so every read reports the same
/// bytes, it survives the connect handshake that moves the authorization's
/// status, and it carries no credential material.
///
/// The table spans both `AuthType` values, applications the issue did not cite,
/// and several Regions — the Region a call is signed for never appears in the
/// address, because the address names the emulator, not an AWS endpoint.
#[tokio::test]
async fn an_oauth2_authorization_reports_its_consent_url_and_an_api_key_one_reports_none() {
    // (case, app, tenant, the Region the create call is signed for)
    let placements: &[(&str, &str, &str, &str)] = &[
        ("ZENDESK in the default Region", "ZENDESK", "tenant-z", "us-east-1"),
        ("ASANA in another Region", "ASANA", "tenant-a", "eu-west-1"),
        ("M365AUDITLOGS in a third Region", "M365AUDITLOGS", "tenant-m", "ap-southeast-2"),
    ];

    for (placement, app, tenant, region) in placements {
        for (auth_type, reports_url) in AUTH_TYPES {
            let case = format!("{placement} with authType {auth_type}");
            // A fresh emulator per row: a Region holds one app bundle.
            let emulator = Emulator::new();
            let bundle = app_bundle(&emulator, region).await;

            let created =
                create_authorization(&emulator, region, &bundle, app, auth_type, tenant).await;
            assert_eq!(
                created.status, 201,
                "case {case:?}: expected HTTP 201, got {} with body {}",
                created.status, created.body
            );
            let arn = authorization_arn(&created);
            let reported = auth_url(&created);

            if *reports_url {
                let expected = expected_auth_url(&bundle, &arn);
                assert_eq!(
                    reported.as_deref(),
                    Some(expected.as_str()),
                    "case {case:?}: expected the consent address {expected:?}, got {reported:?} \
                     in body {}",
                    created.body
                );
                for secret in [API_KEY, CLIENT_ID, CLIENT_SECRET] {
                    assert!(
                        !expected.contains(secret),
                        "case {case:?}: the reported address must carry no credential material, \
                         got {reported:?}"
                    );
                }
            } else {
                assert_eq!(
                    reported, None,
                    "case {case:?}: an authorization with no consent step must report no authUrl, \
                     got {reported:?} in body {}",
                    created.body
                );
            }

            // The address is part of the stored record, so a read by either
            // identifier form reports the record the create call returned.
            for identifier in [arn.clone(), uuid_of(&arn)] {
                let read = get_authorization(&emulator, region, &bundle, &identifier).await;
                assert_eq!(
                    read.status, 200,
                    "case {case:?} read by {identifier:?}: expected HTTP 200, got {} with body {}",
                    read.status, read.body
                );
                assert_eq!(
                    read.body.get("appAuthorization"),
                    created.body.get("appAuthorization"),
                    "case {case:?} read by {identifier:?}: expected the created record {}, got {}",
                    created.body,
                    read.body
                );
            }

            // Connecting the authorization moves its status; the address it
            // reports is unchanged.
            let connect_body = if *auth_type == "apiKey" {
                json!({})
            } else {
                json!({"authRequest": {
                    "redirectUri": "https://example.com/oauth/callback",
                    "code": "placeholder-authorization-code",
                }})
            };
            let connected = emulator
                .call_in_region(
                    region,
                    "POST",
                    &format!(
                        "/appbundles/{}/appauthorizations/{}/connect",
                        encode(&bundle),
                        encode(&arn)
                    ),
                    Some(connect_body),
                )
                .await;
            assert_eq!(
                connected.status, 200,
                "case {case:?}: expected the connect call to succeed, got {} with body {}",
                connected.status, connected.body
            );
            let after = get_authorization(&emulator, region, &bundle, &arn).await;
            assert_eq!(
                auth_url(&after),
                reported,
                "case {case:?}: expected the consent address {reported:?} to survive the connect, \
                 got {}",
                after.body
            );
        }
    }
}

/// RULE: the address an `oauth2` authorization reports is one this emulator
/// ANSWERS: an unsigned fetch of it — the way a browser that was handed the URL
/// opens it — renders the consent page of that very authorization, out of the
/// stored record, and the fetch changes nothing a later caller sees.
///
/// The table spans applications and Regions, both of the ways a caller can have
/// arrived at the address (from the create response and from a later read), and
/// both methods a browser opens one with (`GET`, and the `HEAD` that asks only
/// what is there).
#[tokio::test]
async fn the_reported_consent_url_is_a_page_this_emulator_serves() {
    // (case, app, tenant, Region, whether the address is taken from a later read)
    let cases: &[(&str, &str, &str, &str, bool)] = &[
        ("SMARTSHEET from the create response", "SMARTSHEET", "tenant-s", "us-east-1", false),
        ("GOOGLE from a later read", "GOOGLE", "tenant-g", "eu-central-1", true),
        ("OKTA from a later read in another Region", "OKTA", "tenant-o", "us-west-2", true),
    ];

    for (case, app, tenant, region, from_read) in cases {
        let emulator = Emulator::new();
        let bundle = app_bundle(&emulator, region).await;
        let created =
            create_authorization(&emulator, region, &bundle, app, "oauth2", tenant).await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected HTTP 201, got {} with body {}",
            created.status, created.body
        );
        let arn = authorization_arn(&created);
        let source = if *from_read {
            get_authorization(&emulator, region, &bundle, &arn).await
        } else {
            created
        };
        let url = auth_url(&source).unwrap_or_else(|| {
            panic!(
                "case {case:?}: an oauth2 authorization must report an authUrl, got {}",
                source.body
            )
        });
        let path = served_path(case, &url);

        // Follow the address, unsigned: the page of that authorization.
        let page = emulator.fetch("GET", &path).await;
        assert_page(case, &page, 200);
        for expected in [
            app.to_string(),
            format!("{tenant} display"),
            (*tenant).to_string(),
            uuid_of(&arn),
            "PendingConnect".to_string(),
        ] {
            assert!(
                page.document.contains(&expected),
                "case {case:?}: the page at {url:?} must state {expected:?} of the stored \
                 authorization, got {:?}",
                page.document
            );
        }
        assert!(
            ![API_KEY, CLIENT_ID, CLIENT_SECRET]
                .iter()
                .any(|secret| page.document.contains(secret)),
            "case {case:?}: the page at {url:?} must carry no credential material, got {:?}",
            page.document
        );

        // A HEAD of the same address says what is there and carries no body.
        let head = emulator.fetch("HEAD", &path).await;
        assert_page(case, &head, 200);
        assert!(
            head.document.is_empty(),
            "case {case:?}: a HEAD of {url:?} must carry no body, got {:?}",
            head.document
        );

        // Opening the page is a READ: the authorization is exactly as it was.
        let after = get_authorization(&emulator, region, &bundle, &arn).await;
        assert_eq!(
            after.body.get("appAuthorization"),
            source.body.get("appAuthorization"),
            "case {case:?}: opening {url:?} must change nothing a later caller sees, expected {}, \
             got {}",
            source.body,
            after.body
        );
    }
}

/// RULE: nothing is invented for a consent address this emulator holds no
/// authorization for — an address of an authorization a delete removed, or one
/// of a resource that never existed, is answered as not found rather than with a
/// page.
///
/// The table spans the ways an address can name no stored authorization: the
/// authorization gone, the bundle gone, and ids no create ever minted.
#[tokio::test]
async fn a_consent_url_naming_no_stored_authorization_is_not_found() {
    const REGION: &str = "us-east-1";
    let emulator = Emulator::new();
    let bundle = app_bundle(&emulator, REGION).await;
    let created = create_authorization(&emulator, REGION, &bundle, "SLACK", "oauth2", "t-1").await;
    assert_eq!(
        created.status, 201,
        "the set-up authorization must be created, got {} with body {}",
        created.status, created.body
    );
    let arn = authorization_arn(&created);
    let held = auth_url(&created).expect("an oauth2 authorization reports an authUrl");

    // (case, the path fetched)
    let absent: &[(&str, String)] = &[
        (
            "an authorization id no create minted",
            oauth_consent_path(&uuid_of(&bundle), "7c58e8bc-4614-42c2-8bab-a842107c8e16"),
        ),
        (
            "a bundle id no create minted",
            oauth_consent_path("2f1be9f3-676c-45ef-b590-3dae0f6cc4d9", &uuid_of(&arn)),
        ),
        (
            "neither id ever minted",
            oauth_consent_path("not-a-bundle", "not-an-authorization"),
        ),
    ];
    for (case, path) in absent {
        let page = emulator.fetch("GET", path).await;
        assert_page(case, &page, 404);
        assert!(
            !page.document.contains("SLACK"),
            "case {case:?}: no stored authorization may be reported at {path:?}, got {:?}",
            page.document
        );
    }

    // The held authorization's own address is served until it is deleted, and
    // not found afterwards.
    let path = served_path("the held authorization", &held);
    assert_page("before the delete", &emulator.fetch("GET", &path).await, 200);
    let deleted = delete_authorization(&emulator, &bundle, &arn).await;
    assert_eq!(
        deleted.status, 204,
        "the set-up delete must answer HTTP 204, got {} with body {}",
        deleted.status, deleted.body
    );
    assert_page("after the delete", &emulator.fetch("GET", &path).await, 404);
}

/// RULE: the address is per AUTHORIZATION and is written once: every `oauth2`
/// authorization a bundle holds reports its own, naming its own identifier, and
/// an update that rotates the credential or renames the tenant leaves it exactly
/// as the create call reported it — the bytes a caller is holding on to between
/// the create call and the consent step.
///
/// The table spans the number of authorizations held at once and the members an
/// update supplies (`credential` alone, `tenant` alone, both).
#[tokio::test]
async fn every_oauth2_authorization_reports_its_own_unchanging_consent_url() {
    const REGION: &str = "us-east-1";
    // (case, the PATCH body)
    let updates: &[(&str, Value)] = &[
        (
            "rotating the credential",
            json!({"credential": {"oauth2Credential": {
                "clientId": "rotated-client-id",
                "clientSecret": "rotated-client-secret",
            }}}),
        ),
        (
            "renaming the tenant",
            json!({"tenant": {"tenantIdentifier": "tenant-0", "tenantDisplayName": "renamed"}}),
        ),
        (
            "both members at once",
            json!({
                "credential": {"oauth2Credential": {
                    "clientId": "rotated-client-id-2",
                    "clientSecret": "rotated-client-secret-2",
                }},
                "tenant": {"tenantIdentifier": "tenant-0", "tenantDisplayName": "renamed again"},
            }),
        ),
    ];

    for count in [1_usize, 2, 4] {
        let emulator = Emulator::new();
        let bundle = app_bundle(&emulator, REGION).await;
        let mut urls: Vec<String> = Vec::new();
        let mut first_arn = String::new();

        for index in 0..count {
            let created = create_authorization(
                &emulator,
                REGION,
                &bundle,
                "SLACK",
                "oauth2",
                &format!("tenant-{index}"),
            )
            .await;
            assert_eq!(
                created.status, 201,
                "case {count} authorizations, number {index}: expected HTTP 201, got {} with \
                 body {}",
                created.status, created.body
            );
            let arn = authorization_arn(&created);
            let expected = expected_auth_url(&bundle, &arn);
            let reported = auth_url(&created);
            assert_eq!(
                reported.as_deref(),
                Some(expected.as_str()),
                "case {count} authorizations, number {index}: expected {expected:?}, got \
                 {reported:?}"
            );
            if index == 0 {
                first_arn = arn;
            }
            urls.push(expected);
        }

        let distinct: BTreeSet<&String> = urls.iter().collect();
        assert_eq!(
            distinct.len(),
            urls.len(),
            "case {count} authorizations: every authorization must report its own address, got \
             {urls:?}"
        );

        // Every update of the first authorization reports the address the create
        // call established for it, and a read afterwards still does.
        let established = urls.first().expect("at least one authorization").clone();
        for (label, body) in updates {
            let case = format!("{count} authorizations, {label}");
            let updated =
                update_authorization(&emulator, REGION, &bundle, &first_arn, body.clone()).await;
            assert_eq!(
                updated.status, 200,
                "case {case:?}: expected HTTP 200, got {} with body {}",
                updated.status, updated.body
            );
            assert_eq!(
                auth_url(&updated).as_deref(),
                Some(established.as_str()),
                "case {case:?}: an update must report the established address {established:?}, \
                 got {:?}",
                updated.body
            );
            let read = get_authorization(&emulator, REGION, &bundle, &first_arn).await;
            assert_eq!(
                auth_url(&read).as_deref(),
                Some(established.as_str()),
                "case {case:?}: a read after the update must report {established:?}, got {}",
                read.body
            );
        }
    }
}

/// A fetched page: the status it was answered under, and the media type a page
/// served from this emulator's own origin announces.
fn assert_page(case: &str, fetched: &Fetched, status: u16) {
    assert_eq!(
        fetched.status, status,
        "case {case:?}: expected HTTP {status}, got {} with document {:?}",
        fetched.status, fetched.document
    );
    assert_eq!(
        fetched.content_type, OAUTH_CONSENT_MEDIA_TYPE,
        "case {case:?}: expected the page's media type {OAUTH_CONSENT_MEDIA_TYPE:?}, got {:?}",
        fetched.content_type
    );
}
