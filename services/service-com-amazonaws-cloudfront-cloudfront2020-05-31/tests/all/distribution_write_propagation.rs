// SPDX-License-Identifier: CC0-1.0

//! What a call that STORES a distribution configuration leaves for the next read, end to end
//! over HTTP.
//!
//! RULE: `Distribution$Status` reports whether "the distribution's information is fully
//! propagated to all CloudFront edge locations", and that propagation is owed ONCE — the span
//! after the call that MINTED the distribution, which has never reached the edge locations. A
//! call that stores a new configuration for a distribution whose propagation is already behind
//! it does not restart that span: it applies the configuration at the instant it is made, so
//! the transient status is what the WRITE'S OWN answer reports and the FIRST read a caller
//! makes afterwards reports the terminal one. Without that, a caller following the documented
//! wait-for-`Deployed` pattern pays a whole poll interval per write for a propagation that has
//! in fact finished, and a workflow of several writes — continuous deployment's copy, repoint,
//! attach and promote — is spent entirely in those waits.
//!
//! The rows differ along WHICH call stored the configuration, which is what the rule
//! generalises over: every route of this service that replaces a distribution's configuration
//! (the update, in three forms, the two web ACL association calls, and the staging-config
//! promotion). The second test states the other half of the rule over the two calls that MINT
//! a distribution — a create and a copy — whose first propagation is still owed and which a
//! read therefore still reports as transient.
//!
//! The harness (the router, the request helpers, the `DistributionConfig` builder, the staged
//! continuous deployment and the web ACLs WAF really holds) is the crate's own, reused rather
//! than copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_copy::copy_distribution;
use crate::distribution_deployment_settles::{continuous_deployment, deployed};
use crate::distribution_lifecycle::{
    awaited_deployment, call_with_headers, config, create_distribution, emulator, get_distribution,
    is_version_token, update_distribution, Answer, Config, API_VERSION, DEPLOYED,
};
use crate::distribution_web_acl_association::disassociate_web_acl;
use crate::web_acl_association::{associate_web_acl, held_web_acls};

/// The status a distribution whose configuration has not reached the edge locations yet
/// reports — a LITERAL here rather than the crate's own constant, so a constant edited to some
/// other value fails these rows.
const IN_PROGRESS: &str = "InProgress";

/// Which call stores the configuration the row then reads.
#[derive(Debug, Clone, Copy)]
enum Write {
    /// `PUT /2020-05-31/distribution/{Id}/config` carrying the configuration unchanged.
    UpdateUnchanged,
    /// The same route carrying a different `Comment`.
    UpdateComment,
    /// The same route carrying `Enabled` turned off — the write a teardown makes.
    UpdateDisabled,
    /// `PUT /2020-05-31/distribution/{Id}/associate-web-acl`.
    AssociateWebAcl,
    /// `PUT /2020-05-31/distribution/{Id}/disassociate-web-acl`.
    DisassociateWebAcl,
    /// `PUT /2020-05-31/distribution/{Id}/promote-staging-config`.
    PromoteStagingConfig,
}

impl Write {
    /// Whether the answer to this call carries a `Distribution`, and so a `Status` of its own:
    /// the two web ACL association calls answer with the distribution's `Id` and `ETag` and
    /// nothing else (`AssociateDistributionWebACLResult`,
    /// `DisassociateDistributionWebACLResult`), so for those the status is only ever read.
    fn reports_a_status(self) -> bool {
        !matches!(self, Write::AssociateWebAcl | Write::DisassociateWebAcl)
    }
}

/// Every call of this service that replaces the configuration of a distribution that already
/// exists.
const WRITES: [Write; 6] = [
    Write::UpdateUnchanged,
    Write::UpdateComment,
    Write::UpdateDisabled,
    Write::AssociateWebAcl,
    Write::DisassociateWebAcl,
    Write::PromoteStagingConfig,
];

/// The ARN of one web ACL the account really holds, created through WAF's own control plane.
async fn one_web_acl(router: &MultiServiceRouter, case: &str) -> String {
    held_web_acls(router)
        .await
        .first()
        .unwrap_or_else(|| panic!("case {case}: the account holds no web ACL to associate"))
        .1
        .clone()
}

