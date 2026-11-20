// SPDX-License-Identifier: CC0-1.0

//! The POLICY DOCUMENT CloudFront's resource-policy surface attaches, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. The rules under test, each about the DOCUMENT rather than the `ResourceArn`
//! the sibling suites table:
//!
//! * a `PolicyDocument` that is not a resource-based policy document in the IAM JSON policy
//!   language — a text that is not parseable JSON, JSON that is no policy, a document
//!   carrying a member name the language does not define or no version string it defines, or
//!   a policy declaring no statement — is refused on its FORMAT: HTTP 400 `InvalidArgument`
//!   with [`INVALID_POLICY_FORMAT_MESSAGE`], and nothing is persisted;
//! * a document that IS a policy but does not describe a plain share of the resource it is
//!   being attached to — a statement naming a resource other than the request's
//!   `ResourceArn` or naming none at all, an action outside the read of that one origin
//!   ([`SHAREABLE_VPC_ORIGIN_ACTIONS`]), or a condition on the grant — is refused as an
//!   unusable POLICY: HTTP 400 `InvalidArgument` with [`INVALID_POLICY_MESSAGE`], and nothing
//!   is persisted;
//! * what a read reports is the service's own rendering of the document it accepted
//!   ([`stored`]): the parsed document re-rendered, so a one-value list collapses to that
//!   value alone and every other member survives, and two layouts of the same policy
//!   converge on one text.
//!
//! The rows differ along what the rules generalise over: for the format, the JSON type of
//! the text and which clause of the grammar it breaks; for the policy, which element names
//! the offending value, whether it is written as a value or in a list of them, which of the
//! five VPC-origin actions the model defines is granted and in which case it is spelled, and
//! the operator and value type a condition is written with; for the rendering, the arity of
//! each value list, which element carries it and the whitespace the document arrived in.
//!
//! The harness (the router and every request helper) is the lifecycle suites', reused rather
//! than copied.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    EMPTY_RESOURCE_POLICY_DOCUMENT, INVALID_POLICY_FORMAT_MESSAGE, INVALID_POLICY_MESSAGE,
    RESOURCE_POLICY_RESOURCE_TYPE, SHAREABLE_VPC_ORIGIN_ACTIONS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{emulator, Answer, ACCOUNT_ID};
use crate::resource_policy_lifecycle::{
    created_vpc_origin_arn, get_resource_policy, granted_principal, message, policy,
    put_resource_policy, reported_document, stored,
};

/// Every action this service's model defines on a VPC origin, beside whether a resource
/// policy on one may grant it.
///
/// Read off the model: `GetVpcOrigin` is the one operation that addresses a SINGLE VPC origin
/// by its id and only reads it (`GET /2020-05-31/vpc-origin/{Id}`). `CreateVpcOrigin`,
/// `UpdateVpcOrigin` and `DeleteVpcOrigin` change the resource (`POST`, `PUT` and `DELETE`),
/// and `ListVpcOrigins` reads the account's collection rather than one origin
/// (`GET /2020-05-31/vpc-origin`, which takes no id), so none of them names something a
/// policy attached to ONE origin grants (`smithy_query(query="operation",
/// operation="GetVpcOrigin,ListVpcOrigins,CreateVpcOrigin,UpdateVpcOrigin,DeleteVpcOrigin")`).
const VPC_ORIGIN_ACTIONS: [(&str, bool); 5] = [
    ("cloudfront:GetVpcOrigin", true),
    ("cloudfront:ListVpcOrigins", false),
    ("cloudfront:CreateVpcOrigin", false),
    ("cloudfront:UpdateVpcOrigin", false),
    ("cloudfront:DeleteVpcOrigin", false),
];

/// A one-statement document granting `action` over `resource`, written as a caller writes
/// one: the elements the policy language requires of a resource-based statement, plus the
/// resource element only when a row names one.
fn granting(action: &str, resource: Option<&str>) -> String {
    let resource = resource
        .map(|value| format!(",\"Resource\":{value}"))
        .unwrap_or_default();
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"share\",\"Effect\":\"Allow\",\
         \"Principal\":{{\"AWS\":\"{}\"}},\
         \"Action\":{action}{resource}}}]}}",
        granted_principal()
    )
}

