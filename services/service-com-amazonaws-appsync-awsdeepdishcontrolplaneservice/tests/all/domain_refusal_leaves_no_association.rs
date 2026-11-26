// SPDX-License-Identifier: CC0-1.0

//! The WHOLE custom-domain leg in one sequence: the create, the per-domain reads, the
//! account's listing and the three API-association calls, around a certificate AppSync will
//! not serve a custom domain with.
//!
//! RULE: a `CreateDomainName` AppSync refuses because the certificate the request names could
//! not terminate the HTTPS handshake the requested name would answer stores NOTHING — so the
//! account holds no custom domain of that name, and every operation that addresses one by name
//! answers the absent-domain refusal its own model declares (`NotFoundException`, HTTP 404,
//! `smithy_query(query="operation",operation="GetDomainName,AssociateApi,…")`) with AppSync's
//! wording for that operation's class, the account's listing holds no entry for it, and no API
//! can be mapped onto it. The same name under a certificate whose chain ends in a public trust
//! anchor is created and does carry an association, so no row rests on a blanket refusal.
//!
//! The rows span, independently:
//!
//! * the PROVENANCE of the certificate the refused create names — imported into Certificate
//!   Manager under an authority of the caller's own making (offered with that authority, with
//!   the whole chain to it, and with no chain at all) and uploaded to Identity and Access
//!   Management as a server certificate, the two services `certificateArn` admits a
//!   certificate of;
//! * the FORM of the name the create asks for — the certificate's own name, one of its subject
//!   alternative names, and a name one label under a wildcard;
//! * and the OPERATION that addresses the name the refusal left absent — the two reads, the
//!   update, the delete and the three association calls, which do not word the condition
//!   alike.
//!
//! Every certificate here is real material the owning service really holds, created through
//! that service's own operation over the same engine, and the API an association names is one
//! `CreateGraphqlApi` really minted. The suites that vary ONE dimension of the certificate
//! decision are `crate::domain_certificate_public_trust` (where the chain ends),
//! `crate::domain_certificate_provenance` (which service issued it) and
//! `crate::domain_certificate_coverage` (which names it covers); this one asserts what the
//! refusal leaves behind across the whole surface that addresses a custom domain.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::EmulatedService;

use crate::domain_certificate_issuing_authority::uploaded_material;
use crate::domain_certificate_provenance::{
    appsync_call, create_domain, message, requested, CERTIFICATE_INVALID, DOMAIN_NAMES_PATH,
    DOMAIN_NOT_FOUND_ON_CHANGE, DOMAIN_NOT_FOUND_ON_READ,
};
use crate::domain_certificate_public_trust::{imported_material, listed_domains, Authority};

/// The error shape every one of those operations models for it, at its modelled status.
/// Source: `smithy_query(query="operation",operation="GetDomainName")` and its siblings.
const NOT_FOUND_EXCEPTION: &str = "NotFoundException";
const NOT_FOUND_STATUS: u16 = 404;

/// The error shape and status `CreateDomainName` models for a certificate it will not serve
/// with. Source: `smithy_query(query="operation",operation="CreateDomainName")`.
const BAD_REQUEST_EXCEPTION: &str = "BadRequestException";
const BAD_REQUEST_STATUS: u16 = 400;

/// The names the rows create their domains under: one name space per row, so no row's domain
/// can be the one another row asked for.
const ROOT_SIGNED_NAME: &str = "root-signed.appsync-lifecycle-test.com";
const DEEP_SUBJECT_NAME: &str = "deep-subject.appsync-lifecycle-test.com";
const DEEP_ALTERNATIVE_NAME: &str = "deep-alternative.appsync-lifecycle-test.com";
const NO_CHAIN_NAME: &str = "no-chain.appsync-lifecycle-test.com";
const WILDCARD_APEX: &str = "wild.appsync-lifecycle-test.com";
const UPLOADED_NAME: &str = "uploaded.appsync-lifecycle-test.com";
const SERVED_NAME: &str = "served.appsync-lifecycle-test.com";

/// The server certificate name the account's upload is held under, a
/// `serverCertificateNameType` value that is also admissible inside `certificateArn`'s pattern.
const UPLOADED_CERTIFICATE_NAME: &str = "appsync-lifecycle-uploaded";

/// This service beside the two that hold certificates for an account, over ONE engine, so the
/// certificate every create names is the certificate the owning service really holds.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_acm_certificatemanager::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// `GetDomainName` / `UpdateDomainName` / `DeleteDomainName`'s path for `domain_name`.
fn domain_path(domain_name: &str) -> String {
    format!("{DOMAIN_NAMES_PATH}/{domain_name}")
}

/// The path the three association calls share.
fn association_path(domain_name: &str) -> String {
    format!("{DOMAIN_NAMES_PATH}/{domain_name}/apiassociation")
}

