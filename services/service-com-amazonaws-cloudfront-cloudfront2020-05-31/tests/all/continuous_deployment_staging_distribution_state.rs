// SPDX-License-Identifier: CC0-1.0

//! The STATE of the staging distribution a continuous deployment policy names, end to end
//! over HTTP.
//!
//! The rule: a continuous deployment policy moves production traffic to the staging
//! distribution its `StagingDistributionDnsNames` names, and
//! `DistributionConfig$Enabled` is "whether the distribution is enabled to accept user
//! requests for content" — so a distribution that is disabled accepts none, a policy over it
//! could never route a request, and the service refuses to store one. The refusal is the
//! `InvalidArgument` (HTTP 400) the operation models, carrying the sentence CloudFront states
//! for the condition, and nothing is created.
//!
//! Checked on every payload that states a policy configuration — the create and the update —
//! because the condition is a property of the configuration rather than of one call.
//!
//! The rows differ along what the rule generalises over: whether the named distribution is
//! enabled or disabled, HOW MANY names a policy carries and which of them is the disabled
//! one, and which traffic configuration the policy carries. The harness (the router, the
//! request helper, the element scanner and the policy document writer) is the sibling suites',
//! reused rather than copied.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    DISABLED_STAGING_DISTRIBUTION_MESSAGE, FOREIGN_STAGING_DISTRIBUTION_DOMAIN_MESSAGE,
};

use crate::continuous_deployment_policies::{create_policy, get_policy, Policy, Traffic};
use crate::continuous_deployment_policy_update::update_policy;
use crate::distribution_lifecycle::{
    call, config as distribution_config, create_distribution, emulator, API_VERSION,
};

/// Create one distribution, enabled or not, and report the domain name CloudFront assigned
/// it — the name a policy's `StagingDistributionDnsNames` carries.
async fn distribution_domain(
    router: &MultiServiceRouter,
    reference: &str,
    enabled: bool,
) -> String {
    let mut config = distribution_config("staging", reference);
    config.enabled = enabled;
    let created = create_distribution(router, &config.xml()).await;
    assert_eq!(
        created.status, 201,
        "case the {} distribution {reference}: expected HTTP 201 from CreateDistribution, got \
         {} with {}",
        if enabled { "enabled" } else { "disabled" },
        created.status,
        created.raw
    );
    let domain = created
        .root()
        .text_at("DomainName")
        .unwrap_or_default()
        .to_string();
    assert!(
        !domain.is_empty(),
        "case the distribution {reference}: expected an assigned domain name, got {}",
        created.raw
    );
    domain
}

/// How many policies `ListContinuousDeploymentPolicies` reports.
async fn stored_policy_count(router: &MultiServiceRouter) -> usize {
    let listed = call(
        router,
        "GET",
        &format!("/{API_VERSION}/continuous-deployment-policy"),
        None,
        None,
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "expected HTTP 200 from ListContinuousDeploymentPolicies, got {} with {}",
        listed.status, listed.raw
    );
    listed
        .root()
        .children_named("Items")
        .first()
        .map(|items| items.children_named("ContinuousDeploymentPolicySummary").len())
        .unwrap_or_default()
}

/// The traffic configurations the rows below are run over: each kind a policy may carry, so
/// the state of the staging distribution is what decides the answer rather than the shape of
/// the traffic configuration.
fn traffic_rows() -> Vec<(&'static str, Traffic)> {
    vec![
        ("no traffic configuration", Traffic::None),
        (
            "a SingleWeight configuration",
            Traffic::SingleWeight {
                weight: 0.1,
                stickiness: None,
            },
        ),
        (
            "a SingleHeader configuration",
            Traffic::SingleHeader {
                header: "aws-cf-cd-slice".to_string(),
                value: "staging".to_string(),
            },
        ),
    ]
}

/// A create naming a DISABLED staging distribution is refused with `InvalidArgument` and the
/// sentence CloudFront states for the condition, and no policy is stored; the identical call
/// over an ENABLED distribution is accepted.
#[tokio::test]
async fn a_policy_over_a_disabled_staging_distribution_is_refused() {
    let router = emulator();

    for (index, (case, traffic)) in traffic_rows().into_iter().enumerate() {
        let before = stored_policy_count(&router).await;
        let disabled =
            distribution_domain(&router, &format!("cd-disabled-{index}"), false).await;
        let refused = create_policy(
            &router,
            &Policy {
                case: "a disabled staging distribution",
                dns_names: vec![disabled.clone()],
                enabled: true,
                traffic: traffic.clone(),
            }
            .xml(),
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {case}: expected HTTP 400 over a disabled staging distribution, got {} with \
             {}",
            refused.status, refused.raw
        );
        assert_eq!(
            refused.error_code(),
            "InvalidArgument",
            "case {case}: expected InvalidArgument, got {:?} in {}",
            refused.error_code(),
            refused.raw
        );
        assert_eq!(
            refused.root().find_text("Message"),
            Some(DISABLED_STAGING_DISTRIBUTION_MESSAGE),
            "case {case}: expected the refusal sentence {DISABLED_STAGING_DISTRIBUTION_MESSAGE:?}, \
             got {:?}",
            refused.root().find_text("Message")
        );
        assert_eq!(
            stored_policy_count(&router).await,
            before,
            "case {case}: the refused create stored a policy"
        );

        // The SAME call over an enabled distribution is the one CloudFront accepts, which is
        // what makes the staging distribution's state the thing deciding.
        let enabled = distribution_domain(&router, &format!("cd-enabled-{index}"), true).await;
        let created = create_policy(
            &router,
            &Policy {
                case: "an enabled staging distribution",
                dns_names: vec![enabled.clone()],
                enabled: true,
                traffic: traffic.clone(),
            }
            .xml(),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected HTTP 201 over an enabled staging distribution, got {} with \
             {}",
            created.status, created.raw
        );
        assert_eq!(
            stored_policy_count(&router).await,
            before + 1,
            "case {case}: the accepted create stored no policy"
        );
    }
}

