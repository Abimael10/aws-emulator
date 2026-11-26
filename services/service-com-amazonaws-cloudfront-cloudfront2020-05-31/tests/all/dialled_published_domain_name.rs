// SPDX-License-Identifier: CC0-1.0

//! What a client holding ONLY a distribution's published `DomainName` can do with it.
//!
//! RULE: `Distribution$DomainName` is the address a VIEWER is handed — a client takes the value
//! a create or a read reports and fetches `https://<DomainName>/<object>` through it — so what
//! this emulator publishes there has to be an address it ANSWERS that distribution's edge on,
//! whatever address the create that minted the distribution was reached at. A value in a zone
//! Amazon Web Services publishes its own endpoints in resolves nowhere near the machine this
//! emulator runs on, so a client handed one cannot open a connection at all: not one viewer
//! request is served, and every operational control built on edge traffic — an error-rate
//! metric, an alarm over it, the notification that alarm publishes — then measures nothing while
//! the control plane reports the distribution deployed.
//!
//! So each row here creates a distribution, reads the `DomainName` it published, and DIALS THAT
//! VALUE: a socket opened to the published address itself, with nothing read out of this
//! emulator's records, stating the published value as the host it addressed — which is what a
//! client handed that value does. The answer has to come from THAT distribution's edge, carrying
//! that distribution's own content, and the value has to be the one address the emulator serves
//! its edge on (the listener the create opened for it) and the one every read reports.
//!
//! The rows span what the rule generalises over: the address the create was REACHED at — the
//! emulator's own endpoint as a loopback literal and as a loopback name, and this service's
//! published host in its global and its regional spelling, which is the caller that resolves
//! AWS's own hostnames — crossed with the operation that MINTED the distribution
//! (`CreateDistribution`, `CreateDistributionWithTags`, and `CopyDistribution`, which mints one
//! from another distribution's stored configuration). Each row names a `DefaultRootObject` of
//! its own — an object this service's own control-plane origin really serves — so the document
//! that comes back says WHICH distribution the dialled address resolved to.
//!
//! The harness is [`crate::distribution_lifecycle`]'s, the addressed hosts are
//! [`crate::announced_location_uri`]'s table, the tag element is
//! [`crate::distribution_tags`]'s and the viewer dial is [`crate::published_edge_address`]'s —
//! each reused rather than copied.

use std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use crate::announced_location_uri::addressed_hosts;
use crate::distribution_lifecycle::{
    awaited_deployment, call_from_host, edge_listener_authority, emulator_with_engine,
    get_distribution, is_published_edge_address, Answer, API_VERSION, HOST,
};
use crate::distribution_tags::tags_xml;
use crate::published_edge_address::dial_as;

/// The AWS-managed cache policy `Managed-CachingOptimized`, so every row's behaviour runs under
/// a policy the service itself holds rather than under a legacy `ForwardedValues` document.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// The viewer path every row fetches: the distribution's ROOT, which is the one request whose
/// origin object the `DefaultRootObject` a row names decides.
const VIEWER_ROOT: &str = "/";

/// The operation that minted a row's distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mint {
    /// `POST /2020-05-31/distribution`.
    Create,
    /// `POST /2020-05-31/distribution?WithTags`.
    CreateWithTags,
    /// `POST /2020-05-31/distribution/<primary>/copy`, which mints a distribution from another
    /// one's STORED configuration rather than from a submitted document.
    Copy,
}

/// One row: the address its create is reached at, the operation that mints it, and the object
/// the distribution it mints serves at its root.
struct Row {
    case: &'static str,
    /// Which row of [`addressed_hosts`]' table the create is addressed to.
    addressed: usize,
    mint: Mint,
    /// The `DefaultRootObject` this distribution names: a path this service's own control-plane
    /// origin serves, taken from that list operation's own `http.uri`
    /// (`smithy_query(query="operation", operation="ListStreamingDistributions,ListPublicKeys,\
    /// ListKeyGroups,ListOriginAccessControls,ListCloudFrontOriginAccessIdentities")`) without
    /// the leading separator a root object does not carry ("do not add a `/` before the object
    /// name").
    root_object: &'static str,
    /// The element the document that object answers with carries, which is what says the
    /// dialled address resolved to THIS distribution.
    document: &'static str,
}