/// Create one GraphQL API and answer the `apiId` it was minted under: the API an association
/// names has to be one this service really holds.
async fn created_api(router: &MultiServiceRouter, name: &str) -> String {
    let answer = appsync_call(
        router,
        "POST",
        "/v1/apis",
        Some(json!({"name": name, "authenticationType": "API_KEY"})),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case CreateGraphqlApi({name}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["graphqlApi"]["apiId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case CreateGraphqlApi({name}): expected an apiId, got {:?}", answer.body)
        })
        .to_string()
}

/// One call that ADDRESSES a custom domain by name: how it is sent, and the wording AppSync
/// states the absent domain with on it.
struct Addressed {
    label: &'static str,
    method: &'static str,
    path: fn(&str) -> String,
    /// The body the call carries, built from the API an association names.
    body: fn(&str) -> Option<Value>,
    absent_text: &'static str,
}

/// Every operation that addresses one custom domain by name, each with its own wording.
fn addressed() -> Vec<Addressed> {
    vec![
        Addressed {
            label: "GetDomainName",
            method: "GET",
            path: domain_path,
            body: |_| None,
            absent_text: DOMAIN_NOT_FOUND_ON_READ,
        },
        Addressed {
            label: "UpdateDomainName",
            method: "POST",
            path: domain_path,
            body: |_| Some(json!({"description": "a description of a domain nothing holds"})),
            absent_text: DOMAIN_NOT_FOUND_ON_READ,
        },
        Addressed {
            label: "GetApiAssociation",
            method: "GET",
            path: association_path,
            body: |_| None,
            absent_text: DOMAIN_NOT_FOUND_ON_READ,
        },
        Addressed {
            label: "AssociateApi",
            method: "POST",
            path: association_path,
            body: |api_id| Some(json!({"apiId": api_id})),
            absent_text: DOMAIN_NOT_FOUND_ON_CHANGE,
        },
        Addressed {
            label: "DisassociateApi",
            method: "DELETE",
            path: association_path,
            body: |_| None,
            absent_text: DOMAIN_NOT_FOUND_ON_CHANGE,
        },
        Addressed {
            label: "DeleteDomainName",
            method: "DELETE",
            path: domain_path,
            body: |_| None,
            absent_text: DOMAIN_NOT_FOUND_ON_CHANGE,
        },
    ]
}

/// One refused row: the name the create asks for, and the certificate it names.
struct Row {
    label: &'static str,
    domain_name: String,
    certificate_arn: String,
}

/// Every refused row, with each certificate created through the service that holds it.
async fn rows(router: &MultiServiceRouter) -> Vec<Row> {
    let root = Authority::root("appsync-lifecycle-test local root authority");
    let intermediate = root.subordinate("appsync-lifecycle-test local intermediate authority");
    let deep_chain = format!("{}{}", intermediate.pem(), root.pem());
    vec![
        Row {
            label: "an imported certificate a local root authority signed, under its own name",
            domain_name: ROOT_SIGNED_NAME.to_string(),
            certificate_arn: imported_material(
                router,
                "import a root-signed certificate",
                root.signs(&[ROOT_SIGNED_NAME]),
                Some(root.pem()),
            )
            .await,
        },
        Row {
            label: "an imported certificate a local intermediate authority signed, under a \
                    subject alternative name of it",
            domain_name: DEEP_ALTERNATIVE_NAME.to_string(),
            certificate_arn: imported_material(
                router,
                "import an intermediate-signed certificate",
                intermediate.signs(&[DEEP_SUBJECT_NAME, DEEP_ALTERNATIVE_NAME]),
                Some(deep_chain),
            )
            .await,
        },
        Row {
            label: "an imported certificate offered with no chain at all",
            domain_name: NO_CHAIN_NAME.to_string(),
            certificate_arn: imported_material(
                router,
                "import a root-signed certificate with no chain",
                root.signs(&[NO_CHAIN_NAME]),
                None,
            )
            .await,
        },
        Row {
            label: "one label under an imported wildcard certificate",
            domain_name: format!("api.{WILDCARD_APEX}"),
            certificate_arn: imported_material(
                router,
                "import a root-signed wildcard certificate",
                root.signs(&[&format!("*.{WILDCARD_APEX}")]),
                Some(root.pem()),
            )
            .await,
        },
        Row {
            label: "an uploaded server certificate a local root authority signed",
            domain_name: UPLOADED_NAME.to_string(),
            certificate_arn: uploaded_material(
                router,
                UPLOADED_CERTIFICATE_NAME,
                root.signs(&[UPLOADED_NAME]),
                Some(root.pem()),
            )
            .await,
        },
    ]
}

