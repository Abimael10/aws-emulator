// SPDX-License-Identifier: CC0-1.0

//! The DEPLOYMENT a created or reconfigured distribution is owed, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. The rule under test is the one `Distribution$Status` states: "when the status
//! is `Deployed`, the distribution's information is fully propagated to all CloudFront edge
//! locations", so a configuration that has just changed is NOT yet propagated and the clock
//! owes it that propagation:
//!
//! * the call that creates or replaces a configuration answers with the TRANSIENT status, so a
//!   client acting straight after it still observes the propagation it is waiting on;
//! * and that status ADVANCES on its own: a client that polls `GetDistribution` — the read the
//!   SDK's own distribution-deployed waiter polls — observes `Deployed` without making any
//!   further call, because the transition is one the CLOCK owes the record rather than one
//!   another request performs;
//! * reading it is not what performs it: the version token the mutation minted is the one the
//!   deployed record reports, so no poll re-versions the distribution.
//!
//! The rows differ along what the rule generalises over: WHICH call moved the distribution —
//! the two creates, the copy that makes a staging distribution, the update that replaces a
//! configuration, and the promotion that moves a staging configuration onto the primary — and,
//! for the promotion, over BOTH distributions of the continuous deployment it ends. Those are
//! every call of this service that stores a distribution configuration.
//!
//! The harness (the router, the request helpers, the configuration builder and the poll that
//! waits for a state) is [`crate::distribution_lifecycle`]'s, reused rather than copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::continuous_deployment_policies::create_policy;
use crate::distribution_copy::copy_distribution;
use crate::distribution_lifecycle::{
    awaited_deployment, call_with_headers, config, create_distribution, emulator, get_distribution,
    update_distribution, Answer, Config, API_VERSION, DEPLOYED,
};

/// The status a distribution whose configuration has not reached the edge locations yet
/// reports — a LITERAL here rather than the crate's own constant, so a constant edited to some
/// other value fails these rows.
const IN_PROGRESS: &str = "InProgress";

/// Which call moved the distribution this row reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mutation {
    /// `POST /2020-05-31/distribution`.
    Create,
    /// `POST /2020-05-31/distribution?WithTags`.
    CreateWithTags,
    /// `POST /2020-05-31/distribution/{Id}/copy` with `Staging: true` — how the continuous
    /// deployment workflow makes a staging distribution.
    CopyAsStaging,
    /// `PUT /2020-05-31/distribution/{Id}/config`.
    Update,
    /// `PUT /2020-05-31/distribution/{Id}/promote-staging-config`.
    Promote,
}

/// Every call that stores a distribution configuration.
const MUTATIONS: [Mutation; 5] = [
    Mutation::Create,
    Mutation::CreateWithTags,
    Mutation::CopyAsStaging,
    Mutation::Update,
    Mutation::Promote,
];

/// `POST /2020-05-31/distribution?WithTags`, whose payload wraps a `DistributionConfig` in a
/// `DistributionConfigWithTags`.
async fn create_distribution_with_tags(router: &MultiServiceRouter, row: &Config) -> Answer {
    let body = format!(
        "<DistributionConfigWithTags \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">{}<Tags/>\
         </DistributionConfigWithTags>",
        row.xml()
    );
    call_with_headers(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution?WithTags"),
        Some(&body),
        &[],
    )
    .await
}

/// A `ContinuousDeploymentPolicyConfig` routing traffic to `dns_name` under the single-header
/// traffic configuration, whose `Header` "must begin with `aws-cf-cd-`"
/// (`SingleHeaderConfig$Header`).
fn policy_xml(dns_name: &str) -> String {
    format!(
        "<ContinuousDeploymentPolicyConfig \
          xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <StagingDistributionDnsNames><Quantity>1</Quantity>\
         <Items><DnsName>{dns_name}</DnsName></Items></StagingDistributionDnsNames>\
         <Enabled>true</Enabled>\
         <TrafficConfig><SingleHeaderConfig><Header>aws-cf-cd-staging</Header>\
         <Value>true</Value></SingleHeaderConfig><Type>SingleHeader</Type></TrafficConfig>\
         </ContinuousDeploymentPolicyConfig>"
    )
}

/// `row.update_xml()` with `extra` added before the closing element.
fn with_members(row: &Config, extra: &str) -> String {
    row.update_xml()
        .replace("</DistributionConfig>", &format!("{extra}</DistributionConfig>"))
}

