// SPDX-License-Identifier: CC0-1.0

//! The web ACL a distribution tenant's `Customizations` names is RESOLVED before it is
//! written, end to end over HTTP.
//!
//! RULE: `WebAclCustomization$Arn` is "the Amazon Resource Name (ARN) of the WAF web ACL" a
//! tenant is protected by (`smithy_query(query="shape",shape="Customizations,WebAclCustomization")`),
//! and the tenant reports that ARN back through the same member — so the ARN a write names has
//! to resolve to a web ACL WAF really holds, in the one namespace CloudFront is protected by
//! (the `CLOUDFRONT` scope, "for a global resource type, such as a Amazon CloudFront
//! distribution"). A customization naming anything else is refused `InvalidArgument` (HTTP
//! 400) with the one sentence this service answers an unusable web ACL with, nothing is
//! created, and a tenant that already had one keeps it. It is the same question, asked of the
//! same records, that `AssociateDistributionTenantWebACL` resolves before it writes that very
//! member.
//!
//! The rows span what the rule generalises over: a web ACL WAF holds in CloudFront's namespace
//! (admitted), one WAF holds in the REGIONAL namespace, an ARN of CloudFront's namespace WAF
//! never minted, a web ACL WAF held and then DELETED, a WAF Classic identifier of either form,
//! and a value that is no ARN at all; beside the customizations that name no web ACL to
//! resolve — the `disable` action, which "won't have WAF web ACL protections", a customization
//! that names only geographic restrictions, and no `Customizations` member at all. Both writes
//! run the whole table: `CreateDistributionTenant` and `UpdateDistributionTenant`.
//!
//! The wire harness, the tenant helpers and WAFv2's own control plane are the sibling suites'
//! ([`crate::distribution_tenant_lifecycle`], [`crate::waf_control_plane`]), reused rather
//! than copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{emulator, Answer, API_VERSION};
use crate::distribution_tenant_lifecycle::{
    create_distribution_tenant, escape, managed_certificate_request, multi_tenant_distribution,
};
use crate::distribution_tenant_update::{update_tenant, update_xml};
use crate::tenant_web_acl_association::{read_tenant, web_acl_of};
use crate::waf_control_plane::{
    create_web_acl, delete_web_acl, held_web_acl_arn, SCOPE_CLOUDFRONT, SCOPE_REGIONAL,
};

/// The code every refusal here travels under, and the status the model gives it
/// (`smithy_query(query="operation",operation="CreateDistributionTenant,UpdateDistributionTenant")`
/// -> `InvalidArgument`, httpError 400).
const INVALID_ARGUMENT: &str = "InvalidArgument";

/// The sentence a write naming a web ACL it cannot resolve is refused with, written out as the
/// LITERAL the service answers so a reworded refusal fails this suite.
const INVALID_WEB_ACL: &str = "The specified web ACL isn't valid. Specify a web ACL that was \
     previously validated and try again.";

/// One customization a row submits, and whether the web ACL it names resolves.
struct Row {
    case: String,
    /// The `Customizations` element, or `None` to leave the member out entirely.
    customizations: Option<String>,
    /// The `(Action, Arn)` a read must report once an ADMITTED row has landed, or `None` for
    /// a row whose customizations name no web ACL.
    web_acl_after: Option<(String, String)>,
    /// Whether the row is refused.
    refused: bool,
}

/// A `Customizations` element overriding the tenant's web ACL with `arn`.
fn overriding(arn: &str) -> String {
    format!(
        "<Customizations><WebAcl><Action>override</Action><Arn>{}</Arn></WebAcl>\
         </Customizations>",
        escape(arn)
    )
}

