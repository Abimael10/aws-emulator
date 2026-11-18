// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for HOW this service mints the opaque identifiers its resources are
//! addressed by.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness comes from [`super::domain_lifecycle`]
//! and every per-operation request helper from the sibling suites; this one adds no
//! second copy of either.
//!
//! RULE: an identifier this service generates for a resource is DRAWN for that resource
//! — not rendered from how many resources the store has already minted — so the nth
//! resource of a fresh store carries no constant identity, two stores answering the SAME
//! requests in the SAME order mint different identifiers, and every value derived from
//! one (the ARN over an application's id, the host over a domain's opaque suffix)
//! interpolates the identifier that was drawn.
//!
//! The tests assert that rule over sets of inputs rather than one recorded call:
//!
//! * the application id, over a table of creates differing in the shape and the length of
//!   the name (both published bounds, a hyphenated name and a digit-carrying one), each
//!   create repeated in THREE fresh stores in the same order — so neither the request nor
//!   the place in the store decides the identity — with the ARN and the `GetApplication`
//!   read of every one of them reporting the id that create answered with;
//! * the identifiers the control plane mints — a VPC-access domain's opaque host id, a
//!   service-managed VPC endpoint's `VpcEndpointId` and a cross-cluster connection's
//!   `ConnectionId` — each minted twice from identical requests in two fresh stores;
//! * the identifiers the application plane mints — a data source attachment's
//!   `attachmentId` and a saved-object migration job's `migrationId` — likewise.
//!
//! Each row also asserts the identifier against the constraint the model publishes for
//! it, so a drawn value is still a value the API admits.

use std::collections::BTreeMap;

use serde_json::json;
use service_com_amazonaws_opensearch_amazonopensearchservice::state::ENDPOINTS_KEY_VPC;

use super::application_plane_identity::is_application_id;
use super::application_updates::{create_application, get_application};
use super::cross_cluster_connections::{
    create_connection, created_domain, domain_info, is_connection_id,
};
use super::data_source_attachments::attach;
use super::data_source_kinds::domain_arn;
use super::domain_lifecycle::{poll_until, string_member, Emulator, REGION};
use super::saved_object_migrations::{active_application, application_with_source, start_migration};
use super::vpc_endpoints::{create_endpoint, member, minted as minted_endpoint, vpc_domain};

/// How many fresh stores the application-id rule is asserted across. Each one answers
/// the same creates in the same order, so an identity rendered from a store's creation
/// count would repeat across them.
const STORES: usize = 3;

/// The service token an APPLICATION's ARN carries — the OpenSearch user-interface
/// plane's own, which the identity rule interpolates the drawn id into.
const APPLICATION_ARN_TOKEN: &str = "opensearch";

/// The number of characters an application's `Id` carries.
///
/// `Id`'s own expression admits 3..30 (`smithy_query(query="shape",shape="Id")`), far
/// wider than the ONE width the service mints inside it: the id IS the host label the
/// application's endpoint is reached on, the form this account's OpenSearch collections
/// and their endpoints take too. Stated here independently of the constant the
/// implementation mints by, so this expectation does not move with it.
const APPLICATION_ID_WIDTH: usize = 20;

/// The prefix the host a VPC-access domain publishes carries, from
/// `DomainStatus$Endpoints`'s own example
/// `vpc-endpoint-h2dsd34efgyghrtguk5gt6j2foh4.us-east-1.es.amazonaws.com`.
const VPC_ACCESS_PREFIX: &str = "vpc-endpoint-";

/// The names the application-id rule is asserted over: the rows differ along the shape
/// and the length of the name — the 3-character lower bound `ApplicationName` publishes,
/// a hyphenated name, a digit-carrying one and the 30-character upper bound
/// (`smithy_query(query="shape",shape="ApplicationName")`).
fn application_names() -> Vec<(&'static str, &'static str)> {
    vec![
        ("the 3-character name bound", "ui1"),
        ("a hyphenated name", "drawn-identity-ui"),
        ("a digit-carrying name", "ui9x8y7"),
        ("the 30-character name bound", "abcdefghij-klmnopqrst-uvwxyz12"),
    ]
}