/// The rows, spanning both the address the create was reached at and the minting operation:
/// every addressed host appears, and every minting operation appears more than once.
const ROWS: [Row; 6] = [
    Row {
        case: "CreateDistribution reached at the emulator's own loopback endpoint",
        addressed: 0,
        mint: Mint::Create,
        root_object: "2020-05-31/distribution",
        document: "DistributionList",
    },
    Row {
        case: "CreateDistributionWithTags reached at the same endpoint by its loopback name",
        addressed: 1,
        mint: Mint::CreateWithTags,
        root_object: "2020-05-31/streaming-distribution",
        document: "StreamingDistributionList",
    },
    Row {
        case: "CopyDistribution reached at this service's own global host",
        addressed: 2,
        mint: Mint::Copy,
        root_object: "2020-05-31/public-key",
        document: "PublicKeyList",
    },
    Row {
        case: "CreateDistribution reached at this service's host in a regional spelling",
        addressed: 3,
        mint: Mint::Create,
        root_object: "2020-05-31/key-group",
        document: "KeyGroupList",
    },
    Row {
        case: "CopyDistribution reached at the emulator's own loopback endpoint",
        addressed: 0,
        mint: Mint::Copy,
        root_object: "2020-05-31/origin-access-control",
        document: "OriginAccessControlList",
    },
    Row {
        case: "CreateDistributionWithTags reached at this service's own global host",
        addressed: 2,
        mint: Mint::CreateWithTags,
        root_object: "2020-05-31/origin-access-identity/cloudfront",
        document: "CloudFrontOriginAccessIdentityList",
    },
];