/// RULE: the create AppSync refuses for its certificate stores nothing, so every operation
/// that addresses that name answers the absent-domain refusal its model declares and the
/// account's listing holds no entry for it.
#[tokio::test]
async fn a_refused_create_leaves_no_domain_for_any_operation_that_addresses_one() {
    let router = router();
    let api = created_api(&router, "appsync-lifecycle-refused").await;

    for row in rows(&router).await {
        let refused = create_domain(&router, &row.domain_name, &row.certificate_arn).await;
        assert_eq!(
            (
                refused.status,
                refused.error_type.as_str(),
                message(&refused)
            ),
            (BAD_REQUEST_STATUS, BAD_REQUEST_EXCEPTION, CERTIFICATE_INVALID),
            "case {}: expected the modelled certificate refusal, got {} {:?} with {:?}",
            row.label,
            refused.status,
            refused.error_type,
            refused.body
        );

        for call in addressed() {
            let answer = appsync_call(
                &router,
                call.method,
                &(call.path)(&row.domain_name),
                (call.body)(&api),
            )
            .await;
            assert_eq!(
                (answer.status, answer.error_type.as_str(), message(&answer)),
                (NOT_FOUND_STATUS, NOT_FOUND_EXCEPTION, call.absent_text),
                "case {} / {}: expected the modelled absent-domain refusal, got {} {:?} with \
                 {:?}",
                row.label,
                call.label,
                answer.status,
                answer.error_type,
                answer.body
            );
        }
    }

    let listed = listed_domains(&router).await;
    assert!(
        listed.is_empty(),
        "case ListDomainNames: expected a refused create to leave nothing to list, got \
         {listed:?}"
    );
}

/// RULE, read the other way round: the refusal is the CERTIFICATE's, not the name's — the same
/// surface serves a name whose certificate ends in a public trust anchor, and the API really
/// maps onto it.
#[tokio::test]
async fn a_publicly_anchored_certificate_makes_the_same_surface_serve_the_name() {
    let router = router();
    let api = created_api(&router, "appsync-lifecycle-served").await;
    let root = Authority::root("appsync-lifecycle-test rotation authority");

    // The same name, first under a certificate of the caller's own authority.
    let brought_in = imported_material(
        &router,
        "import a root-signed certificate for the served name",
        root.signs(&[SERVED_NAME]),
        Some(root.pem()),
    )
    .await;
    let refused = create_domain(&router, SERVED_NAME, &brought_in).await;
    assert_eq!(
        (refused.status, message(&refused)),
        (BAD_REQUEST_STATUS, CERTIFICATE_INVALID),
        "case \"a certificate a local authority signed\": expected the modelled refusal, got {} \
         with {:?}",
        refused.status,
        refused.body
    );

    // And then under one Certificate Manager issued from its own public authority.
    let issued = requested(&router, &[SERVED_NAME]).await;
    let created = create_domain(&router, SERVED_NAME, &issued).await;
    assert_eq!(
        created.status, 200,
        "case \"a publicly issued certificate\": expected 200, got {} with {:?}",
        created.status, created.body
    );
    assert_eq!(
        created.body["domainNameConfig"]["certificateArn"].as_str(),
        Some(issued.as_str()),
        "case \"a publicly issued certificate\": expected the domain to report the certificate \
         it was created with, got {:?}",
        created.body
    );

    let associated = appsync_call(
        &router,
        "POST",
        &association_path(SERVED_NAME),
        Some(json!({"apiId": api})),
    )
    .await;
    assert_eq!(
        associated.status, 200,
        "case \"associate the served name\": expected 200, got {} with {:?}",
        associated.status, associated.body
    );
    assert_eq!(
        associated.body["apiAssociation"]["apiId"].as_str(),
        Some(api.as_str()),
        "case \"associate the served name\": expected the association to name the API, got {:?}",
        associated.body
    );

    // Read back through the service's own reader: the association is the store's, not the
    // create's answer.
    let read = appsync_call(&router, "GET", &association_path(SERVED_NAME), None).await;
    assert_eq!(
        read.status, 200,
        "case \"read the association back\": expected 200, got {} with {:?}",
        read.status, read.body
    );
    assert_eq!(
        read.body["apiAssociation"]["apiId"].as_str(),
        Some(api.as_str()),
        "case \"read the association back\": expected the stored association to name the API, \
         got {:?}",
        read.body
    );
    assert_eq!(
        read.body["apiAssociation"]["domainName"].as_str(),
        Some(SERVED_NAME),
        "case \"read the association back\": expected the association to name the domain, got \
         {:?}",
        read.body
    );

    let listed = listed_domains(&router).await;
    assert_eq!(
        listed,
        vec![SERVED_NAME.to_string()],
        "case ListDomainNames: expected exactly the served name, got {listed:?}"
    );
}