/// Every customization the rule is asserted over, built against the web ACLs WAF holds on
/// `router`.
async fn rows(router: &MultiServiceRouter) -> Vec<Row> {
    // The web ACL every admitted row names: one WAF holds, in CloudFront's own namespace.
    let held = held_web_acl_arn(router, "tenant-customization-held").await;
    // A web ACL of the REGIONAL namespace: WAF holds it, but it protects regional resources,
    // so WAF would never enforce it in front of a tenant.
    let regional = create_web_acl(
        router,
        "tenant-customization-regional",
        SCOPE_REGIONAL,
        "{\"Allow\":{}}",
        "",
    )
    .await
    .arn;
    // A web ACL WAF held and no longer does: the ARN WAF itself minted, released again.
    let released = create_web_acl(
        router,
        "tenant-customization-released",
        SCOPE_CLOUDFRONT,
        "{\"Allow\":{}}",
        "",
    )
    .await;
    let released_arn = released.arn.clone();
    delete_web_acl(router, &released).await;
    // An ARN of CloudFront's namespace WAF never minted, built in the layout WAF writes one
    // in so nothing but the account's records can tell it apart from the one above.
    let never_minted =
        "arn:aws:wafv2:us-east-1:000000000000:global/webacl/NeverMinted/\
         00000000-0000-0000-0000-000000000000"
            .to_string();

    let mut rows = vec![
        Row {
            case: "a web ACL WAF holds in CloudFront's namespace".to_string(),
            customizations: Some(overriding(&held)),
            web_acl_after: Some(("override".to_string(), held.clone())),
            refused: false,
        },
        Row {
            case: "the web ACL disabled, which names none".to_string(),
            customizations: Some(
                "<Customizations><WebAcl><Action>disable</Action></WebAcl></Customizations>"
                    .to_string(),
            ),
            web_acl_after: Some(("disable".to_string(), String::new())),
            refused: false,
        },
        Row {
            case: "customizations that name no web ACL at all".to_string(),
            customizations: Some(
                "<Customizations><GeoRestrictions><RestrictionType>whitelist\
                 </RestrictionType><Locations><Location>DE</Location></Locations>\
                 </GeoRestrictions></Customizations>"
                    .to_string(),
            ),
            web_acl_after: None,
            refused: false,
        },
        Row {
            case: "no Customizations member at all".to_string(),
            customizations: None,
            web_acl_after: None,
            refused: false,
        },
    ];
    for (case, arn) in [
        ("a web ACL of the REGIONAL namespace", regional),
        ("an ARN of CloudFront's namespace WAF never minted", never_minted),
        ("a web ACL WAF held and then released", released_arn),
        (
            "a WAF Classic ARN",
            "arn:aws:waf::000000000000:webacl/473e64fd-f30b-4765-81a0-62ad96dd167a".to_string(),
        ),
        (
            "a bare WAF Classic id",
            "473e64fd-f30b-4765-81a0-62ad96dd167a".to_string(),
        ),
        ("a value that is no ARN at all", "web-acl-please".to_string()),
    ] {
        rows.push(Row {
            case: case.to_string(),
            customizations: Some(overriding(&arn)),
            web_acl_after: None,
            refused: true,
        });
    }
    rows
}

/// The `CreateDistributionTenantRequest` one row submits under `distribution_id`.
fn create_xml(distribution_id: &str, slug: &str, customizations: Option<&str>) -> String {
    format!(
        "<CreateDistributionTenantRequest \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <DistributionId>{}</DistributionId><Name>{slug}</Name>\
         <Domains><member><Domain>{slug}.example.com</Domain></member></Domains>\
         <Parameters><member><Name>tenantName</Name><Value>{slug}</Value></member></Parameters>\
         {}{}</CreateDistributionTenantRequest>",
        escape(distribution_id),
        customizations.unwrap_or_default(),
        managed_certificate_request()
    )
}

/// Assert `answer` is the refusal an unresolvable web ACL earns.
fn assert_refused(answer: &Answer, case: &str, context: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case} ({context}): expected HTTP 400, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        INVALID_ARGUMENT,
        "case {case} ({context}): expected {INVALID_ARGUMENT}, got {:?} in {}",
        answer.error_code(),
        answer.raw
    );
    let message = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message, INVALID_WEB_ACL,
        "case {case} ({context}): expected the fixed sentence, got {message:?}"
    );
}