/// One identifier a resource was minted under, as a client sees it and as this service
/// drew it.
struct Minted {
    /// The identifier the wire carries.
    served: String,
    /// The part of it this service DREW: the whole identifier, or the opaque part behind
    /// the prefix the resource's own form carries.
    drawn: String,
}

/// Whether `token` is an opaque lowercase-alphanumeric token: the alphabet every opaque
/// part this service mints is rendered in, and a non-empty one.
fn is_opaque_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
}

/// Whether `id` satisfies `VpcEndpointId`'s own modelled constraint:
/// `@length(min: 5, max: 256)` with pattern `^aos-[a-zA-Z0-9]*$`
/// (`smithy_query(query="shape",shape="VpcEndpointId")`).
fn is_vpc_endpoint_id(id: &str) -> bool {
    (5..=256).contains(&id.len())
        && id
            .strip_prefix("aos-")
            .is_some_and(|body| body.chars().all(|character| character.is_ascii_alphanumeric()))
}

/// The kinds of identifier the CONTROL plane mints, each read where a client sees it.
#[derive(Debug, Clone, Copy)]
enum ControlPlaneKind {
    /// The opaque host id a VPC-access domain publishes in `DomainStatus$Endpoints`.
    VpcAccessHost,
    /// A service-managed VPC endpoint's `VpcEndpointId`.
    VpcEndpointId,
    /// A cross-cluster connection's `ConnectionId`.
    ConnectionId,
}

/// The kinds of identifier the APPLICATION plane mints.
#[derive(Debug, Clone, Copy)]
enum ApplicationPlaneKind {
    /// A data source attachment's `attachmentId`.
    AttachmentId,
    /// A saved-object migration job's `migrationId`.
    MigrationId,
}

/// The ARN of a search collection of this account: the source a saved-object migration
/// exports from, which is the shape `StartMigration` validates its source against. The
/// domain ARN an attach names comes from [`super::data_source_kinds::domain_arn`], this
/// crate's one composition of it.
fn collection_arn(name: &str) -> String {
    format!(
        "arn:aws:aoss:{REGION}:{}:collection/{name}",
        ::engine::emulator_account_id()
    )
}

/// Mint `kind` in a FRESH store from a request that names nothing drawn, and answer with
/// the identifier that store minted — asserted against the constraint the model
/// publishes for it.
async fn control_plane_identity(kind: ControlPlaneKind) -> Minted {
    let emulator = Emulator::new();
    let case = format!("{kind:?}");
    match kind {
        ControlPlaneKind::VpcAccessHost => {
            let name = "drawn-identity-vpc";
            vpc_domain(&emulator, name).await;
            // A domain still being provisioned publishes no address yet, so the CONDITION
            // — the hosts its access mode gives it having appeared — is polled for rather
            // than waited out.
            let looking = format!("{case}: the VPC-access domain publishing its hosts");
            poll_until(&looking, || async {
                emulator.describe(name).await.body["DomainStatus"]["Endpoints"]
                    [ENDPOINTS_KEY_VPC]
                    .is_string()
            })
            .await;
            let described = emulator.describe(name).await;
            assert_eq!(
                described.status, 200,
                "case {case}: expected 200 from DescribeDomain, got {} with {:?}",
                described.status, described.body
            );
            let host = described.body["DomainStatus"]["Endpoints"][ENDPOINTS_KEY_VPC]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {case}: expected a VPC-access host under {ENDPOINTS_KEY_VPC:?}, got \
                         {:?}",
                        described.body
                    )
                })
                .to_owned();
            let drawn = host
                .strip_prefix(VPC_ACCESS_PREFIX)
                .and_then(|rest| rest.split('.').next())
                .unwrap_or_default()
                .to_owned();
            assert!(
                is_opaque_token(&drawn),
                "case {case}: expected the host to carry an opaque lowercase token, got {host:?}"
            );
            Minted {
                served: host,
                drawn,
            }
        }
        ControlPlaneKind::VpcEndpointId => {
            let arn = vpc_domain(&emulator, "drawn-identity-endpoint").await;
            let answer = create_endpoint(
                &emulator,
                json!({
                    "DomainArn": arn,
                    "VpcOptions": { "SubnetIds": ["subnet-0777000000000001"] },
                }),
            )
            .await;
            let endpoint = minted_endpoint(&answer, &case);
            let id = member(&endpoint, "VpcEndpointId", &case);
            assert!(
                is_vpc_endpoint_id(&id),
                "case {case}: the minted VpcEndpointId must satisfy its modelled constraint, got \
                 {id:?}"
            );
            let drawn = id.trim_start_matches("aos-").to_owned();
            Minted { served: id, drawn }
        }
        ControlPlaneKind::ConnectionId => {
            created_domain(&emulator, "drawn-identity-local").await;
            created_domain(&emulator, "drawn-identity-remote").await;
            let answer = create_connection(
                &emulator,
                json!({
                    "LocalDomainInfo": domain_info("drawn-identity-local", None, None),
                    "RemoteDomainInfo": domain_info("drawn-identity-remote", None, None),
                    "ConnectionAlias": "drawn-identity",
                }),
            )
            .await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected 200 from CreateOutboundConnection, got {} with {:?}",
                answer.status, answer.body
            );
            let id = string_member(&answer.body, "ConnectionId", &case);
            assert!(
                is_connection_id(&id),
                "case {case}: the minted ConnectionId must satisfy its modelled constraint, got \
                 {id:?}"
            );
            let drawn = id.trim_start_matches("cc-").to_owned();
            Minted { served: id, drawn }
        }
    }
}