/// The document the read reports for `arn`, parsed, so a case asserts about the policy the
/// service rendered rather than about its whitespace.
fn reported_value(answer: &Answer) -> ::serde_json::Value {
    let text = reported_document(answer);
    ::serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("the reported document must be JSON, got {text:?} ({error})");
    })
}

/// Assert that `arn` carries no policy: the read answers 200 with the empty document, which
/// is what a refused write has to leave behind.
async fn assert_carries_no_policy(router: &MultiServiceRouter, case: &str, arn: &str) {
    let read = get_resource_policy(router, arn).await;
    assert_eq!(
        read.status, 200,
        "case {case}: expected HTTP 200 from the read after the refusal, got {} ({})",
        read.status, read.raw
    );
    assert_eq!(
        reported_document(&read),
        EMPTY_RESOURCE_POLICY_DOCUMENT,
        "case {case}: expected the refused write to persist nothing, got {:?}",
        reported_document(&read)
    );
}

/// Assert that the write of `document` under `arn` was refused with `expected`.
async fn assert_refused(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    document: &str,
    expected: &str,
) {
    let answer = put_resource_policy(router, arn, document).await;
    assert_eq!(
        answer.status, 400,
        "case {case}: expected HTTP 400 for {document:?}, got {} ({})",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        "InvalidArgument",
        "case {case}: expected InvalidArgument for {document:?}, got {:?} ({})",
        answer.error_code(),
        answer.raw
    );
    assert_eq!(
        message(&answer),
        expected,
        "case {case}: expected message {expected:?}, got {:?}",
        message(&answer)
    );
}

// ---------------------------------------------------------------------------
// The document's format
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_policy_document_that_is_no_policy_is_refused_on_its_format() {
    let router = emulator();
    let arn = created_vpc_origin_arn(&router, "vo-format").await;

    // The rows differ along the JSON type of the text and the clause of the policy grammar
    // it breaks: text that does not parse at all, JSON that is not an object, an object
    // that declares no statement, a statement that is not an object, a statement missing one
    // of the elements a resource-based statement must carry, a member name the language does
    // not define, and no version string it defines.
    let rows: [(&str, String); 16] = [
        (
            "text that is not JSON at all",
            "{this is not valid json at all".to_string(),
        ),
        ("a truncated object", "{\"Statement\":".to_string()),
        ("a bare word", "policy".to_string()),
        ("a JSON number", "5".to_string()),
        ("a JSON string", "\"a policy\"".to_string()),
        ("a JSON array", "[]".to_string()),
        (
            "an empty JSON object, which declares no statement",
            "{}".to_string(),
        ),
        (
            "an empty statement list",
            "{\"Version\":\"2012-10-17\",\"Statement\":[]}".to_string(),
        ),
        (
            "a statement that is not an object",
            "{\"Version\":\"2012-10-17\",\"Statement\":[\"share\"]}".to_string(),
        ),
        (
            "a statement declaring no Effect",
            "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Principal\":\"*\",\
             \"Action\":\"cloudfront:GetVpcOrigin\"}]}"
                .to_string(),
        ),
        (
            "a statement naming no action",
            "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
             \"Principal\":\"*\"}]}"
                .to_string(),
        ),
        (
            "a statement naming no principal, which a resource-based policy states",
            "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
             \"Action\":\"cloudfront:GetVpcOrigin\"}]}"
                .to_string(),
        ),
        (
            "a document declaring no version string",
            "{\"Statement\":[{\"Effect\":\"Allow\",\"Principal\":\"*\",\
             \"Action\":\"cloudfront:GetVpcOrigin\"}]}"
                .to_string(),
        ),
        (
            "a document declaring a version the policy language does not define",
            "{\"Version\":\"2525-01-01\",\"Statement\":[{\"Effect\":\"Allow\",\
             \"Principal\":\"*\",\"Action\":\"cloudfront:GetVpcOrigin\"}]}"
                .to_string(),
        ),
        (
            "a top-level member name the policy language does not define",
            "{\"Version\":\"2012-10-17\",\"Policy\":\"share\",\
             \"Statement\":[{\"Effect\":\"Allow\",\"Principal\":\"*\",\
             \"Action\":\"cloudfront:GetVpcOrigin\"}]}"
                .to_string(),
        ),
        (
            "a statement member name the policy language does not define",
            "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
             \"Principal\":\"*\",\"Action\":\"cloudfront:GetVpcOrigin\",\
             \"Actions\":\"cloudfront:ListVpcOrigins\"}]}"
                .to_string(),
        ),
    ];

    for (case, document) in &rows {
        assert_refused(&router, case, &arn, document, INVALID_POLICY_FORMAT_MESSAGE).await;
        assert_carries_no_policy(&router, case, &arn).await;
    }

    // The same resource takes a document that IS a policy, so what every row above was
    // refused on is the text it submitted and not the resource it named.
    let accepted = policy(&arn, "after-the-format-refusals");
    let written = put_resource_policy(&router, &arn, &accepted).await;
    assert_eq!(
        written.status, 200,
        "case after the format refusals: expected HTTP 200 from the write, got {} ({})",
        written.status, written.raw
    );
}