/// The id and the current version token of one distribution.
///
/// `pub(crate)` so a sibling suite about the same rule reuses this staging rather than
/// standing up a second copy of it.
pub(crate) struct Stored {
    pub(crate) id: String,
    pub(crate) etag: String,
}

/// Create one distribution and report what it was minted as, having waited out the deployment
/// its create owes it: a row that then MUTATES the distribution starts from a deployed one, so
/// the status it reads afterwards is the one its own call produced.
///
/// `pub(crate)`: see [`Stored`].
pub(crate) async fn deployed(router: &MultiServiceRouter, case: &str, row: &Config) -> Stored {
    let created = create_distribution(router, &row.xml()).await;
    assert_eq!(
        created.status, 201,
        "case {case}: expected HTTP 201 from CreateDistribution, got {} with {}",
        created.status, created.raw
    );
    let id = created.root().text_at("Id").unwrap_or_default().to_string();
    awaited_deployment(router, case, &id).await;
    let read = get_distribution(router, &id).await;
    Stored {
        id,
        etag: read.header("etag").to_string(),
    }
}

/// The continuous deployment a promotion ends: a deployed primary, a staging copy of it, a
/// policy routing to that copy, and the primary updated to name the policy. Reports the
/// `(primary, staging)` pair at their current versions.
///
/// `pub(crate)`: see [`Stored`].
pub(crate) async fn continuous_deployment(
    router: &MultiServiceRouter,
    case: &str,
    reference: &str,
) -> (Stored, Stored) {
    let row = config("promotion", reference);
    let primary = deployed(router, case, &row).await;

    let copied = copy_distribution(
        router,
        &primary.id,
        &format!("{reference}-staging"),
        None,
        Some("true"),
        None,
    )
    .await;
    assert_eq!(
        copied.status, 201,
        "case {case}: expected HTTP 201 from CopyDistribution, got {} with {}",
        copied.status, copied.raw
    );
    let staging = Stored {
        id: copied.root().text_at("Id").unwrap_or_default().to_string(),
        etag: copied.header("etag").to_string(),
    };
    let staging_domain = copied
        .root()
        .text_at("DomainName")
        .unwrap_or_default()
        .to_string();

    let policy = create_policy(router, &policy_xml(&staging_domain)).await;
    assert_eq!(
        policy.status, 201,
        "case {case}: expected HTTP 201 from CreateContinuousDeploymentPolicy, got {} with {}",
        policy.status, policy.raw
    );
    let policy_id = policy.root().text_at("Id").unwrap_or_default().to_string();

    let attached = update_distribution(
        router,
        &primary.id,
        &with_members(
            &row,
            &format!("<ContinuousDeploymentPolicyId>{policy_id}</ContinuousDeploymentPolicyId>"),
        ),
        Some(&primary.etag),
    )
    .await;
    assert_eq!(
        attached.status, 200,
        "case {case}: expected HTTP 200 attaching the policy, got {} with {}",
        attached.status, attached.raw
    );
    (
        Stored {
            id: primary.id,
            etag: attached.header("etag").to_string(),
        },
        staging,
    )
}

/// One distribution this row then waits on: what it is called in a failure, its id, and the
/// status the mutation's own answer reported for it (`None` where that call reports no
/// distribution of its own).
struct Moved {
    what: String,
    id: String,
    reported: Option<String>,
}