/// Mint `kind` in a FRESH store from a request that names nothing drawn, and answer with
/// the identifier that store minted.
async fn application_plane_identity(kind: ApplicationPlaneKind) -> Minted {
    let emulator = Emulator::new();
    let case = format!("{kind:?}");
    match kind {
        ApplicationPlaneKind::AttachmentId => {
            // An attach is taken on an ACTIVE application, so the condition is polled for
            // rather than waited out.
            let application = active_application(&emulator, "drawnattachment").await;
            let answer = attach(
                &emulator,
                &application,
                json!({ "dataSourceArn": domain_arn("drawn-identity-source") }),
            )
            .await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected 200 from AttachDataSource, got {} with {:?}",
                answer.status, answer.body
            );
            let id = string_member(&answer.body, "attachmentId", &case);
            // `attachmentId` is an opaque string the model constrains not at all
            // (`smithy_query(query="shape",shape="DataSourceAttachment")`), so what is
            // asserted of it is that it carries an opaque token at all.
            let drawn = id.rsplit('-').next().unwrap_or_default().to_owned();
            assert!(
                is_opaque_token(&drawn),
                "case {case}: expected the attachment id to carry an opaque lowercase token, got \
                 {id:?}"
            );
            Minted { served: id, drawn }
        }
        ApplicationPlaneKind::MigrationId => {
            let application = application_with_source(
                &emulator,
                "drawnmigration",
                &collection_arn("drawnsource"),
            )
            .await;
            let answer = start_migration(
                &emulator,
                json!({
                    "applicationId": application,
                    "migrationOptions": {
                        "source": { "datasourceArn": collection_arn("drawnsource") },
                        "workspace": { "workspaceId": "ws-drawn-identity" },
                    },
                }),
            )
            .await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected 200 from StartMigration, got {} with {:?}",
                answer.status, answer.body
            );
            let id = string_member(&answer.body, "migrationId", &case);
            // A job is read by its `migrationId` ALONE, so the id names the application
            // whose workspace it imports into and then an opaque part of its own.
            let drawn = id
                .strip_prefix(&format!("{application}-"))
                .unwrap_or_else(|| {
                    panic!(
                        "case {case}: expected the job id to name the application {application:?} \
                         it was started for, got {id:?}"
                    )
                })
                .to_owned();
            assert!(
                is_opaque_token(&drawn),
                "case {case}: expected the job id to carry an opaque lowercase token, got {id:?}"
            );
            Minted { served: id, drawn }
        }
    }
}

