// SPDX-License-Identifier: CC0-1.0

//! The STATIC-WEBSITE configuration an `AWS::S3::Bucket` declares, as the Amazon S3 request
//! that puts it on the bucket.
//!
//! `WebsiteConfiguration` is no member of `CreateBucket`'s payload, so it is installed on the
//! created bucket by the ONE call Amazon S3 publishes for it: `PutBucketWebsite` ("sets the
//! configuration of the website that is specified in the website subresource"). Without that
//! call the deployed bucket has no website configuration at all — `GetBucketWebsite` answers
//! `NoSuchWebsiteConfiguration` and the website endpoint serves nothing — so a stack that
//! declares one would complete reporting a configuration Amazon S3 was never asked for.
//!
//! The template's spelling is NOT the API's here, and the translation is what this module is
//! for. CloudFormation states the two documents as the object key itself where the API nests
//! each in a container of its own — `IndexDocument` is the API's `IndexDocument.Suffix` and
//! `ErrorDocument` its `ErrorDocument.Key` — and it names a routing rule's two halves
//! `RedirectRule` and `RoutingRuleCondition` where the API names them `Redirect` and
//! `Condition`. Everything else is named identically on both sides: `RedirectAllRequestsTo`
//! with its `HostName` and `Protocol`, and a redirect's `HostName`, `HttpRedirectCode`,
//! `Protocol`, `ReplaceKeyPrefixWith` and `ReplaceKeyWith`. Each value travels as the
//! template wrote it, so Amazon S3's own deserializer and handler are what judge it — which
//! is also what refuses a configuration naming both `RedirectAllRequestsTo` and another
//! member, since "if you specify this property, you can't specify any other property"
//! (`WebsiteConfiguration$RedirectAllRequestsTo`).
//!
//! A declaration missing a member the API REQUIRES is REFUSED rather than sent incomplete:
//! `RedirectAllRequestsTo$HostName` and `RoutingRule$Redirect` are required, so a declaration
//! stating either container without them is refused here. The stack must not complete with a
//! bucket whose website the deployment never set.

use super::*;

/// The property an `AWS::S3::Bucket` declares its static-website configuration under.
pub(in crate::state) const WEBSITE_CONFIGURATION: &str = "WebsiteConfiguration";

/// The template's name for the index document, which the API carries as
/// `IndexDocument$Suffix`.
const INDEX_DOCUMENT: &str = "IndexDocument";

/// The template's name for the error document, which the API carries as `ErrorDocument$Key`.
const ERROR_DOCUMENT: &str = "ErrorDocument";

/// The template's name for the redirect-everything container, which the API names the same.
const REDIRECT_ALL_REQUESTS_TO: &str = "RedirectAllRequestsTo";

/// The template's name for the list of routing rules, which the API names the same.
const ROUTING_RULES: &str = "RoutingRules";

/// The template's name for a routing rule's redirect, which the API carries as
/// `RoutingRule$Redirect`.
const REDIRECT_RULE: &str = "RedirectRule";

/// The template's name for a routing rule's condition, which the API carries as
/// `RoutingRule$Condition`.
const ROUTING_RULE_CONDITION: &str = "RoutingRuleCondition";

/// The member naming the host a redirect sends the request to, spelled the same in the
/// template and in the API.
const HOST_NAME: &str = "HostName";

/// The member naming the scheme a redirect uses, spelled the same on both sides.
const PROTOCOL: &str = "Protocol";

/// The whole `WebsiteConfiguration` a bucket's declaration states, as the `PutBucketWebsite`
/// request Amazon S3 takes — `None` for a declaration that states none, which leaves the
/// bucket without a website configuration, the state every bucket is created in.
pub(in crate::state) fn declared_website_configuration(
    declared: &Value,
) -> Result<Option<::aws_sdk_s3::types::WebsiteConfiguration>, Refusal> {
    use ::aws_sdk_s3::types::{ErrorDocument, IndexDocument, WebsiteConfiguration};

    let Some(stated) = declared.get(WEBSITE_CONFIGURATION) else {
        return Ok(None);
    };
    // "IndexDocument — the name of the index document for the website": ONE object key in the
    // template, which the API takes as the `Suffix` of its own container.
    let index = match declared_text(stated, INDEX_DOCUMENT) {
        Some(suffix) => Some(
            IndexDocument::builder()
                .suffix(suffix)
                .build()
                .map_err(|error| not_sendable(INDEX_DOCUMENT, &error))?,
        ),
        None => None,
    };
    // "ErrorDocument — the name of the error document for the website": the same story, which
    // the API takes as the `Key` of its own container.
    let error = match declared_text(stated, ERROR_DOCUMENT) {
        Some(key) => Some(
            ErrorDocument::builder()
                .key(key)
                .build()
                .map_err(|error| not_sendable(ERROR_DOCUMENT, &error))?,
        ),
        None => None,
    };
    Ok(Some(
        WebsiteConfiguration::builder()
            .set_index_document(index)
            .set_error_document(error)
            .set_redirect_all_requests_to(declared_redirect_all_requests_to(stated)?)
            .set_routing_rules(declared_routing_rules(stated)?)
            .build(),
    ))
}

