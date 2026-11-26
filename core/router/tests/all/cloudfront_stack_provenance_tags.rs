// SPDX-License-Identifier: CC0-1.0

//! The stack provenance tags a CloudFormation-deployed CloudFront distribution carries, and
//! the same keys refused to an API caller.
//!
//! The rule under test: **a tag key in the namespace AWS reserves for itself (`aws:`) is
//! AWS's to write and no API CALLER's.** The two halves of it need two crates to agree, so
//! they are asserted together here:
//!
//! 1. CloudFormation "propagates" the stack-level `Tags` to the resources it creates and adds
//!    three tags of its own naming the stack and the resource's place in it
//!    (`aws:cloudformation:stack-name`, `aws:cloudformation:stack-id`,
//!    `aws:cloudformation:logical-id`). Those keys are in the reserved namespace, and
//!    CloudFront's own `ListTagsForResource` on the distribution the stack published must
//!    answer with every one of them — the deployment is what writes them, from inside, so no
//!    public API of CloudFront's was called by a client to put them there.
//! 2. A client's own `TagResource` naming a key in that namespace is refused
//!    `InvalidTagging` (HTTP 400) with CloudFront's fixed sentence for it, and the
//!    distribution's tag set is left exactly as the deployment left it — while a key outside
//!    the namespace is attached on the same call path, so what the refusal turns on is the
//!    key, not the surface.
//!
//! What a client does with it: tag-based cost allocation and tag-based access control read
//! `aws:cloudformation:*`, so a distribution a stack created without them is invisible to
//! both; and a caller that could write them could forge a resource's stack provenance.
//!
//! The rows differ along what the rule generalises over on the caller's side: how far into
//! the reserved namespace the key reaches (the prefix alone, one segment, the three keys the
//! deployment itself wrote), the case the namespace is written in — a tag key is
//! case-sensitive but the namespace is AWS's however it is spelled — and keys that only look
//! reserved, which are attached.
//!
//! The harness (the router, the awsQuery wire and the stack poll) is
//! [`super::cloudformation_cloudfront_handler_supplied_config_members`]'s, reused rather than
//! copied.

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use super::cloudformation_cloudfront_handler_supplied_config_members::{
    cloudformation, resource_reasons, router_with_authentication, settled_stack,
};
use super::query_wire::{block_of, elements, opt_element, physical_ids_of};

/// The Region the stack deploys in, which is what the credential scope of each call names.
const REGION: &str = "us-east-1";

/// CloudFront's control-plane host and the API version its routes live under.
const CLOUDFRONT_HOST: &str = "cloudfront.amazonaws.com";
const API_VERSION: &str = "2020-05-31";

/// The logical id the template declares its distribution under.
const DISTRIBUTION_LOGICAL_ID: &str = "ProvenanceCdn";

/// The stack whose deployment writes the provenance tags.
const STACK_NAME: &str = "cloudfront-provenance-tags";

/// The stack-level tag the deployment propagates beside the three of its own.
const STACK_TAG: (&str, &str) = ("owner", "edge-team");

/// The three keys CloudFormation records a resource's stack provenance under, in the
/// namespace AWS reserves. SOURCE: the stack-level `Tags` are "propagated to the resources
/// that CloudFormation creates in the stack", beside the stack name, the stack id and the
/// resource's logical id.
const STACK_NAME_TAG: &str = "aws:cloudformation:stack-name";
const STACK_ID_TAG: &str = "aws:cloudformation:stack-id";
const LOGICAL_ID_TAG: &str = "aws:cloudformation:logical-id";