// ---------------------------------------------------------------------------
// The resource the policy speaks about
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_policy_naming_another_resource_is_refused_and_attaches_nothing() {
    let router = emulator();
    let arn = created_vpc_origin_arn(&router, "vo-resource-element").await;
    let sibling = created_vpc_origin_arn(&router, "vo-sibling").await;
    let fabricated = format!(
        "arn:aws:cloudfront::{ACCOUNT_ID}:{RESOURCE_POLICY_RESOURCE_TYPE}/vo-never-created"
    );

    // The rows differ along which resource the statement names and how it is written: a
    // sibling resource that EXISTS, a well-formed ARN naming nothing, the policy language's
    // wildcard, one of a list of values, the negated element, a second statement, and a
    // statement naming no resource at all.
    let refused: [(&str, String); 7] = [
        (
            "a sibling VPC origin that exists",
            granting("\"cloudfront:GetVpcOrigin\"", Some(&format!("\"{sibling}\""))),
        ),
        (
            "a well-formed VPC origin ARN naming nothing",
            granting(
                "\"cloudfront:GetVpcOrigin\"",
                Some(&format!("\"{fabricated}\"")),
            ),
        ),
        (
            "the wildcard, which names every resource",
            granting("\"cloudfront:GetVpcOrigin\"", Some("\"*\"")),
        ),
        (
            "a list carrying this resource and a sibling",
            granting(
                "\"cloudfront:GetVpcOrigin\"",
                Some(&format!("[\"{arn}\",\"{sibling}\"]")),
            ),
        ),
        (
            "the negated resource element naming a sibling",
            format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":\"*\",\"Action\":\"cloudfront:GetVpcOrigin\",\
                 \"NotResource\":\"{sibling}\"}}]}}"
            ),
        ),
        (
            "a second statement naming another resource",
            format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":\"*\",\"Action\":\"cloudfront:GetVpcOrigin\",\
                 \"Resource\":\"{arn}\"}},{{\"Effect\":\"Allow\",\"Principal\":\"*\",\
                 \"Action\":\"cloudfront:GetVpcOrigin\",\"Resource\":\"{fabricated}\"}}]}}"
            ),
        ),
        (
            "no resource element at all, so the policy names no ARN to match",
            granting("\"cloudfront:GetVpcOrigin\"", None),
        ),
    ];

    for (case, document) in &refused {
        assert_refused(&router, case, &arn, document, INVALID_POLICY_MESSAGE).await;
        assert_carries_no_policy(&router, case, &arn).await;
    }

    // And the forms that DO name the resource the policy is being attached to are accepted:
    // the ARN itself, and the ARN as the one value of a list.
    let accepted: [(&str, String); 2] = [
        (
            "the ARN the policy is attached to",
            granting("\"cloudfront:GetVpcOrigin\"", Some(&format!("\"{arn}\""))),
        ),
        (
            "that ARN as the one value of a list",
            granting("\"cloudfront:GetVpcOrigin\"", Some(&format!("[\"{arn}\"]"))),
        ),
    ];

    for (case, document) in &accepted {
        let written = put_resource_policy(&router, &arn, document).await;
        assert_eq!(
            written.status, 200,
            "case {case}: expected HTTP 200 from the write, got {} ({})",
            written.status, written.raw
        );
        let read = get_resource_policy(&router, &arn).await;
        let expected = stored(document);
        assert_eq!(
            reported_document(&read),
            expected.as_str(),
            "case {case}: expected the document {expected:?}, got {:?}",
            reported_document(&read)
        );
    }

    // The sibling the refused documents named carries nothing either: a refused write
    // attaches no policy anywhere.
    assert_carries_no_policy(&router, "the sibling the refusals named", &sibling).await;
}

