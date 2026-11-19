// SPDX-License-Identifier: CC0-1.0

//! WHICH web ACL a CloudFront resource can be put behind: the association RESOLVES it,
//! end to end over HTTP, with WAFv2 mounted on the same engine.
//!
//! The rule under test: an operation that puts a WAF web ACL in front of a CloudFront
//! resource resolves the web ACL its required `WebACLArn` names against WAF — the service
//! that holds web ACLs — before it writes anything, and a value that names no web ACL of
//! WAFv2's global namespace the account holds is refused `InvalidArgument` (HTTP 400) with
//! CloudFront's fixed sentence, leaving the resource's configuration and version exactly as
//! they were. Both operations that do so are held to it:
//! `AssociateDistributionWebACL` (a distribution's own `WebACLId`) and
//! `AssociateDistributionTenantWebACL` (a tenant's `Customizations$WebAcl`).
//!
//! The rows differ along what the rule generalises over — WHETHER the value names a web ACL
//! WAF holds, and the WAYS a value can fail to:
//!
//! * two web ACLs WAF really holds, differing in their rules and their default action, which
//!   the association stores and every later read answers with;
//! * a `wafv2` web ACL ARN of the global namespace, in this very account, that WAF never
//!   minted;
//! * a web ACL WAF really holds in the REGIONAL namespace, so the pair with the first rows
//!   differs in the namespace alone rather than in whether the web ACL exists;
//! * an ARN naming another WAFv2 resource kind (an IP set) that WAF really holds;
//! * a WAF Classic web ACL ARN, and the bare WAF Classic ACL id — identifiers of a
//!   generation this emulator holds no web ACL of at all;
//! * a string that is no ARN at all, and one that merely opens like one.
//!
//! Beside the table, two invariants of the same rule: a web ACL that WAF held and that was
//! DELETED can no longer be associated (the association follows WAF's state, not its own
//! memory of it), and a refused association leaves `DistributionConfig$WebACLId` empty where
//! the distribution had no web ACL — nothing a later read can see was written.
//!
//! The wire harness, the association helpers and the readers are the ones
//! [`crate::distribution_lifecycle`], [`crate::web_acl_association`],
//! [`crate::distribution_tenant_lifecycle`] and [`crate::tenant_web_acl_association`] already
//! build, and the web ACLs come from WAFv2's own control plane
//! ([`crate::waf_control_plane`]), so these assertions travel the same path a client takes.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::CUSTOMIZATION_ACTION_OVERRIDE;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{config_table, emulator, Answer};
use crate::distribution_tenant_lifecycle::multi_tenant_distribution;
use crate::tenant_web_acl_association::{associate, create_tenant, read_tenant, web_acl_of, STARTS};
use crate::waf_control_plane::{
    create_web_acl, delete_web_acl, CreatedWebAcl, COUNTING_RULE, SCOPE_CLOUDFRONT, SCOPE_REGIONAL,
};
use crate::web_acl_association::{associate_web_acl, create, read_back};

/// The sentence CloudFront refuses an association whose web ACL it could not resolve with.
/// The whole text is fixed — it states that the web ACL named is not a validated one and what
/// to name instead — so nothing of the request appears in it. Spelled out here rather than
/// imported from the implementation, so a constant edited to some other wording fails these
/// rows.
const INVALID_WEB_ACL: &str = "The specified web ACL isn't valid. Specify a web ACL that was \
     previously validated and try again.";

/// What one row names as its web ACL, and what the association owes it.
#[derive(Debug)]
struct Row {
    case: &'static str,
    /// The `WebACLArn` the request states.
    web_acl: String,
    /// Whether the association is served (and the value stored), or refused.
    resolves: bool,
}

/// Every web ACL WAFv2 holds for these rows, created through its own control plane: two of
/// the global namespace (which CloudFront is protected by), one of the regional namespace and
/// an IP set of the global one — so the refusals below are about the namespace and the
/// resource kind rather than about a resource that happens not to exist.
async fn staged(router: &MultiServiceRouter) -> (CreatedWebAcl, CreatedWebAcl, String, String) {
    let plain = create_web_acl(
        router,
        "resolution-global-plain",
        SCOPE_CLOUDFRONT,
        "{\"Allow\":{}}",
        "",
    )
    .await;
    let ruled = create_web_acl(
        router,
        "resolution-global-ruled",
        SCOPE_CLOUDFRONT,
        "{\"Block\":{}}",
        COUNTING_RULE,
    )
    .await;
    let regional = create_web_acl(
        router,
        "resolution-regional",
        SCOPE_REGIONAL,
        "{\"Allow\":{}}",
        "",
    )
    .await
    .arn;

    // An IP set of the GLOBAL namespace: a WAFv2 resource of this account, of a kind that is
    // no web ACL, so what refuses it is the kind its ARN names.
    let (status, answered) = crate::waf_control_plane::waf_call(
        router,
        "CreateIPSet",
        "{\"Name\":\"resolution-ip-set\",\"Scope\":\"CLOUDFRONT\",\
         \"IPAddressVersion\":\"IPV4\",\"Addresses\":[\"203.0.113.0/24\"]}",
    )
    .await;
    assert_eq!(
        status, 200,
        "case staging: expected HTTP 200 from CreateIPSet, got {status} with {answered}"
    );
    let ip_set = crate::waf_control_plane::json_string(&answered, "ARN")
        .unwrap_or_else(|| panic!("case staging: CreateIPSet answered no ARN: {answered}"));

    (plain, ruled, regional, ip_set)
}