/// One `DistributionConfig`: one origin — this service's own control-plane host, which the edge
/// fetches in process — one default cache behaviour serving viewers either scheme, and the root
/// object this row is told apart by.
fn config_xml(root_object: &str, reference: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <DefaultRootObject>{root_object}</DefaultRootObject>\
         <Origins><Quantity>1</Quantity><Items>\
         <Origin><Id>primary</Id><DomainName>{HOST}</DomainName></Origin>\
         </Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_OPTIMIZED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>{reference}</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// The `CopyDistributionRequest` a copy row sends: the reference the copy is created under, and
/// the state it is created in — "a Boolean flag to specify the state of the staging distribution
/// when it's created", which has to be enabled for the copy to serve a viewer at all.
fn copy_xml(reference: &str) -> String {
    format!(
        "<CopyDistributionRequest xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Enabled>true</Enabled>\
         </CopyDistributionRequest>"
    )
}

/// The `Id` and the `DomainName` one minting answer reports.
fn minted(answer: &Answer, case: &str) -> (String, String) {
    assert_eq!(
        answer.status, 201,
        "case {case}: expected HTTP 201 from the minting call, got {} with {}",
        answer.status, answer.raw
    );
    let id = answer.root().find_text("Id").unwrap_or_default().to_string();
    let domain = answer
        .root()
        .find_text("DomainName")
        .unwrap_or_default()
        .to_string();
    assert!(
        !id.is_empty() && !domain.is_empty(),
        "case {case}: expected an Id and a DomainName from the minting call, got {}",
        answer.raw
    );
    (id, domain)
}

/// Mint one row's distribution, addressed at `host`, and report its `(Id, DomainName)`.
async fn mint_row(
    router: &MultiServiceRouter,
    row: &Row,
    host: &str,
    reference: &str,
) -> (String, String) {
    let case = row.case;
    let config = config_xml(row.root_object, reference);
    let answer = match row.mint {
        Mint::Create => {
            call_from_host(
                router,
                host,
                "POST",
                &format!("/{API_VERSION}/distribution"),
                Some(&config),
                &[],
            )
            .await
        }
        Mint::CreateWithTags => {
            let body = format!(
                "<DistributionConfigWithTags \
                 xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">{config}{}\
                 </DistributionConfigWithTags>",
                tags_xml(Some(&[(
                    "owner".to_string(),
                    Some("dialled-published-domain-name".to_string())
                )]))
            );
            call_from_host(
                router,
                host,
                "POST",
                &format!("/{API_VERSION}/distribution?WithTags"),
                Some(&body),
                &[],
            )
            .await
        }
        Mint::Copy => {
            // A copy runs under a PRIMARY's stored configuration, so the row creates one first,
            // addressed at the same host and under a reference of its own; the primary is a
            // distribution of its own, with an address of its own, and the copy inherits the
            // root object that tells this row apart.
            let primary = call_from_host(
                router,
                host,
                "POST",
                &format!("/{API_VERSION}/distribution"),
                Some(&config_xml(row.root_object, &format!("{reference}-primary"))),
                &[],
            )
            .await;
            let (primary_id, primary_domain) = minted(&primary, case);
            let copied = call_from_host(
                router,
                host,
                "POST",
                &format!("/{API_VERSION}/distribution/{primary_id}/copy"),
                Some(&copy_xml(reference)),
                &[],
            )
            .await;
            let (_, domain) = minted(&copied, case);
            assert_ne!(
                domain, primary_domain,
                "case {case}: expected the copy to publish an address of its own, got the \
                 primary's {primary_domain:?}"
            );
            copied
        }
    };
    minted(&answer, case)
}

/// A client that holds nothing but the value a distribution published as its `DomainName` dials
/// it and is served by THAT distribution's edge — whichever operation minted the distribution
/// and whichever address that call was reached at.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_holding_only_the_published_domain_name_is_served_by_that_distributions_edge() {
    let (router, engine) = emulator_with_engine();
    // Held for the whole test: the ports the own-endpoint rows address stay this process's own,
    // so no listener this engine opens can be given one of them.
    let hosts = addressed_hosts();

    // Every row's distribution is minted first, so the propagation each one is owed runs for all
    // of them at once rather than one row at a time.
    let mut published: Vec<(&Row, String, String)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let addressed = hosts
            .rows
            .get(row.addressed)
            .unwrap_or_else(|| panic!("case {}: the addressed host table has that row", row.case));
        let (id, domain) = mint_row(
            &router,
            row,
            &addressed.host,
            &format!("ref-dialled-published-{index}"),
        )
        .await;
        published.push((row, id, domain));
    }

    // The published value is an address, it is the ONE address this emulator serves that
    // distribution's edge on, and no two distributions published the same one.
    let mut addresses: BTreeSet<String> = BTreeSet::new();
    for (row, id, domain) in &published {
        let case = row.case;
        assert!(
            is_published_edge_address(domain),
            "case {case}: expected an address a client can dial (host:port), got {domain:?}"
        );
        let served_on = edge_listener_authority(&engine, case, id);
        assert_eq!(
            *domain, served_on,
            "case {case}: expected the published value to be the address the edge is served on \
             ({served_on:?}), got {domain:?}"
        );
        assert!(
            addresses.insert(domain.clone()),
            "case {case}: the address {domain:?} was already published by another distribution"
        );
    }

    for (row, id, domain) in &published {
        let case = row.case;
        awaited_deployment(&router, case, id).await;

        // The dial itself: a socket to the published value, stating it as the host it addressed,
        // asking for the distribution's root. Nothing but the published value is used to reach
        // it.
        let served = dial_as(domain, "GET", VIEWER_ROOT, domain).await;
        assert!(
            served.starts_with("HTTP/1.1 200"),
            "case {case}: expected the published address {domain:?} to serve the root object, \
             got {served:?}"
        );
        assert!(
            served
                .to_ascii_lowercase()
                .contains("x-cache: miss from cloudfront"),
            "case {case}: expected the answer from {domain:?} to come from the edge, got \
             {served:?}"
        );
        // WHICH distribution answered: the document is the one this row's own root object names,
        // and no other row's.
        assert!(
            served.contains(&format!("<{}", row.document)),
            "case {case}: expected the document {:?} this distribution's root object names from \
             {domain:?}, got {served:?}",
            row.document
        );
        for other in ROWS.iter().filter(|other| other.document != row.document) {
            assert!(
                !served.contains(&format!("<{}", other.document)),
                "case {case}: expected nothing of the distribution serving {:?} from {domain:?}, \
                 got {served:?}",
                other.document
            );
        }

        // And the value a later read reports is the same address, so a client that read it back
        // rather than keeping the minting answer dials the same place.
        let read = get_distribution(&router, id).await;
        let read_domain = read.root().find_text("DomainName").unwrap_or_default();
        assert_eq!(
            read_domain, domain,
            "case {case}: expected GetDistribution to report the published address {domain:?}, \
             got {read_domain:?}"
        );
    }
}