/// The check is over EVERY name a policy carries, and it is read after the resolution of the
/// name itself: a list carrying one disabled distribution beside enabled ones is refused
/// wherever the disabled one stands, while a name no distribution of this account answers to
/// is still refused as the name of no staging distribution.
#[tokio::test]
async fn every_named_staging_distribution_is_resolved_and_read() {
    let router = emulator();
    let enabled_first = distribution_domain(&router, "cd-multi-enabled-1", true).await;
    let enabled_second = distribution_domain(&router, "cd-multi-enabled-2", true).await;
    let disabled = distribution_domain(&router, "cd-multi-disabled", false).await;

    // (case, the names the policy carries, the sentence the refusal states or `None` for a
    // configuration the service stores).
    let rows: Vec<(&str, Vec<String>, Option<&str>)> = vec![
        (
            "the disabled name first",
            vec![disabled.clone(), enabled_first.clone()],
            Some(DISABLED_STAGING_DISTRIBUTION_MESSAGE),
        ),
        (
            "the disabled name last",
            vec![enabled_first.clone(), disabled.clone()],
            Some(DISABLED_STAGING_DISTRIBUTION_MESSAGE),
        ),
        (
            "a name no distribution of this account answers to",
            vec!["d111111abcdef8.cloudfront.net".to_string()],
            Some(FOREIGN_STAGING_DISTRIBUTION_DOMAIN_MESSAGE),
        ),
        (
            "enabled names only",
            vec![enabled_first.clone(), enabled_second.clone()],
            None,
        ),
    ];

    for (case, dns_names, refusal) in rows {
        let before = stored_policy_count(&router).await;
        let answer = create_policy(
            &router,
            &Policy {
                case: "the names a policy carries",
                dns_names,
                enabled: true,
                traffic: Traffic::None,
            }
            .xml(),
        )
        .await;
        match refusal {
            Some(message) => {
                assert_eq!(
                    (answer.status, answer.error_code()),
                    (400, "InvalidArgument"),
                    "case {case}: expected a 400 InvalidArgument, got {} / {:?} with {}",
                    answer.status,
                    answer.error_code(),
                    answer.raw
                );
                assert_eq!(
                    answer.root().find_text("Message"),
                    Some(message),
                    "case {case}: expected the sentence {message:?}, got {:?}",
                    answer.root().find_text("Message")
                );
                assert_eq!(
                    stored_policy_count(&router).await,
                    before,
                    "case {case}: the refused create stored a policy"
                );
            }
            None => {
                assert_eq!(
                    answer.status, 201,
                    "case {case}: expected HTTP 201, got {} with {}",
                    answer.status, answer.raw
                );
                assert_eq!(
                    stored_policy_count(&router).await,
                    before + 1,
                    "case {case}: the accepted create stored no policy"
                );
            }
        }
    }
}

/// An UPDATE is held to the same rule as the create, and the policy it would have moved is
/// left exactly as it was: the configuration a read reports afterwards is the one the create
/// stored, at the version the create handed back.
#[tokio::test]
async fn an_update_onto_a_disabled_staging_distribution_is_refused() {
    let router = emulator();
    let enabled = distribution_domain(&router, "cd-update-enabled", true).await;
    let disabled = distribution_domain(&router, "cd-update-disabled", false).await;

    let stored = Policy {
        case: "the stored policy",
        dns_names: vec![enabled.clone()],
        enabled: true,
        traffic: Traffic::SingleHeader {
            header: "aws-cf-cd-slice".to_string(),
            value: "staging".to_string(),
        },
    };
    let created = create_policy(&router, &stored.xml()).await;
    assert_eq!(
        created.status, 201,
        "expected HTTP 201 from CreateContinuousDeploymentPolicy, got {} with {}",
        created.status, created.raw
    );
    let id = created
        .root()
        .find_text("Id")
        .expect("the created policy reports an id")
        .to_string();
    let etag = created.header("etag").to_string();

    let moved = Policy {
        case: "moved onto a disabled staging distribution",
        dns_names: vec![disabled.clone()],
        enabled: true,
        traffic: stored.traffic.clone(),
    };
    let refused = update_policy(&router, &id, &moved.xml(), Some(&etag)).await;
    assert_eq!(
        (refused.status, refused.error_code()),
        (400, "InvalidArgument"),
        "expected a 400 InvalidArgument from the update, got {} / {:?} with {}",
        refused.status,
        refused.error_code(),
        refused.raw
    );
    assert_eq!(
        refused.root().find_text("Message"),
        Some(DISABLED_STAGING_DISTRIBUTION_MESSAGE),
        "expected the refusal sentence {DISABLED_STAGING_DISTRIBUTION_MESSAGE:?}, got {:?}",
        refused.root().find_text("Message")
    );

    let read = get_policy(&router, &id).await;
    assert_eq!(
        read.status, 200,
        "expected HTTP 200 from GetContinuousDeploymentPolicy, got {} with {}",
        read.status, read.raw
    );
    assert_eq!(
        read.header("etag"),
        etag,
        "the refused update minted a new version: {}",
        read.raw
    );
    let config = read
        .root()
        .at("ContinuousDeploymentPolicyConfig")
        .expect("the read reports the stored configuration");
    stored.assert_matches(config, "after the refused update");
}