/// Make `which` against a distribution whose own propagation is already behind it, and report
/// the distribution it wrote beside the answer it gave.
async fn written(router: &MultiServiceRouter, case: &str, which: Write) -> (String, Answer) {
    let reference = format!("ref-write-propagation-{case}");
    let row = config("write-propagation", &reference);
    match which {
        Write::UpdateUnchanged => {
            let stored = deployed(router, case, &row).await;
            let answer =
                update_distribution(router, &stored.id, &row.update_xml(), Some(&stored.etag))
                    .await;
            (stored.id, answer)
        }
        Write::UpdateComment => {
            let stored = deployed(router, case, &row).await;
            let changed = Config {
                comment: "a comment this write stored".to_string(),
                ..row.clone()
            };
            let answer =
                update_distribution(router, &stored.id, &changed.update_xml(), Some(&stored.etag))
                    .await;
            (stored.id, answer)
        }
        Write::UpdateDisabled => {
            let stored = deployed(router, case, &row).await;
            let disabled = Config {
                enabled: false,
                ..row.clone()
            };
            let answer = update_distribution(
                router,
                &stored.id,
                &disabled.update_xml(),
                Some(&stored.etag),
            )
            .await;
            (stored.id, answer)
        }
        Write::AssociateWebAcl => {
            let stored = deployed(router, case, &row).await;
            let web_acl = one_web_acl(router, case).await;
            let answer = associate_web_acl(router, &stored.id, &web_acl, Some(&stored.etag)).await;
            (stored.id, answer)
        }
        Write::DisassociateWebAcl => {
            let stored = deployed(router, case, &row).await;
            let web_acl = one_web_acl(router, case).await;
            let associated =
                associate_web_acl(router, &stored.id, &web_acl, Some(&stored.etag)).await;
            assert_eq!(
                associated.status, 200,
                "case {case}: expected HTTP 200 from AssociateDistributionWebACL, got {} with {}",
                associated.status, associated.raw
            );
            // The association is itself a write, so its own propagation is waited out before
            // the row's call is made: what the row reads afterwards is what IT left behind.
            awaited_deployment(router, case, &stored.id).await;
            let version = get_distribution(router, &stored.id).await;
            let answer =
                disassociate_web_acl(router, &stored.id, Some(version.header("etag"))).await;
            (stored.id, answer)
        }
        Write::PromoteStagingConfig => {
            let (primary, staging) = continuous_deployment(router, case, &reference).await;
            // The attach that ended the staging is a write of the primary too, so the primary
            // is read as deployed again before the promotion is made.
            awaited_deployment(router, case, &primary.id).await;
            let answer = call_with_headers(
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
            (primary.id, answer)
        }
    }
}

/// RULE: a configuration this service has already applied is reported as propagating by the
/// answer to the call that applied it, and by nothing after it — the first read reports
/// `Deployed`, and that read changes nothing about the record.
#[tokio::test]
async fn a_write_leaves_the_configuration_deployed_at_the_instant_it_applied_it() {
    for which in WRITES {
        // One emulator per row, so a row cannot read a distribution another one left behind.
        let router = emulator();
        let case = format!("{which:?}");
        let (id, answer) = written(&router, &case, which).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected HTTP 200 from the write, got {} with {}",
            answer.status, answer.raw
        );
        // The transient status IS observable, for every write whose answer carries a
        // `Distribution` at all: the answer to the call that stored the configuration reports
        // it, which is where a client reads it.
        if which.reports_a_status() {
            assert_eq!(
                answer.root().text_at("Status"),
                Some(IN_PROGRESS),
                "case {case}: expected the write's own answer to report {IN_PROGRESS}, got {:?}",
                answer.root().text_at("Status")
            );
        }
        let version = answer.header("etag").to_string();
        assert!(
            is_version_token(&version),
            "case {case}: expected a minted version token in the write's ETag, got {version:?}"
        );

        // The FIRST read after the write — no poll, no waiting: the configuration was applied
        // by the call above, so the propagation it is owed is owed at that instant.
        let read = get_distribution(&router, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected HTTP 200 from GetDistribution, got {} with {}",
            read.status, read.raw
        );
        let status = read.root().text_at("Status").unwrap_or_default();
        assert_eq!(
            status, DEPLOYED,
            "case {case}: expected the first read after the write to report {DEPLOYED}, got \
             {status:?}"
        );
        // Reading is not writing: the version the write minted is the one the record reports.
        assert_eq!(
            read.header("etag"),
            version,
            "case {case}: expected the read to report the version the write minted \
             ({version:?}), got {:?}",
            read.header("etag")
        );
        // And it stays there: the status a later read reports is the same terminal one.
        let again = get_distribution(&router, &id).await;
        let status = again.root().text_at("Status").unwrap_or_default();
        assert_eq!(
            status, DEPLOYED,
            "case {case}: expected {DEPLOYED} to be stable across reads, got {status:?}"
        );
    }
}

/// RULE, the other half: a distribution that has NEVER propagated is owed that propagation, so
/// a read straight after the call that minted it reports the transient status, and the CLOCK —
/// no call — advances it. Both calls that mint a distribution are rows here.
#[tokio::test]
async fn a_minted_distribution_is_still_owed_its_first_propagation() {
    let router = emulator();
    let row = config("mint-propagation", "ref-mint-propagation");

    let created = create_distribution(&router, &row.xml()).await;
    assert_eq!(
        created.status, 201,
        "case create: expected HTTP 201 from CreateDistribution, got {} with {}",
        created.status, created.raw
    );
    let id = created
        .root()
        .text_at("Id")
        .unwrap_or_default()
        .to_string();
    let read = get_distribution(&router, &id).await;
    assert_eq!(
        read.root().text_at("Status"),
        Some(IN_PROGRESS),
        "case create: expected a read of a just-created distribution to report {IN_PROGRESS}, \
         got {:?}",
        read.root().text_at("Status")
    );
    awaited_deployment(&router, "create", &id).await;

    // A copy is a distribution of its own, minted by this call, so its own first propagation is
    // owed exactly as a create's is.
    let copied = copy_distribution(
        &router,
        &id,
        "ref-mint-propagation-copy",
        Some(true),
        Some("true"),
        None,
    )
    .await;
    assert_eq!(
        copied.status, 201,
        "case copy: expected HTTP 201 from CopyDistribution, got {} with {}",
        copied.status, copied.raw
    );
    let copy_id = copied
        .root()
        .text_at("Id")
        .unwrap_or_default()
        .to_string();
    let read = get_distribution(&router, &copy_id).await;
    assert_eq!(
        read.root().text_at("Status"),
        Some(IN_PROGRESS),
        "case copy: expected a read of a just-copied distribution to report {IN_PROGRESS}, got \
         {:?}",
        read.root().text_at("Status")
    );
    awaited_deployment(&router, "copy", &copy_id).await;
}