/// The `RedirectAllRequestsTo` a website configuration states, or `None` where it states none.
///
/// `HostName` is REQUIRED by the API — "name of the host where requests are redirected" — so a
/// declaration stating the container without it is refused rather than sent incomplete.
fn declared_redirect_all_requests_to(
    stated: &Value,
) -> Result<Option<::aws_sdk_s3::types::RedirectAllRequestsTo>, Refusal> {
    use ::aws_sdk_s3::types::{Protocol, RedirectAllRequestsTo};

    let Some(declared) = stated.get(REDIRECT_ALL_REQUESTS_TO) else {
        return Ok(None);
    };
    let host = declared_text(declared, HOST_NAME)
        .ok_or_else(|| missing(&format!("{REDIRECT_ALL_REQUESTS_TO}.{HOST_NAME}")))?;
    Ok(Some(
        RedirectAllRequestsTo::builder()
            .host_name(host)
            .set_protocol(declared_text(declared, PROTOCOL).map(|text| Protocol::from(text.as_str())))
            .build()
            .map_err(|error| not_sendable(REDIRECT_ALL_REQUESTS_TO, &error))?,
    ))
}

/// The `RoutingRules` a website configuration states, or `None` where it lists none.
///
/// `RoutingRule$Redirect` is REQUIRED by the API — "container for redirect information" — so a
/// listed rule naming no `RedirectRule` is refused rather than sent incomplete. The condition
/// is optional on both sides: a rule stating none applies to every request.
fn declared_routing_rules(
    stated: &Value,
) -> Result<Option<Vec<::aws_sdk_s3::types::RoutingRule>>, Refusal> {
    use ::aws_sdk_s3::types::{Condition, Protocol, Redirect, RoutingRule};

    let listed = stated
        .get(ROUTING_RULES)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if listed.is_empty() {
        return Ok(None);
    }
    let mut rules = Vec::with_capacity(listed.len());
    for rule in listed {
        let declared = rule
            .get(REDIRECT_RULE)
            .ok_or_else(|| missing(&format!("{ROUTING_RULES}.{REDIRECT_RULE}")))?;
        let redirect = Redirect::builder()
            .set_host_name(declared_text(declared, HOST_NAME))
            .set_http_redirect_code(declared_text(declared, "HttpRedirectCode"))
            .set_protocol(
                declared_text(declared, PROTOCOL).map(|text| Protocol::from(text.as_str())),
            )
            .set_replace_key_prefix_with(declared_text(declared, "ReplaceKeyPrefixWith"))
            .set_replace_key_with(declared_text(declared, "ReplaceKeyWith"))
            .build();
        let condition = rule.get(ROUTING_RULE_CONDITION).map(|stated| {
            Condition::builder()
                .set_http_error_code_returned_equals(declared_text(
                    stated,
                    "HttpErrorCodeReturnedEquals",
                ))
                .set_key_prefix_equals(declared_text(stated, "KeyPrefixEquals"))
                .build()
        });
        rules.push(
            RoutingRule::builder()
                .redirect(redirect)
                .set_condition(condition)
                .build(),
        );
    }
    Ok(Some(rules))
}

/// The refusal for a `WebsiteConfiguration` declaration missing a member Amazon S3 REQUIRES.
fn missing(member: &str) -> Refusal {
    Refusal {
        code: MISSING_PROPERTY_CODE.to_string(),
        message: format!("the {WEBSITE_CONFIGURATION} of {S3_BUCKET} declares no {member}"),
    }
}

/// The refusal for a declared member the SDK will not build into the request shape, worded the
/// way every other sub-resource of this type words one.
fn not_sendable(member: &str, error: &dyn ::std::fmt::Display) -> Refusal {
    Refusal {
        code: UNIDENTIFIED_FAILURE.to_string(),
        message: format!(
            "the {member} of the {WEBSITE_CONFIGURATION} of {S3_BUCKET} cannot be sent to \
             Amazon S3: {error}"
        ),
    }
}