/// A create stores a tenant only where the web ACL its customization names resolves, and an
/// admitted customization is what a read of the tenant reports.
#[tokio::test]
async fn a_create_admits_only_a_web_acl_waf_holds() {
    let router = emulator();
    let distribution_id = multi_tenant_distribution(&router, "ref-tenant-customization").await;

    for (index, row) in rows(&router).await.into_iter().enumerate() {
        let case = row.case.as_str();
        let slug = format!("cust{index}");
        let answer = create_distribution_tenant(
            &router,
            &create_xml(&distribution_id, &slug, row.customizations.as_deref()),
        )
        .await;
        if row.refused {
            assert_refused(&answer, case, "CreateDistributionTenant");
            // Nothing was created: the name the refused create named is still free, which is
            // what a second create under it answering 201 shows.
            let free = create_distribution_tenant(
                &router,
                &create_xml(&distribution_id, &slug, None),
            )
            .await;
            assert_eq!(
                free.status, 201,
                "case {case}: expected the refused create to have claimed nothing, got {} with \
                 {}",
                free.status, free.raw
            );
            continue;
        }

        assert_eq!(
            answer.status, 201,
            "case {case}: expected HTTP 201 from CreateDistributionTenant, got {} with {}",
            answer.status, answer.raw
        );
        let id = answer.root().text_at("Id").unwrap_or_default().to_string();
        let read = read_tenant(&router, &id, case).await;
        assert_eq!(
            web_acl_of(read.root()),
            row.web_acl_after,
            "case {case}: expected the tenant to report web ACL customization {:?}, got {:?}",
            row.web_acl_after,
            web_acl_of(read.root())
        );
    }
}

/// The same rule on the UPDATE path, against a tenant that already carries a web ACL: a
/// refused update leaves that customization and the tenant's version exactly as they were.
#[tokio::test]
async fn an_update_admits_only_a_web_acl_waf_holds() {
    let router = emulator();
    let distribution_id =
        multi_tenant_distribution(&router, "ref-tenant-customization-update").await;
    let held = held_web_acl_arn(&router, "tenant-customization-update-held").await;

    for (index, row) in rows(&router).await.into_iter().enumerate() {
        let case = row.case.as_str();
        // Each row updates a tenant of its own, created carrying a web ACL WAF holds, so a
        // refused update is visible as the customization it did not replace.
        let slug = format!("custup{index}");
        let created = create_distribution_tenant(
            &router,
            &create_xml(&distribution_id, &slug, Some(&overriding(&held))),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected the subject tenant to be created, got {} with {}",
            created.status, created.raw
        );
        let id = created.root().text_at("Id").unwrap_or_default().to_string();
        let etag = created.header("etag").to_string();

        let answer = update_tenant(
            &router,
            &id,
            &update_xml(row.customizations.as_deref().unwrap_or_default()),
            Some(&etag),
        )
        .await;
        if row.refused {
            assert_refused(&answer, case, "UpdateDistributionTenant");
            let read = read_tenant(&router, &id, case).await;
            assert_eq!(
                web_acl_of(read.root()),
                Some(("override".to_string(), held.clone())),
                "case {case}: expected the refused update to leave the web ACL it had, got {:?}",
                web_acl_of(read.root())
            );
            assert_eq!(
                read.header("etag"),
                etag,
                "case {case}: expected the refused update to leave the version at {etag:?}, got \
                 {:?}",
                read.header("etag")
            );
            continue;
        }

        assert_eq!(
            answer.status, 200,
            "case {case}: expected HTTP 200 from UpdateDistributionTenant, got {} with {}",
            answer.status, answer.raw
        );
        let read = read_tenant(&router, &id, case).await;
        // An update that states no `Customizations` leaves the stored ones as they were,
        // which is the web ACL the create put there.
        let expected = match row.customizations {
            Some(_) => row.web_acl_after.clone(),
            None => Some(("override".to_string(), held.clone())),
        };
        assert_eq!(
            web_acl_of(read.root()),
            expected,
            "case {case}: expected the tenant to report web ACL customization {expected:?}, got \
             {:?}",
            web_acl_of(read.root())
        );
    }
}