/// The sentence CloudFront refuses a caller's reserved key with, written out as the LITERAL
/// that service answers so a reworded refusal fails this suite.
const RESERVED_KEY_MESSAGE: &str = "Tag key cannot start with 'aws:'.";

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Call one CloudFront control-plane operation over its restXml wire, and answer
/// `(status, body)`.
async fn cloudfront(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    document: Option<&str>,
) -> (u16, String) {
    let authorization = authorization("cloudfront");
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", CLOUDFRONT_HOST)
        .header("authorization", authorization.as_str());
    if document.is_some() {
        builder = builder.header("content-type", "application/xml");
    }
    let response = router
        .dispatch(
            builder
                .body(
                    document
                        .map(|body| Bytes::from(body.to_string()))
                        .unwrap_or_default(),
                )
                .expect("a well-formed request"),
        )
        .await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// The template the stack deploys: one distribution over a custom origin, which needs no
/// other service to stand up.
fn template() -> String {
    serde_json::json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            DISTRIBUTION_LOGICAL_ID: {
                "Type": "AWS::CloudFront::Distribution",
                "Properties": {"DistributionConfig": {
                    "Enabled": true,
                    "Comment": "the provenance of a deployed distribution",
                    "Origins": [{
                        "Id": "the-site",
                        "DomainName": "origin.example.com",
                        "CustomOriginConfig": {
                            "OriginProtocolPolicy": "https-only",
                            "OriginSSLProtocols": ["TLSv1.2"]
                        }
                    }],
                    "DefaultCacheBehavior": {
                        "TargetOriginId": "the-site",
                        "ViewerProtocolPolicy": "allow-all"
                    }
                }}
            }
        }
    })
    .to_string()
}

