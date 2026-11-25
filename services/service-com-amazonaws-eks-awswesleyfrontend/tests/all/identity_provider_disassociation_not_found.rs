// SPDX-License-Identifier: CC0-1.0

//! The not-found answer of `DisassociateIdentityProviderConfig`, over a TABLE of ways
//! the resource a request names can be absent.
//!
//! The RULE under test: this operation addresses ONE resource — the
//! `(cluster, configuration)` pair — so a pair it cannot resolve is 404
//! `ResourceNotFoundException` with the service's single generic not-found wording
//! and NO identity member, whichever half is missing and however it came to be
//! missing. That is the same answer `DescribeIdentityProviderConfig` gives for the
//! same pair, which is what lets a client that disassociates a configuration and
//! then asks again read one 404 rather than two different ones.
//!
//! The rows differ along what the rule generalises over: which half of the pair is
//! absent (a cluster that was never created, a name the cluster never carried, a
//! name differing only in case, the pair signed for another region) and HOW it became
//! absent (never there, or disassociated by this very operation). One row holds the
//! pair that IS there, so the rule is about absence rather than about the shape of the
//! request.
//!
//! The harness is the shared one: `wire`'s request driver and the identity-provider
//! drivers and not-found assertion `identity_provider_config_not_found.rs` owns.

use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::REGION;
use crate::identity_provider_config_not_found::{
    active_cluster_in, assert_not_found, associate_in, describe_config_in,
};
use crate::wire::{call, emulator, encode_label, read_until, Answer};

/// `POST /clusters/{name}/identity-provider-configs/disassociate` for one
/// `(type, name)` pair.
async fn disassociate_in(
    router: &MultiServiceRouter,
    region: &str,
    cluster: &str,
    config_name: &str,
) -> Answer {
    let body = serde_json::json!({
        "identityProviderConfig": { "type": "oidc", "name": config_name },
    });
    call(
        router,
        region,
        "POST",
        &format!(
            "/clusters/{}/identity-provider-configs/disassociate",
            encode_label(cluster)
        ),
        &body.to_string(),
    )
    .await
}

#[tokio::test]
async fn every_absent_configuration_is_the_same_not_found_answer() {
    let router = emulator();

    // One cluster carries two configurations and one carries none, so "not found" is
    // never about an empty store; the same names exist in no other region.
    active_cluster_in(&router, REGION, "idp-away-holder").await;
    active_cluster_in(&router, REGION, "idp-away-empty").await;
    for config_name in ["kept-config", "removed-config"] {
        let associated = associate_in(&router, REGION, "idp-away-holder", config_name).await;
        assert_eq!(
            associated.status, 200,
            "setup {config_name:?}: the association must succeed, got {associated:?}"
        );
        read_until(
            config_name,
            "the clock owes the association its ACTIVE configuration",
            || describe_config_in(&router, REGION, "idp-away-holder", config_name),
            |answer| {
                answer.body["identityProviderConfig"]["oidc"]["status"]
                    == serde_json::json!("ACTIVE")
            },
        )
        .await;
    }

    // THE ROUND TRIP: one of them is disassociated through this very operation, and
    // once the clock has completed that update the configuration is gone — so asking
    // again addresses a pair that does not exist.
    let removed = disassociate_in(&router, REGION, "idp-away-holder", "removed-config").await;
    assert_eq!(
        removed.status, 200,
        "setup: the disassociation must be accepted, got {removed:?}"
    );
    read_until(
        "removed-config",
        "the disassociated configuration left the cluster",
        || describe_config_in(&router, REGION, "idp-away-holder", "removed-config"),
        |answer| answer.status == 404,
    )
    .await;

    // (case, region, cluster, configuration name)
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "a configuration this operation has already disassociated",
            REGION,
            "idp-away-holder",
            "removed-config",
        ),
        (
            "a cluster that was never created",
            REGION,
            "no-such-cluster",
            "kept-config",
        ),
        (
            "neither half exists",
            REGION,
            "no-such-cluster",
            "no-such-config",
        ),
        (
            "a known cluster that carries no configuration",
            REGION,
            "idp-away-empty",
            "kept-config",
        ),
        (
            "a known cluster, a name it never carried",
            REGION,
            "idp-away-holder",
            "absent-config",
        ),
        (
            "a name differing from the stored one only in case",
            REGION,
            "idp-away-holder",
            "KEPT-CONFIG",
        ),
        (
            "the right names, signed for another region",
            "eu-west-1",
            "idp-away-holder",
            "kept-config",
        ),
    ];

    for (case, region, cluster, config_name) in cases {
        let answer = disassociate_in(&router, region, cluster, config_name).await;
        assert_not_found(case, &answer, &[cluster, config_name]);
    }

    // The row that IS there: the same call disassociates the configuration the
    // cluster still carries, so the rows above are about the pair naming no resource
    // and not about the operation refusing every pair.
    let accepted = disassociate_in(&router, REGION, "idp-away-holder", "kept-config").await;
    assert_eq!(
        accepted.status, 200,
        "case the configuration that is there: expected 200, got {accepted:?}"
    );
    assert_eq!(
        accepted.body["update"]["type"],
        serde_json::json!("DisassociateIdentityProviderConfig"),
        "case the configuration that is there: expected that update type, got {accepted:?}"
    );
}
