// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE `ContinuousDeploymentPolicyInUse` carries, end to end over HTTP.
//!
//! Every assertion goes through the router, so the case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. The rule: a `DeleteContinuousDeploymentPolicy` of a policy a stored
//! distribution still attaches is refused `ContinuousDeploymentPolicyInUse` (HTTP 409), and
//! what the refusal STATES is the fixed sentence this service puts on the wire for that
//! condition — a property of the CONDITION, so it carries neither the policy the request
//! addressed nor the distribution that holds it, and two callers whose only difference is which
//! policy they asked to delete read the same sentence.
//!
//! The rows differ along what the rule generalises over: which policy of the suite's own table
//! is attached (the traffic configurations differ), and which distribution of the account
//! attaches it — the sentence is the same for each, which is what "fixed text" means.
//!
//! The expected sentence is a LITERAL here rather than the crate's own constant, so a constant
//! edited to some other wording fails this suite. The harness (the router, the request helpers
//! and each kind's configuration table) is the one the per-resource suites use, reused rather
//! than copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::continuous_deployment_policies::{create_policy, get_policy, policy_table, staged};
use crate::distribution_lifecycle::{
    call, config, create_distribution, emulator, update_distribution, Answer, API_VERSION,
};

/// The sentence `ContinuousDeploymentPolicyInUse` carries on the wire.
const IN_USE: &str =
    "The specified continuous deployment policy is currently associated with a distribution.";

/// `DELETE /2020-05-31/continuous-deployment-policy/{Id}` under the `If-Match` header.
async fn delete_policy(router: &MultiServiceRouter, id: &str, if_match: &str) -> Answer {
    call(
        router,
        "DELETE",
        &format!("/{API_VERSION}/continuous-deployment-policy/{id}"),
        None,
        Some(if_match),
    )
    .await
}

/// RULE: the refusal of a delete of an attached policy states the service's fixed sentence for
/// the condition, naming nothing of the request — and the policy is still there afterwards.
#[tokio::test]
async fn deleting_an_attached_policy_states_the_services_own_sentence() {
    let router = emulator();
    let policies = staged(&router, "inusesentence", policy_table()).await;

    for (index, policy) in policies.iter().enumerate().take(3) {
        let case = policy.case;
        let created = create_policy(&router, &policy.xml()).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected HTTP 201 from CreateContinuousDeploymentPolicy, got {} with {}",
            created.status, created.raw
        );
        let policy_id = created.root().text_at("Id").unwrap_or_default().to_string();
        let policy_etag = created.header("etag").to_string();

        let row = config("in-use-sentence", &format!("ref-in-use-{index}"));
        let distribution = create_distribution(&router, &row.xml()).await;
        assert_eq!(
            distribution.status, 201,
            "case {case}: expected HTTP 201 from CreateDistribution, got {} with {}",
            distribution.status, distribution.raw
        );
        let distribution_id = distribution
            .root()
            .text_at("Id")
            .unwrap_or_default()
            .to_string();
        // `ContinuousDeploymentPolicyId` is the member a primary distribution attaches a policy
        // through, declared after the ones the row's own document states.
        let attached = update_distribution(
            &router,
            &distribution_id,
            &row.update_xml().replace(
                "</DistributionConfig>",
                &format!(
                    "<ContinuousDeploymentPolicyId>{policy_id}</ContinuousDeploymentPolicyId>\
                     </DistributionConfig>"
                ),
            ),
            Some(distribution.header("etag")),
        )
        .await;
        assert_eq!(
            attached.status, 200,
            "case {case}: expected HTTP 200 attaching the policy, got {} with {}",
            attached.status, attached.raw
        );

        let refused = delete_policy(&router, &policy_id, &policy_etag).await;
        assert_eq!(
            refused.status, 409,
            "case {case}: expected HTTP 409 while a distribution attaches the policy, got {} \
             with {}",
            refused.status, refused.raw
        );
        assert_eq!(
            refused.error_code(),
            "ContinuousDeploymentPolicyInUse",
            "case {case}: expected ContinuousDeploymentPolicyInUse, got {:?} in {}",
            refused.error_code(),
            refused.raw
        );
        let stated = refused.root().find_text("Message").unwrap_or_default();
        assert_eq!(
            stated, IN_USE,
            "case {case}: expected the sentence {IN_USE:?}, got {stated:?}"
        );
        // Fixed text: nothing of the request or of the store reaches it.
        for named in [policy_id.as_str(), distribution_id.as_str()] {
            assert!(
                !stated.contains(named),
                "case {case}: expected nothing of the request in the sentence, got {stated:?}"
            );
        }
        // And the refusal left the policy where it was.
        let read = get_policy(&router, &policy_id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected the refused delete to leave the policy readable, got {} with \
             {}",
            read.status, read.raw
        );
    }
}