/// The table both operations are driven over: what the rule generalises over is whether the
/// value names a web ACL WAF holds in CloudFront's own namespace.
fn rows(plain: &str, ruled: &str, regional: &str, ip_set: &str) -> Vec<Row> {
    // The same ARN FORM one of the held web ACLs takes — this partition, this account, the
    // global namespace, the web ACL kind — with a name and id WAF never minted, so the only
    // thing that can refuse it is that it names no web ACL.
    let never_minted = format!(
        "{}/never-minted/11112222-3333-4444-5555-666677778888",
        plain.rsplitn(3, '/').nth(2).unwrap_or(plain)
    );
    vec![
        Row {
            case: "a web ACL WAF holds, with no rules",
            web_acl: plain.to_string(),
            resolves: true,
        },
        Row {
            case: "a web ACL WAF holds, with a rule and a blocking default",
            web_acl: ruled.to_string(),
            resolves: true,
        },
        Row {
            case: "a global web ACL ARN of this account that WAF never minted",
            web_acl: never_minted,
            resolves: false,
        },
        Row {
            case: "a web ACL WAF holds in the regional namespace",
            web_acl: regional.to_string(),
            resolves: false,
        },
        Row {
            case: "an IP set WAF holds in the global namespace",
            web_acl: ip_set.to_string(),
            resolves: false,
        },
        Row {
            case: "a WAF Classic web ACL ARN",
            web_acl: "arn:aws:waf::000000000000:webacl/473e64fd-f30b-4765-81a0-62ad96dd167a"
                .to_string(),
            resolves: false,
        },
        Row {
            case: "a bare WAF Classic ACL id",
            web_acl: "473e64fd-f30b-4765-81a0-62ad96dd167a".to_string(),
            resolves: false,
        },
        Row {
            case: "a string that is no ARN at all",
            web_acl: "not-a-valid-web-acl-arn".to_string(),
            resolves: false,
        },
        Row {
            case: "a value that merely opens like an ARN",
            web_acl: "arn:aws:wafv2".to_string(),
            resolves: false,
        },
    ]
}

/// The `InvalidArgument` refusal an unresolvable web ACL earns, asserted whole: the status,
/// the modelled error code and CloudFront's fixed sentence.
fn assert_refused(answer: &Answer, case: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case}: expected HTTP 400 for a web ACL that resolves to nothing, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        "InvalidArgument",
        "case {case}: expected InvalidArgument, got {:?} with {}",
        answer.error_code(),
        answer.raw
    );
    let message = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message, INVALID_WEB_ACL,
        "case {case}: expected the message {INVALID_WEB_ACL:?}, got {message:?}"
    );
}

/// A distribution is put behind a web ACL WAF holds and no other value: every row that
/// resolves lands and reads back, and every row that resolves to nothing is refused with
/// CloudFront's sentence while the distribution keeps the web ACL and the version it had.
#[tokio::test]
async fn the_distribution_association_resolves_the_web_acl_in_waf() {
    let router = emulator();
    let (plain, ruled, regional, ip_set) = staged(&router).await;
    let template = &config_table()[0];

    for (index, row) in rows(&plain.arn, &ruled.arn, &regional, &ip_set)
        .iter()
        .enumerate()
    {
        let case = row.case;
        let reference = format!("ref-resolution-dist-{index}");
        let body = template
            .xml()
            .replace(&template.caller_reference, &reference);
        let (id, created_etag) = create(&router, case, &body).await;

        let answer = associate_web_acl(&router, &id, &row.web_acl, Some(&created_etag)).await;
        if row.resolves {
            assert_eq!(
                answer.status, 200,
                "case {case}: expected HTTP 200 associating {:?}, got {} with {}",
                row.web_acl, answer.status, answer.raw
            );
            let (stored, _, _) = read_back(&router, &id, case).await;
            assert_eq!(
                stored, row.web_acl,
                "case {case}: expected the distribution to name {:?}, got {stored:?}",
                row.web_acl
            );
        } else {
            assert_refused(&answer, case);
            // Nothing was written and no version was minted: the distribution still names no
            // web ACL at all, at the version its create left it at.
            let (stored, read_etag, _) = read_back(&router, &id, case).await;
            assert_eq!(
                stored, "",
                "case {case}: expected the refused association to leave no web ACL, got \
                 {stored:?}"
            );
            assert_eq!(
                read_etag, created_etag,
                "case {case}: expected the refused association to leave the version at \
                 {created_etag:?}, got {read_etag:?}"
            );

            // The same distribution takes a web ACL that DOES resolve straight afterwards, so
            // what the refusal was about is the value named and nothing else.
            let landed = associate_web_acl(&router, &id, &plain.arn, Some(&created_etag)).await;
            assert_eq!(
                landed.status, 200,
                "case {case}: expected HTTP 200 associating the resolvable {:?} after the \
                 refusal, got {} with {}",
                plain.arn, landed.status, landed.raw
            );
            let (stored, _, _) = read_back(&router, &id, case).await;
            assert_eq!(
                stored, plain.arn,
                "case {case}: expected the resolvable association to store {:?}, got {stored:?}",
                plain.arn
            );
        }
    }
}