// ---------------------------------------------------------------------------
// The action the policy grants
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_policy_granting_an_action_the_surface_does_not_share_is_refused() {
    let router = emulator();
    let arn = created_vpc_origin_arn(&router, "vo-action-element").await;

    // The catalogue the surface shares is exactly the model's read of ONE VPC origin.
    let shareable: Vec<&str> = VPC_ORIGIN_ACTIONS
        .iter()
        .filter(|(_, shareable)| *shareable)
        .map(|(action, _)| *action)
        .collect();
    assert_eq!(
        shareable,
        SHAREABLE_VPC_ORIGIN_ACTIONS.to_vec(),
        "the surface must share exactly the actions that read the one shared origin, got {:?}",
        SHAREABLE_VPC_ORIGIN_ACTIONS
    );

    // Every action the model defines on a VPC origin, asked one per row: the read of that one
    // origin is attached, and every action that changes it or reads the account's collection
    // is refused.
    for (action, is_shareable) in VPC_ORIGIN_ACTIONS {
        let case = format!("the action {action}");
        let document = granting(&format!("\"{action}\""), Some(&format!("\"{arn}\"")));
        if is_shareable {
            let written = put_resource_policy(&router, &arn, &document).await;
            assert_eq!(
                written.status, 200,
                "case {case}: expected HTTP 200 from the write, got {} ({})",
                written.status, written.raw
            );
            let read = get_resource_policy(&router, &arn).await;
            let expected = stored(&document);
            assert_eq!(
                reported_document(&read),
                expected.as_str(),
                "case {case}: expected the document {expected:?}, got {:?}",
                reported_document(&read)
            );
        } else {
            assert_refused(&router, &case, &arn, &document, INVALID_POLICY_MESSAGE).await;
        }
    }

    // A grant the surface refuses, in the forms an action element takes: a wildcard (which
    // stands for actions the surface does not share), one mutating action beside the shared
    // one, an action of this service that is not about the shared resource, an action of
    // another service, and the negated element naming a mutating action. Every row names the
    // resource the policy is attached to, so what is refused is the ACTION it grants.
    let resource = format!("\"{arn}\"");
    let refused: [(&str, String); 6] = [
        (
            "the wildcard that stands for every action",
            granting("\"*\"", Some(&resource)),
        ),
        (
            "every action of this service's namespace",
            granting("\"cloudfront:*\"", Some(&resource)),
        ),
        (
            "a prefix wildcard over the shared resource's actions",
            granting("\"cloudfront:GetVpcOrigin*\"", Some(&resource)),
        ),
        (
            "a mutating action beside the shared one",
            granting(
                "[\"cloudfront:GetVpcOrigin\",\"cloudfront:DeleteVpcOrigin\"]",
                Some(&resource),
            ),
        ),
        (
            "an action of this service that is not about a VPC origin",
            granting("\"cloudfront:GetDistribution\"", Some(&resource)),
        ),
        (
            "an action of another service",
            granting("\"s3:GetObject\"", Some(&resource)),
        ),
    ];
    for (case, document) in &refused {
        assert_refused(&router, case, &arn, document, INVALID_POLICY_MESSAGE).await;
    }

    // The negated action element is read the same way: it names an action, so a mutating one
    // there is refused too.
    assert_refused(
        &router,
        "the negated action element naming a mutating action",
        &arn,
        &format!(
            "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Deny\",\
             \"Principal\":\"*\",\"NotAction\":\"cloudfront:DeleteVpcOrigin\",\
             \"Resource\":\"{arn}\"}}]}}"
        ),
        INVALID_POLICY_MESSAGE,
    )
    .await;

    // An action name is read case-insensitively, as IAM reads one, so the shared action
    // spelled in another case is the same grant.
    let lowercased = granting("\"cloudfront:getvpcorigin\"", Some(&resource));
    let written = put_resource_policy(&router, &arn, &lowercased).await;
    assert_eq!(
        written.status, 200,
        "case the shared action in lower case: expected HTTP 200, got {} ({})",
        written.status, written.raw
    );

    // Every refusal above left the policy the resource already carried alone: the document
    // the last accepted write persisted is still the one a read reports.
    let read = get_resource_policy(&router, &arn).await;
    let expected = stored(&lowercased);
    assert_eq!(
        reported_document(&read),
        expected.as_str(),
        "case after the action refusals: expected the document {expected:?}, got {:?}",
        reported_document(&read)
    );
}