/// Make `mutation` and report every distribution it moved.
async fn moved(router: &MultiServiceRouter, case: &str, index: usize) -> Vec<Moved> {
    let reference = format!("ref-settles-{index}");
    let row = config("settles", &reference);
    match MUTATIONS[index] {
        Mutation::Create => {
            let created = create_distribution(router, &row.xml()).await;
            assert_eq!(
                created.status, 201,
                "case {case}: expected HTTP 201, got {} with {}",
                created.status, created.raw
            );
            vec![Moved {
                what: "the created distribution".to_string(),
                id: created.root().text_at("Id").unwrap_or_default().to_string(),
                reported: created.root().text_at("Status").map(str::to_string),
            }]
        }
        Mutation::CreateWithTags => {
            let created = create_distribution_with_tags(router, &row).await;
            assert_eq!(
                created.status, 201,
                "case {case}: expected HTTP 201, got {} with {}",
                created.status, created.raw
            );
            vec![Moved {
                what: "the distribution created with tags".to_string(),
                id: created.root().text_at("Id").unwrap_or_default().to_string(),
                reported: created.root().text_at("Status").map(str::to_string),
            }]
        }
        Mutation::CopyAsStaging => {
            let primary = deployed(router, case, &row).await;
            let copied = copy_distribution(
                router,
                &primary.id,
                &format!("{reference}-copy"),
                None,
                Some("true"),
                None,
            )
            .await;
            assert_eq!(
                copied.status, 201,
                "case {case}: expected HTTP 201, got {} with {}",
                copied.status, copied.raw
            );
            vec![Moved {
                what: "the staging copy".to_string(),
                id: copied.root().text_at("Id").unwrap_or_default().to_string(),
                reported: copied.root().text_at("Status").map(str::to_string),
            }]
        }
        Mutation::Update => {
            let stored = deployed(router, case, &row).await;
            // The SAME configuration, submitted whole: an update stores the configuration the
            // request carries, so what it moves is the propagation rather than any member.
            let updated =
                update_distribution(router, &stored.id, &row.update_xml(), Some(&stored.etag))
                    .await;
            assert_eq!(
                updated.status, 200,
                "case {case}: expected HTTP 200, got {} with {}",
                updated.status, updated.raw
            );
            vec![Moved {
                what: "the updated distribution".to_string(),
                id: stored.id,
                reported: updated.root().text_at("Status").map(str::to_string),
            }]
        }
        Mutation::Promote => {
            let (primary, staging) =
                continuous_deployment(router, case, &reference).await;
            // Both distributions have been written by the workflow above, and the promotion
            // writes the primary again — so the row waits on the pair.
            let promoted = call_with_headers(
                router,
                "PUT",
                &format!(
                    "/{API_VERSION}/distribution/{}/promote-staging-config\
                     ?StagingDistributionId={}",
                    primary.id, staging.id
                ),
                None,
                &[("if-match", &format!("{}, {}", primary.etag, staging.etag))],
            )
            .await;
            assert_eq!(
                promoted.status, 200,
                "case {case}: expected HTTP 200, got {} with {}",
                promoted.status, promoted.raw
            );
            vec![
                Moved {
                    what: "the promoted primary".to_string(),
                    id: primary.id,
                    reported: promoted.root().text_at("Status").map(str::to_string),
                },
                Moved {
                    what: "the staging distribution of the promotion".to_string(),
                    id: staging.id,
                    reported: None,
                },
            ]
        }
    }
}

/// RULE: every call that stores a distribution configuration leaves the distribution in the
/// transient status, and the clock then owes it the terminal one — so a client that only POLLS
/// `GetDistribution` observes `Deployed`, and the poll changes nothing about the record.
#[tokio::test]
async fn every_stored_configuration_is_owed_its_deployment() {
    for (index, mutation) in MUTATIONS.iter().enumerate() {
        // One emulator per row, so a row cannot read a distribution another one left behind.
        let router = emulator();
        let case = format!("{mutation:?}");
        for subject in moved(&router, &case, index).await {
            let case = format!("{case} / {}", subject.what);
            assert!(
                !subject.id.is_empty(),
                "case {case}: expected the call to report a distribution id"
            );
            // The transient status is OBSERVABLE: the answer to the call that moved the
            // distribution reports it, so a client acting straight after that call sees the
            // propagation it has to wait for.
            if let Some(reported) = subject.reported.as_deref() {
                assert_eq!(
                    reported, IN_PROGRESS,
                    "case {case}: expected the answer to report {IN_PROGRESS:?}, got \
                     {reported:?}"
                );
            }
            // And the clock advances it with no further call: the poll only READS.
            let before = get_distribution(&router, &subject.id).await;
            let version = before.header("etag").to_string();
            awaited_deployment(&router, &case, &subject.id).await;
            let after = get_distribution(&router, &subject.id).await;
            let status = after.root().find_text("Status").unwrap_or_default();
            assert_eq!(
                status, DEPLOYED,
                "case {case}: expected {DEPLOYED:?} once the clock owes it, got {status:?}"
            );
            // Reading is not writing: the version the mutation minted is the one the deployed
            // record still reports.
            assert_eq!(
                after.header("etag"),
                version,
                "case {case}: expected the polls to leave the version at {version:?}, got {:?}",
                after.header("etag")
            );
        }
    }
}