/// A distribution TENANT is held to the same rule, over the same rows: its
/// `Customizations$WebAcl` is written only for a web ACL WAF holds, and a refusal leaves the
/// customization and the version it had.
#[tokio::test]
async fn the_tenant_association_resolves_the_web_acl_in_waf() {
    let router = emulator();
    let distribution_id = multi_tenant_distribution(&router, "ref-resolution-tenant").await;
    let (plain, ruled, regional, ip_set) = staged(&router).await;
    let start = STARTS[0];

    for (index, row) in rows(&plain.arn, &ruled.arn, &regional, &ip_set)
        .iter()
        .enumerate()
    {
        let case = row.case;
        let slug = format!("res{index}");
        let (id, created_etag) =
            create_tenant(&router, &distribution_id, &slug, case, start).await;

        let answer = associate(&router, &id, &row.web_acl, Some(&created_etag)).await;
        if row.resolves {
            assert_eq!(
                answer.status, 200,
                "case {case}: expected HTTP 200 associating {:?} with a tenant, got {} with {}",
                row.web_acl, answer.status, answer.raw
            );
            let read = read_tenant(&router, &id, case).await;
            assert_eq!(
                web_acl_of(read.root()),
                Some((
                    CUSTOMIZATION_ACTION_OVERRIDE.to_string(),
                    row.web_acl.clone()
                )),
                "case {case}: expected the tenant to carry {:?}, got {:?}",
                row.web_acl,
                web_acl_of(read.root())
            );
        } else {
            assert_refused(&answer, case);
            let read = read_tenant(&router, &id, case).await;
            assert_eq!(
                web_acl_of(read.root()),
                None,
                "case {case}: expected the refused association to leave the tenant with no web \
                 ACL customization, got {:?}",
                web_acl_of(read.root())
            );
            assert_eq!(
                read.header("etag"),
                created_etag,
                "case {case}: expected the refused association to leave the version at \
                 {created_etag:?}, got {:?}",
                read.header("etag")
            );
        }
    }
}

/// The association follows WAF's STATE: a web ACL that resolved while WAF held it resolves to
/// nothing once it is deleted there, and the refusal is the same sentence as for one that
/// never existed. Nothing of CloudFront's own changed in between.
#[tokio::test]
async fn a_web_acl_deleted_in_waf_can_no_longer_be_associated() {
    let router = emulator();
    let case = "deleted-in-waf";
    let web_acl = create_web_acl(
        &router,
        "resolution-to-be-deleted",
        SCOPE_CLOUDFRONT,
        "{\"Allow\":{}}",
        "",
    )
    .await;
    let template = &config_table()[0];

    // While WAF holds it, the association lands.
    let body = template
        .xml()
        .replace(&template.caller_reference, "ref-resolution-held");
    let (held_id, held_etag) = create(&router, case, &body).await;
    let landed = associate_web_acl(&router, &held_id, &web_acl.arn, Some(&held_etag)).await;
    assert_eq!(
        landed.status, 200,
        "case {case}: expected HTTP 200 while WAF holds {:?}, got {} with {}",
        web_acl.arn, landed.status, landed.raw
    );

    delete_web_acl(&router, &web_acl).await;

    // Once WAF no longer holds it, the very same ARN resolves to nothing.
    let body = template
        .xml()
        .replace(&template.caller_reference, "ref-resolution-deleted");
    let (id, etag) = create(&router, case, &body).await;
    let refused = associate_web_acl(&router, &id, &web_acl.arn, Some(&etag)).await;
    assert_refused(&refused, case);
    let (stored, read_etag, _) = read_back(&router, &id, case).await;
    assert_eq!(
        stored, "",
        "case {case}: expected the refused association to leave no web ACL, got {stored:?}"
    );
    assert_eq!(
        read_etag, etag,
        "case {case}: expected the refused association to leave the version at {etag:?}, got \
         {read_etag:?}"
    );
}