// ---------------------------------------------------------------------------
// The condition the policy would put on the share
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_policy_conditioning_the_share_is_refused_and_attaches_nothing() {
    let router = emulator();
    let arn = created_vpc_origin_arn(&router, "vo-condition").await;

    // The statement the rows below start from is one the surface attaches, so what each row
    // changes is the condition block alone.
    let shared = policy(&arn, "share");
    let written = put_resource_policy(&router, &arn, &shared).await;
    assert_eq!(
        written.status, 200,
        "case the unconditional share: expected HTTP 200 from the write, got {} ({})",
        written.status, written.raw
    );

    // The rows differ along the operator the condition is written with, the type of the value
    // it compares, how many conditions one statement carries, which statement carries one,
    // and an EMPTY condition block: the element's presence is what the surface refuses, so
    // none of those is attached.
    let conditioned = |condition: &str| {
        format!(
            "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"share\",\
             \"Effect\":\"Allow\",\
             \"Principal\":{{\"AWS\":\"{}\"}},\
             \"Action\":\"cloudfront:GetVpcOrigin\",\"Resource\":\"{arn}\",\
             \"Condition\":{condition}}}]}}",
            granted_principal()
        )
    };
    let rows: [(&str, String); 6] = [
        (
            "a string comparison",
            conditioned("{\"StringEquals\":{\"aws:PrincipalAccount\":\"111122223333\"}}"),
        ),
        (
            "a pattern comparison over a source ARN",
            conditioned(
                "{\"StringLike\":{\"aws:SourceArn\":\"arn:aws:cloudfront::111122223333:*\"}}",
            ),
        ),
        (
            "a numeric comparison",
            conditioned("{\"NumericLessThan\":{\"aws:MultiFactorAuthAge\":3600}}"),
        ),
        (
            "two operators in one block",
            conditioned(
                "{\"Bool\":{\"aws:SecureTransport\":true},\
                 \"StringEquals\":{\"aws:PrincipalOrgID\":\"o-abcdefghij\"}}",
            ),
        ),
        ("an empty condition block", conditioned("{}")),
        (
            "a condition on the second statement only",
            format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":\"*\",\"Action\":\"cloudfront:GetVpcOrigin\",\
                 \"Resource\":\"{arn}\"}},{{\"Effect\":\"Deny\",\"Principal\":\"*\",\
                 \"Action\":\"cloudfront:GetVpcOrigin\",\"Resource\":\"{arn}\",\
                 \"Condition\":{{\"Bool\":{{\"aws:SecureTransport\":false}}}}}}]}}"
            ),
        ),
    ];

    for (case, document) in &rows {
        assert_refused(&router, case, &arn, document, INVALID_POLICY_MESSAGE).await;

        // Nothing was attached by the refused call: the resource still carries the
        // unconditional share the accepted write left.
        let read = get_resource_policy(&router, &arn).await;
        let expected = stored(&shared);
        assert_eq!(
            reported_document(&read),
            expected.as_str(),
            "case {case}: expected the document {expected:?}, got {:?}",
            reported_document(&read)
        );
    }
}