/// The tag set CloudFront reports for `arn`, read through `ListTagsForResource` as the
/// key-to-value map a client reads it as.
async fn tags_of(
    router: &MultiServiceRouter,
    arn: &str,
    case: &str,
) -> std::collections::BTreeMap<String, String> {
    let (status, body) = cloudfront(
        router,
        "GET",
        &format!("/{API_VERSION}/tagging?Resource={arn}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListTagsForResource on {arn}, got {status} with \
         {body}"
    );
    body.split("<Tag>")
        .skip(1)
        .filter_map(|tag| {
            let tag = tag.split("</Tag>").next()?;
            let key = elements(tag, "Key").into_iter().next()?;
            let value = elements(tag, "Value").into_iter().next().unwrap_or_default();
            Some((key, value))
        })
        .collect()
}

/// One `TagResource` call a client makes, with the tag set `tags` names.
async fn tag_resource(
    router: &MultiServiceRouter,
    arn: &str,
    tags: &[(&str, &str)],
) -> (u16, String) {
    let items: String = tags
        .iter()
        .map(|(key, value)| format!("<Tag><Key>{key}</Key><Value>{value}</Value></Tag>"))
        .collect();
    let document = format!(
        "<Tags xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Items>{items}</Items></Tags>"
    );
    cloudfront(
        router,
        "POST",
        &format!("/{API_VERSION}/tagging?Operation=Tag&Resource={arn}"),
        Some(&document),
    )
    .await
}

/// One key a client's own `TagResource` names, and whether the reserved namespace refuses it.
struct Row {
    case: &'static str,
    key: &'static str,
    reserved: bool,
}

/// Every key the caller's half of the rule is asserted over.
const ROWS: &[Row] = &[
    Row {
        case: "the reserved prefix alone",
        key: "aws:",
        reserved: true,
    },
    Row {
        case: "one reserved segment",
        key: "aws:owner",
        reserved: true,
    },
    Row {
        case: "the key the deployment itself wrote",
        key: STACK_NAME_TAG,
        reserved: true,
    },
    Row {
        case: "the namespace in upper case",
        key: "AWS:owner",
        reserved: true,
    },
    Row {
        case: "the namespace without its colon",
        key: "aws",
        reserved: false,
    },
    Row {
        case: "a key opening with the namespace's letters",
        key: "awsome",
        reserved: false,
    },
    Row {
        case: "a key carrying a colon of its own",
        key: "cost:centre",
        reserved: false,
    },
];

/// A deployed distribution carries its stack's provenance tags, and a client may not write a
/// key in that namespace onto it.
#[tokio::test]
async fn a_deployed_distribution_carries_its_stack_tags_and_a_caller_may_not_write_them() {
    let router = router_with_authentication(false);

    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK_NAME),
            ("TemplateBody", &template()),
            ("Tags.member.1.Key", STACK_TAG.0),
            ("Tags.member.1.Value", STACK_TAG.1),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case the deployment: expected HTTP 200 from CreateStack, got {status} with {body}"
    );
    let described = settled_stack(&router, STACK_NAME, "the deployment").await;
    let reported = opt_element(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        reported,
        "CREATE_COMPLETE",
        "case the deployment: expected the stack to reach CREATE_COMPLETE, got {reported:?} — {}",
        resource_reasons(&router, STACK_NAME).await
    );
    let stack_id = opt_element(&described, "StackId").unwrap_or_default();

    // The distribution the stack published, read in CloudFront by the physical id it
    // published for it.
    let (status, resources) =
        cloudformation(&router, "DescribeStackResources", &[("StackName", STACK_NAME)]).await;
    assert_eq!(
        status, 200,
        "case the deployment: expected HTTP 200 from DescribeStackResources, got {status} with \
         {resources}"
    );
    let id = physical_ids_of(&resources)
        .get(DISTRIBUTION_LOGICAL_ID)
        .cloned()
        .unwrap_or_else(|| {
            panic!("case the deployment: the stack published no physical id in {resources}")
        });
    let (status, read) = cloudfront(
        &router,
        "GET",
        &format!("/{API_VERSION}/distribution/{id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case the deployment: expected CloudFront to hold the distribution {id}, got {status} \
         with {read}"
    );
    let arn = block_of(&read, "ARN")
        .map(str::to_string)
        .unwrap_or_else(|| panic!("case the deployment: the read carries no ARN in {read}"));

    // RULE 1: the three keys CloudFormation records a resource's stack provenance under are
    // on the distribution, beside the stack-level tag the deployment propagated.
    let carried = tags_of(&router, &arn, "the deployment").await;
    for (key, expected) in [
        (STACK_NAME_TAG, STACK_NAME.to_string()),
        (STACK_ID_TAG, stack_id.clone()),
        (LOGICAL_ID_TAG, DISTRIBUTION_LOGICAL_ID.to_string()),
        (STACK_TAG.0, STACK_TAG.1.to_string()),
    ] {
        assert_eq!(
            carried.get(key),
            Some(&expected),
            "case the deployment: expected the distribution to carry {key}={expected:?}, got \
             {carried:?}"
        );
    }

    // RULE 2: a client's own call is held to the namespace, key by key, and a refusal leaves
    // the tag set exactly as the deployment left it.
    let mut expected = carried.clone();
    for row in ROWS {
        let case = row.case;
        let before = tags_of(&router, &arn, case).await;
        assert_eq!(
            before, expected,
            "case {case:?}: expected the tag set {expected:?} before the call, got {before:?}"
        );

        let (status, answer) = tag_resource(&router, &arn, &[(row.key, "written-by-a-caller")]).await;
        if row.reserved {
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 from TagResource, got {status} with {answer}"
            );
            assert_eq!(
                elements(&answer, "Code").into_iter().next().as_deref(),
                Some("InvalidTagging"),
                "case {case:?}: expected InvalidTagging, got {answer}"
            );
            // Read through the element reader that DECODES the entities the XML serializer
            // escapes, so what is compared is the sentence rather than its encoding.
            assert_eq!(
                elements(&answer, "Message").into_iter().next().as_deref(),
                Some(RESERVED_KEY_MESSAGE),
                "case {case:?}: expected the fixed sentence, got {answer}"
            );
        } else {
            assert_eq!(
                status, 204,
                "case {case:?}: expected HTTP 204 from TagResource, got {status} with {answer}"
            );
            expected.insert(row.key.to_string(), "written-by-a-caller".to_string());
        }

        let after = tags_of(&router, &arn, case).await;
        assert_eq!(
            after, expected,
            "case {case:?}: expected the tag set {expected:?} after the call, got {after:?}"
        );
    }
}
