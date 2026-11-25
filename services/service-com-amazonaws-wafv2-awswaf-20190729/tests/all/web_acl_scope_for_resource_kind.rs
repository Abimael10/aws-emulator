// SPDX-License-Identifier: CC0-1.0

//! Which web ACL can be put in front of which protected resource, end to end over the
//! wire.
//!
//! The rule: a web ACL protects the resource kinds its own `Scope` serves — the global
//! namespace is "for a global resource type, such as a Amazon CloudFront distribution.
//! For an Amplify application, use CLOUDFRONT"
//! (`smithy_query(query="doc",shape="CreateWebACLRequest",member="Scope")`), while a
//! resource that serves its traffic in one Region takes a REGIONAL web ACL — so
//! `AssociateWebACL` binds the pairs whose scopes match and refuses the others with
//! `WAFInvalidParameterException` (HTTP 400) and WAF's "the resource is not supported"
//! sentence, binding nothing.
//!
//! The table spans the class rather than the one recorded pair: every documented
//! resource form, crossed with BOTH scopes a web ACL can be created in, so each row
//! states which of the two scopes that kind takes and the other row of the same kind is
//! the refusal. The harness is the one in [`crate::web_acl_lifecycle`], so every call
//! goes through the router's front door.

use serde_json::json;

use crate::web_acl_associations::{listed_resources, web_acl_for_resource, PROTECTED_RESOURCES};
use crate::web_acl_lifecycle::{create_web_acl, Waf};

/// The message WAF's refusal carries when the web ACL's scope does not serve the kind
/// of resource the request names, as recorded: the whole message is this sentence, with
/// nothing of the request in it.
const RESOURCE_NOT_SUPPORTED: &str = "The resource is not supported in current region";

/// The two scopes a web ACL can be created in
/// (`smithy_query(query="shape",shape="Scope")`).
const SCOPES: &[&str] = &["REGIONAL", "CLOUDFRONT"];

/// Every documented resource form is bound by a web ACL of the scope its kind takes,
/// and refused by a web ACL of the other scope.
#[tokio::test]
async fn a_web_acl_binds_the_resource_kinds_its_scope_serves_and_refuses_the_others() {
    let waf = Waf::new();

    // One web ACL per scope, so each row of the cross product names a web ACL this
    // account really holds — the refusal under test is then about the SCOPE and not
    // about a web ACL nothing minted.
    let mut web_acls: Vec<(&str, String, String)> = Vec::new();
    for scope in SCOPES {
        let name = format!("scope-{}-acl", scope.to_lowercase());
        let (id, arn, _token) = create_web_acl(&waf, scope, &name, json!([])).await;
        web_acls.push((scope, id, arn));
    }

    for (resource_case, resource_arn, resource_type) in PROTECTED_RESOURCES {
        // The scope this kind takes: the global namespace for the kind served from
        // CloudFront's edge, the regional one for a kind that serves its traffic in a
        // single Region.
        let serving_scope = crate::web_acl_associations::web_acl_scope(resource_type);

        for (scope, web_acl_id, web_acl_arn) in &web_acls {
            let case = format!("{resource_case} with a {scope} web ACL");
            if *scope == serving_scope {
                waf.ok(
                    "AssociateWebACL",
                    json!({ "ResourceArn": resource_arn, "WebACLArn": web_acl_arn }),
                )
                .await;
                let bound = web_acl_for_resource(&waf, resource_arn)
                    .await
                    .unwrap_or_else(|| panic!("case {case:?}: the resource must report a web ACL"));
                assert_eq!(
                    bound["Id"].as_str(),
                    Some(web_acl_id.as_str()),
                    "case {case:?}: expected the web ACL {web_acl_id}, got {bound}"
                );
                let listed = listed_resources(&waf, web_acl_arn, Some(resource_type)).await;
                assert!(
                    listed.contains(&(*resource_arn).to_string()),
                    "case {case:?}: {resource_type} must list {resource_arn}, got {listed:?}"
                );
                // Taken away again, so the refused row below is judged against a
                // resource that carries nothing.
                waf.ok("DisassociateWebACL", json!({ "ResourceArn": resource_arn }))
                    .await;
            } else {
                let refusal = waf
                    .refused(
                        "AssociateWebACL",
                        json!({ "ResourceArn": resource_arn, "WebACLArn": web_acl_arn }),
                        "WAFInvalidParameterException",
                        &case,
                    )
                    .await;
                assert_eq!(
                    refusal["message"].as_str(),
                    Some(RESOURCE_NOT_SUPPORTED),
                    "case {case:?}: expected WAF's unsupported-resource sentence, got {refusal}"
                );
                let bound = web_acl_for_resource(&waf, resource_arn).await;
                assert!(
                    bound.is_none(),
                    "case {case:?}: a refused association must bind nothing, got {bound:?}"
                );
                let listed = listed_resources(&waf, web_acl_arn, Some(resource_type)).await;
                assert!(
                    listed.is_empty(),
                    "case {case:?}: a refused association must store nothing, yet \
                     {resource_type} lists {listed:?}"
                );
            }
        }
    }
}

/// A refused association leaves the binding the resource already carries: the scope
/// refusal is judged before anything is written, so a kind that is already protected by
/// the right web ACL keeps it when the wrong one is named.
#[tokio::test]
async fn a_scope_refusal_leaves_the_binding_the_resource_already_carries() {
    let waf = Waf::new();
    let (regional_id, regional_arn, _regional_token) =
        create_web_acl(&waf, "REGIONAL", "keeps-regional-acl", json!([])).await;
    let (global_id, global_arn, _global_token) =
        create_web_acl(&waf, "CLOUDFRONT", "keeps-global-acl", json!([])).await;

    // One row per scope: the resource kind that scope serves, the web ACL that is put
    // in front of it, and the web ACL of the other scope that must be refused for it.
    let rows: &[(&str, &str, &str, &str, &str)] = &[
        (
            "a resource served in one Region",
            "arn:aws:cognito-idp:us-west-2:123456789012:userpool/us-west-2_sCoPeKeEp",
            regional_id.as_str(),
            regional_arn.as_str(),
            global_arn.as_str(),
        ),
        (
            "a resource served from the edge",
            "arn:aws:amplify:us-west-2:123456789012:apps/d2keepsitsacl",
            global_id.as_str(),
            global_arn.as_str(),
            regional_arn.as_str(),
        ),
    ];

    for (case, resource_arn, held_id, held_arn, refused_arn) in rows {
        waf.ok(
            "AssociateWebACL",
            json!({ "ResourceArn": resource_arn, "WebACLArn": held_arn }),
        )
        .await;
        waf.refused(
            "AssociateWebACL",
            json!({ "ResourceArn": resource_arn, "WebACLArn": refused_arn }),
            "WAFInvalidParameterException",
            case,
        )
        .await;
        let bound = web_acl_for_resource(&waf, resource_arn)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the resource must still report a web ACL"));
        assert_eq!(
            bound["Id"].as_str(),
            Some(*held_id),
            "case {case:?}: the binding must still be {held_id}, got {bound}"
        );
    }
}