// ---------------------------------------------------------------------------
// The form the surface reports a stored document in
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_stored_document_is_the_services_own_rendering_of_the_submitted_one() {
    let router = emulator();
    let arn = created_vpc_origin_arn(&router, "vo-rendering").await;

    // The rows differ along the arity of a value list, which element carries it, and the
    // whitespace the document arrived in. Each row states the value the read must report,
    // parsed, so what is asserted is the policy and not its layout.
    //
    // The identity every row grants to is the one the surface resolves ([`granted_principal`]),
    // and the row that keeps a two-value list names the WILDCARD beside it — also a principal
    // the surface resolves — so each row's only variable is the layout under test.
    let principal = granted_principal();
    let rows: [(&str, String, ::serde_json::Value); 5] = [
        (
            "a one-value Action list, which collapses to the value alone",
            granting("[\"cloudfront:GetVpcOrigin\"]", Some(&format!("\"{arn}\""))),
            ::serde_json::json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Sid": "share",
                    "Effect": "Allow",
                    "Principal": {"AWS": principal.as_str()},
                    "Action": "cloudfront:GetVpcOrigin",
                    "Resource": arn,
                }],
            }),
        ),
        (
            "an Action already written as the value alone, which is unchanged",
            granting("\"cloudfront:GetVpcOrigin\"", Some(&format!("\"{arn}\""))),
            ::serde_json::json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Sid": "share",
                    "Effect": "Allow",
                    "Principal": {"AWS": principal.as_str()},
                    "Action": "cloudfront:GetVpcOrigin",
                    "Resource": arn,
                }],
            }),
        ),
        (
            "a two-value principal list, which keeps its brackets",
            format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"share\",\
                 \"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\
                 [\"{principal}\",\"*\"]}},\
                 \"Action\":\"cloudfront:GetVpcOrigin\",\"Resource\":\"{arn}\"}}]}}"
            ),
            ::serde_json::json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Sid": "share",
                    "Effect": "Allow",
                    "Principal": {"AWS": [
                        principal.as_str(),
                        "*",
                    ]},
                    "Action": "cloudfront:GetVpcOrigin",
                    "Resource": arn,
                }],
            }),
        ),
        (
            "a one-value Resource list and a one-value principal list, which both collapse",
            format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":[\"{principal}\"]}},\
                 \"Action\":\"cloudfront:GetVpcOrigin\",\"Resource\":[\"{arn}\"]}}]}}"
            ),
            ::serde_json::json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": {"AWS": principal.as_str()},
                    "Action": "cloudfront:GetVpcOrigin",
                    "Resource": arn,
                }],
            }),
        ),
        (
            "the same policy submitted with whitespace, which converges on one text",
            format!(
                "{{\n  \"Version\": \"2012-10-17\",\n  \"Statement\": [\n    {{\n      \
                 \"Effect\": \"Allow\",\n      \"Principal\": {{ \"AWS\": \
                 [ \"{principal}\" ] }},\n      \"Action\": \
                 [ \"cloudfront:GetVpcOrigin\" ],\n      \"Resource\": [ \"{arn}\" ]\n    \
                 }}\n  ]\n}}"
            ),
            ::serde_json::json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": {"AWS": principal.as_str()},
                    "Action": "cloudfront:GetVpcOrigin",
                    "Resource": arn,
                }],
            }),
        ),
    ];

    let mut texts: Vec<(&str, String)> = Vec::new();
    for (case, submitted, expected) in &rows {
        let written = put_resource_policy(&router, &arn, submitted).await;
        assert_eq!(
            written.status, 200,
            "case {case}: expected HTTP 200 from the write, got {} ({})",
            written.status, written.raw
        );

        let read = get_resource_policy(&router, &arn).await;
        let reported = reported_value(&read);
        assert_eq!(
            &reported, expected,
            "case {case}: expected the document {expected:?}, got {reported:?}"
        );

        // The rendering is a FIXED POINT: a client that puts back what it read sees no
        // drift, which is what lets a read-modify-write round trip converge.
        let text = reported_document(&read).to_string();
        let again = put_resource_policy(&router, &arn, &text).await;
        assert_eq!(
            again.status, 200,
            "case {case}: expected HTTP 200 writing back what the read reported, got {} ({})",
            again.status, again.raw
        );
        let reread = get_resource_policy(&router, &arn).await;
        assert_eq!(
            reported_document(&reread),
            text.as_str(),
            "case {case}: expected the re-written document {text:?}, got {:?}",
            reported_document(&reread)
        );
        texts.push((case, text));
    }

    // The two layouts of the last policy are stored as the SAME text: the document the read
    // reports is the service's rendering of the parsed policy, not the caller's bytes.
    let (compact_case, compact) = &texts[3];
    let (spaced_case, spaced) = &texts[4];
    assert_eq!(
        compact, spaced,
        "case {compact_case} against {spaced_case}: expected one stored text, got \
         {compact:?} and {spaced:?}"
    );

    // And a one-value Action list is reported as the bare value in the TEXT too, not only in
    // the parsed document.
    let (collapsed_case, collapsed) = &texts[0];
    assert!(
        collapsed.contains("\"Action\" : \"cloudfront:GetVpcOrigin\""),
        "case {collapsed_case}: expected the Action to be reported as the value alone, got \
         {collapsed:?}"
    );
}