/// RULE: an application's id is drawn FOR the application, so the same creates made in the
/// same order against fresh stores mint different ids — and the ARN each create answers
/// with, and the read of that application, carry the id that was drawn for it.
#[tokio::test]
async fn an_application_id_is_drawn_for_the_application_rather_than_for_its_place_in_the_store() {
    let account = ::engine::emulator_account_id();
    // Every id any store of this test minted, against the store and the name it was
    // minted for — so a repeat names both sides of the collision.
    let mut held: BTreeMap<String, String> = BTreeMap::new();

    for store in 0..STORES {
        let emulator = Emulator::new();
        for (row, name) in application_names() {
            let case = format!("store {store}, {row}");
            let created = create_application(&emulator, json!({ "name": name })).await;
            assert_eq!(
                created.status, 200,
                "case {case}: expected 200 from CreateApplication({name:?}), got {} with {:?}",
                created.status, created.body
            );

            let id = string_member(&created.body, "id", &case);
            assert!(
                is_application_id(&id),
                "case {case}: expected an id inside the modelled expression, got {id:?}"
            );
            assert_eq!(
                id.chars().count(),
                APPLICATION_ID_WIDTH,
                "case {case}: expected an id of the width the service mints, got {id:?}"
            );

            // The ARN is built OVER the drawn id, so the identity one call mints is the
            // identity every later call resolves.
            let expected =
                format!("arn:aws:{APPLICATION_ARN_TOKEN}:{REGION}:{account}:application/{id}");
            let arn = string_member(&created.body, "arn", &case);
            assert_eq!(
                arn, expected,
                "case {case}: expected {expected:?}, got {arn:?}"
            );

            // Read back through the operation that reports the stored record: the create's
            // answer and the read cannot disagree about the identity.
            let read = get_application(&emulator, &id).await;
            assert_eq!(
                read.status, 200,
                "case {case}: expected 200 from GetApplication({id}), got {} with {:?}",
                read.status, read.body
            );
            assert_eq!(
                string_member(&read.body, "id", &case),
                id,
                "case {case}: expected the read to report the id the create minted, got {:?}",
                read.body
            );
            assert_eq!(
                string_member(&read.body, "arn", &case),
                arn,
                "case {case}: expected the read to report the ARN the create minted, got {:?}",
                read.body
            );

            let earlier = held.insert(id.clone(), case.clone());
            assert!(
                earlier.is_none(),
                "case {case}: expected an id no other application of any store holds, got {id:?} \
                 again (first minted for {earlier:?})"
            );
        }
    }

    assert_eq!(
        held.len(),
        STORES * application_names().len(),
        "case every create of every store: expected one distinct id per application, got {held:?}"
    );
}

/// RULE, over the identifiers the CONTROL plane mints: each is drawn for the resource it
/// names, so two fresh stores answering identical requests mint different ones.
#[tokio::test]
async fn the_control_plane_draws_the_identifier_of_every_resource_it_mints() {
    for kind in [
        ControlPlaneKind::VpcAccessHost,
        ControlPlaneKind::VpcEndpointId,
        ControlPlaneKind::ConnectionId,
    ] {
        let case = format!("{kind:?}");
        let one = control_plane_identity(kind).await;
        let other = control_plane_identity(kind).await;
        assert_ne!(
            one.drawn, other.drawn,
            "case {case}: expected a fresh store to draw an opaque part of its own, got \
             {:?} twice",
            one.drawn
        );
        assert_ne!(
            one.served, other.served,
            "case {case}: expected a fresh store to mint an identifier of its own, got {:?} twice",
            one.served
        );
    }
}

/// RULE, over the identifiers the APPLICATION plane mints: each is drawn for the record it
/// names, so two fresh stores answering identical requests mint different ones.
#[tokio::test]
async fn the_application_plane_draws_the_identifier_of_every_record_it_mints() {
    for kind in [
        ApplicationPlaneKind::AttachmentId,
        ApplicationPlaneKind::MigrationId,
    ] {
        let case = format!("{kind:?}");
        let one = application_plane_identity(kind).await;
        let other = application_plane_identity(kind).await;
        assert_ne!(
            one.drawn, other.drawn,
            "case {case}: expected a fresh store to draw an opaque part of its own, got \
             {:?} twice",
            one.drawn
        );
        assert_ne!(
            one.served, other.served,
            "case {case}: expected a fresh store to mint an identifier of its own, got {:?} twice",
            one.served
        );
    }
}
