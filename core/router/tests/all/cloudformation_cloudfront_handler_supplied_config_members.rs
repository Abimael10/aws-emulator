// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::CloudFront::Distribution` without the
//! configuration members CloudFront's API demands of every document it is sent deploys, and
//! deploys AGAIN when the stack is updated.
//!
//! The rule under test: **a resource handler sends the API payload WHOLE — for a member the
//! API demands that the resource type publishes as an optional property, the handler itself
//! states the value the resource behaves by when a template names none, so a template that
//! leaves the member out deploys and UPDATES exactly as the resource type documents.** Three
//! members of a distribution's configuration are such a member:
//!
//! * a custom origin's `HTTPPort` and `HTTPSPort` are `@required` on the wire
//!   (`smithy_query(query="shape",shape="CustomOriginConfig")`) and documented with the
//!   defaults `80` and `443`, so a declaration that states only a protocol policy is reached
//!   on those ports;
//! * `DistributionConfig$PriceClass` is what CloudFront demands of a configuration being
//!   WRITTEN — an update submits the configuration whole, so a member the resource carries
//!   and the document leaves out is refused (`IllegalUpdate`, "Price class is missing for the
//!   resource") rather than defaulted — and a distribution that chose no class serves from
//!   all edge locations, which is `PriceClass_All`. It "only supports standard
//!   distributions. You can't specify this field for multi-tenant distributions", so a
//!   `tenant-only` configuration is complete without it and must not carry one.
//!
//! Two crates have to agree for the rule to hold (CloudFormation renders and sends the
//! document; CloudFront judges it, holds the configuration and answers the readback), so the
//! suite lives here rather than in either crate.
//!
//! The rows differ along what the rule generalises over: which of the two ports a
//! declaration states (neither, only the HTTPS one, only the HTTP one), whether the
//! configuration declares a price class of its own, whether the distribution is standard or
//! multi-tenant — and, for a multi-tenant one, which member says so, its `ConnectionMode` or
//! the `TenantConfig` that describes its tenants — and whether the change the update carries
//! is to the origin the distribution fetches from (the recorded blue/green cutover, an origin
//! `DomainName` resolved from a stack parameter) or to another member of the configuration.
//!
//! Every expected value is computed here from what the row declares; the distribution's id is
//! CloudFront's to mint and is read out of the stack's own report of its resources.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{block_of, form_encode, opt_element, physical_ids_of};

/// How long a stack operation is given to reach its terminal status. GENEROUS: what is waited
/// for is the CONDITION (the status the stack reports), polled until it holds, so the only
/// thing this bound decides is when a stack that never settles is reported as a failure.
const SETTLES_WITHIN: Duration = Duration::from_secs(60);

/// How often the stack's status is read while waiting for it to settle.
const POLL_EVERY: Duration = Duration::from_millis(50);

/// The Region every row deploys in. CloudFront is a global service, and the Region is what
/// the credential scope of each call names.
const REGION: &str = "us-east-1";

/// The logical id the rows declare their distribution under.
pub(crate) const DISTRIBUTION_LOGICAL_ID: &str = "Cdn";

/// The stack parameter each row's update changes, and the two values it takes: the comment
/// the distribution carries, so every row's update really changes the configuration CloudFront
/// is sent.
pub(crate) const COMMENT_PARAMETER: &str = "Comment";
pub(crate) const COMMENT_ON_CREATE: &str = "the estate as created";
const COMMENT_ON_UPDATE: &str = "the estate after its update";

/// The stack parameter that decides WHICH bucket the origin-repointing row fetches from, and
/// the two values it takes.
pub(crate) const ACTIVE_BUCKET_PARAMETER: &str = "ActiveBucket";
pub(crate) const BUCKET_A: &str = "A";
pub(crate) const BUCKET_B: &str = "B";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    router_with_authentication(false)
}

/// The same three services, with request authentication as `authenticated` says — which is
/// what a suite serving VIEWER requests through this estate needs to choose.
pub(crate) fn router_with_authentication(authenticated: bool) -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(authenticated);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
pub(crate) async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Read one distribution back through CloudFront's own `GetDistribution`.
async fn get_distribution(router: &MultiServiceRouter, id: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("/2020-05-31/distribution/{id}"))
        .header("host", format!("cloudfront.{REGION}.amazonaws.com"))
        .header("authorization", authorization("cloudfront"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Read the stack's status until it is one `DescribeStacks` reports as terminal, or until
/// [`SETTLES_WITHIN`] has passed.
///
/// The CONDITION is what is waited for — a status that is not still in progress — rather than
/// a fixed span, so the suite answers as soon as the stack has settled however fast or slow
/// the host is.
pub(crate) async fn settled_stack(
    router: &MultiServiceRouter,
    stack_name: &str,
    case: &str,
) -> String {
    let deadline = Instant::now() + SETTLES_WITHIN;
    loop {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body \
             {described}"
        );
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack was still {reported:?} when the wait ran out"
        );
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// What each resource of the stack reported for itself, as `[LogicalId STATUS: reason]` lines:
/// the per-resource reason a deployment that did not complete failed with, which is what names
/// the member the API would not take.
pub(crate) async fn resource_reasons(router: &MultiServiceRouter, stack_name: &str) -> String {
    let (_, events) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack_name)]).await;
    events
        .split("<member>")
        .skip(1)
        .filter_map(|member| {
            let reason = opt_element(member, "ResourceStatusReason")?;
            let logical = opt_element(member, "LogicalResourceId").unwrap_or_default();
            let status = opt_element(member, "ResourceStatus").unwrap_or_default();
            Some(format!("[{logical} {status}: {reason}]"))
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// One row of the table: a template, and what the rule says the distribution it declares must
/// then be in CloudFront, both as created and after the stack has been updated.
struct Row {
    case: &'static str,
    /// The template body, built from the two bucket names the stack creates.
    template: fn(&str, &str) -> String,
    /// The `DomainName` the configuration's single origin must report, for the value
    /// [`ACTIVE_BUCKET_PARAMETER`] carries ([`BUCKET_A`] on the create, [`BUCKET_B`] on the
    /// update) — the one row whose origin reads that parameter repoints, the others do not.
    expected_origin_domain: fn(&str, &str, &str) -> String,
    /// The `HTTPPort` the origin's custom origin configuration must report, or `None` for a
    /// row whose origin is an S3 origin and carries no custom origin configuration at all.
    expected_http_port: Option<&'static str>,
    /// The `HTTPSPort` it must report.
    expected_https_port: Option<&'static str>,
    /// The `PriceClass` the configuration must report, or `None` for a configuration that
    /// must carry none — a multi-tenant distribution, which the member does not support.
    expected_price_class: Option<&'static str>,
}

/// The template of the origin-repointing row: two private buckets, an origin access control,
/// and one distribution whose single origin's `DomainName` is resolved from the stack's own
/// `ActiveBucket` parameter — the blue/green origin cutover a stack update performs. It
/// declares no price class.
pub(crate) fn repointed_origin_template(bucket_a: &str, bucket_b: &str) -> String {
    serde_json::json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Parameters": {
            ACTIVE_BUCKET_PARAMETER: {
                "Type": "String",
                "Default": BUCKET_A,
                "AllowedValues": [BUCKET_A, BUCKET_B]
            },
            COMMENT_PARAMETER: {"Type": "String", "Default": COMMENT_ON_CREATE}
        },
        "Conditions": {
            "IsBucketA": {"Fn::Equals": [{"Ref": ACTIVE_BUCKET_PARAMETER}, BUCKET_A]}
        },
        "Resources": {
            "BucketA": {"Type": "AWS::S3::Bucket", "Properties": {"BucketName": bucket_a}},
            "BucketB": {"Type": "AWS::S3::Bucket", "Properties": {"BucketName": bucket_b}},
            "Oac": {
                "Type": "AWS::CloudFront::OriginAccessControl",
                "Properties": {"OriginAccessControlConfig": {
                    "Name": "repointed-origin-access",
                    "SigningProtocol": "sigv4",
                    "SigningBehavior": "always",
                    "OriginAccessControlOriginType": "s3"
                }}
            },
            DISTRIBUTION_LOGICAL_ID: {
                "Type": "AWS::CloudFront::Distribution",
                "Properties": {"DistributionConfig": {
                    "Enabled": true,
                    "Comment": {"Ref": COMMENT_PARAMETER},
                    "DefaultRootObject": "index.html",
                    "Origins": [{
                        "Id": "active",
                        "DomainName": {"Fn::If": [
                            "IsBucketA",
                            {"Fn::GetAtt": ["BucketA", "RegionalDomainName"]},
                            {"Fn::GetAtt": ["BucketB", "RegionalDomainName"]}
                        ]},
                        "S3OriginConfig": {"OriginAccessIdentity": ""},
                        "OriginAccessControlId": {"Ref": "Oac"}
                    }],
                    "DefaultCacheBehavior": {
                        "TargetOriginId": "active",
                        "ViewerProtocolPolicy": "allow-all"
                    }
                }}
            }
        }
    })
    .to_string()
}

/// Which member a row declares a MULTI-TENANT distribution with — the two ways a declaration
/// says a configuration is one, either of which makes the members only a standard distribution
/// has (the price class among them) members CloudFront refuses on that configuration.
#[derive(Debug, Clone, Copy)]
enum Tenancy {
    /// `ConnectionMode: tenant-only`, the member that states what the distribution IS.
    ConnectionMode,
    /// A `TenantConfig` and no mode at all: the member "only supports multi-tenant
    /// distributions", so declaring one says the same thing.
    TenantConfig,
}

impl Tenancy {
    /// The `(member, value)` the declaration carries for this way of saying it.
    fn declaration(self) -> (&'static str, serde_json::Value) {
        match self {
            Self::ConnectionMode => (
                "ConnectionMode",
                serde_json::Value::String("tenant-only".to_string()),
            ),
            Self::TenantConfig => (
                "TenantConfig",
                serde_json::json!({"ParameterDefinitions": [{
                    "Name": "tenantName",
                    "Definition": {"StringSchema": {
                        "Required": false,
                        "Comment": "the tenant's own name",
                        "DefaultValue": "demo"
                    }}
                }]}),
            ),
        }
    }
}

/// The template of a custom-origin row: one distribution over the bucket's website endpoint,
/// with the ports and the price class the row's fragments state (each merged into the
/// declaration), so a row can state one port, the other, or neither — and, where the row names
/// one, the member that makes the configuration a MULTI-TENANT distribution's: its
/// `ConnectionMode`, or the `TenantConfig` that describes its tenants.
fn custom_origin_template(
    bucket_a: &str,
    ports: serde_json::Value,
    price_class: Option<&str>,
    tenancy: Option<Tenancy>,
) -> String {
    let mut custom_origin_config = serde_json::json!({
        "OriginProtocolPolicy": "https-only",
        "OriginSSLProtocols": ["TLSv1.2"]
    });
    let members = custom_origin_config
        .as_object_mut()
        .expect("a JSON object");
    for (name, value) in ports.as_object().expect("a JSON object") {
        members.insert(name.clone(), value.clone());
    }
    let mut config = serde_json::json!({
        "Enabled": true,
        "Comment": {"Ref": COMMENT_PARAMETER},
        "Origins": [{
            "Id": "the-site",
            "DomainName": format!("{bucket_a}.s3-website-{REGION}.amazonaws.com"),
            "CustomOriginConfig": custom_origin_config
        }],
        "DefaultCacheBehavior": {
            "TargetOriginId": "the-site",
            "ViewerProtocolPolicy": "allow-all"
        }
    });
    let members = config.as_object_mut().expect("a JSON object");
    if let Some(price_class) = price_class {
        members.insert(
            "PriceClass".to_string(),
            serde_json::Value::String(price_class.to_string()),
        );
    }
    if let Some(tenancy) = tenancy {
        let (member, value) = tenancy.declaration();
        members.insert(member.to_string(), value);
    }
    serde_json::json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Parameters": {
            ACTIVE_BUCKET_PARAMETER: {
                "Type": "String",
                "Default": BUCKET_A,
                "AllowedValues": [BUCKET_A, BUCKET_B]
            },
            COMMENT_PARAMETER: {"Type": "String", "Default": COMMENT_ON_CREATE}
        },
        "Resources": {
            "BucketA": {"Type": "AWS::S3::Bucket", "Properties": {"BucketName": bucket_a}},
            DISTRIBUTION_LOGICAL_ID: {
                "Type": "AWS::CloudFront::Distribution",
                "Properties": {"DistributionConfig": config}
            }
        }
    })
    .to_string()
}

/// The website endpoint of the row's own bucket: the host every custom-origin row's origin
/// names, whatever the `ActiveBucket` parameter says — only the repointing row reads that
/// parameter.
fn website_endpoint(bucket_a: &str, _bucket_b: &str, _active: &str) -> String {
    format!("{bucket_a}.s3-website-{REGION}.amazonaws.com")
}

/// The rows.
const ROWS: &[Row] = &[
    Row {
        case: "an S3 origin repointed by a stack parameter, declaring no price class",
        template: repointed_origin_template,
        expected_origin_domain: |bucket_a, bucket_b, active| {
            let bucket = if active == BUCKET_A { bucket_a } else { bucket_b };
            format!("{bucket}.s3.{REGION}.amazonaws.com")
        },
        expected_http_port: None,
        expected_https_port: None,
        expected_price_class: Some("PriceClass_All"),
    },
    Row {
        case: "a custom origin declaring neither port",
        template: |bucket_a, _bucket_b| {
            custom_origin_template(bucket_a, serde_json::json!({}), None, None)
        },
        expected_origin_domain: website_endpoint,
        expected_http_port: Some("80"),
        expected_https_port: Some("443"),
        expected_price_class: Some("PriceClass_All"),
    },
    Row {
        case: "a custom origin declaring only the HTTPS port",
        template: |bucket_a, _bucket_b| {
            custom_origin_template(bucket_a, serde_json::json!({"HTTPSPort": 8443}), None, None)
        },
        expected_origin_domain: website_endpoint,
        expected_http_port: Some("80"),
        expected_https_port: Some("8443"),
        expected_price_class: Some("PriceClass_All"),
    },
    Row {
        case: "a custom origin declaring only the HTTP port",
        template: |bucket_a, _bucket_b| {
            custom_origin_template(bucket_a, serde_json::json!({"HTTPPort": 8080}), None, None)
        },
        expected_origin_domain: website_endpoint,
        expected_http_port: Some("8080"),
        expected_https_port: Some("443"),
        expected_price_class: Some("PriceClass_All"),
    },
    Row {
        case: "a configuration declaring a price class of its own",
        template: |bucket_a, _bucket_b| {
            custom_origin_template(
                bucket_a,
                serde_json::json!({"HTTPPort": 80, "HTTPSPort": 443}),
                Some("PriceClass_100"),
                None,
            )
        },
        expected_origin_domain: website_endpoint,
        expected_http_port: Some("80"),
        expected_https_port: Some("443"),
        expected_price_class: Some("PriceClass_100"),
    },
    Row {
        case: "a multi-tenant distribution declared by its connection mode, which supports no \
               price class",
        template: |bucket_a, _bucket_b| {
            custom_origin_template(
                bucket_a,
                serde_json::json!({}),
                None,
                Some(Tenancy::ConnectionMode),
            )
        },
        expected_origin_domain: website_endpoint,
        expected_http_port: Some("80"),
        expected_https_port: Some("443"),
        expected_price_class: None,
    },
    Row {
        case: "a multi-tenant distribution declared by its tenant configuration alone, naming \
               no connection mode",
        template: |bucket_a, _bucket_b| {
            custom_origin_template(
                bucket_a,
                serde_json::json!({}),
                None,
                Some(Tenancy::TenantConfig),
            )
        },
        expected_origin_domain: website_endpoint,
        expected_http_port: Some("80"),
        expected_https_port: Some("443"),
        expected_price_class: None,
    },
];

/// Every row: the stack deploys the distribution its template declares without the members
/// the API demands and the type publishes as optional, CloudFront holds the documented value
/// for each of them, and the stack UPDATE — which submits the configuration whole — completes
/// and leaves those values where they were.
#[tokio::test]
async fn a_stack_deploys_and_updates_a_distribution_whose_template_omits_demanded_members() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack_name = format!("cloudfront-supplied-members-{index}");
        let bucket_a = format!("cloudfront-supplied-members-{index}-a");
        let bucket_b = format!("cloudfront-supplied-members-{index}-b");
        let template = (row.template)(&bucket_a, &bucket_b);

        let (status, body) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", &template),
                ("Parameters.member.1.ParameterKey", ACTIVE_BUCKET_PARAMETER),
                ("Parameters.member.1.ParameterValue", BUCKET_A),
                ("Parameters.member.2.ParameterKey", COMMENT_PARAMETER),
                ("Parameters.member.2.ParameterValue", COMMENT_ON_CREATE),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );
        let described = settled_stack(&router, &stack_name, case).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {:?} \u{2014} {}",
            opt_element(&described, "StackStatus"),
            resource_reasons(&router, &stack_name).await
        );

        let (status, resources) = cloudformation(
            &router,
            "DescribeStackResources",
            &[("StackName", &stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             body {resources}"
        );
        let distribution_id = physical_ids_of(&resources)
            .get(DISTRIBUTION_LOGICAL_ID)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: no {DISTRIBUTION_LOGICAL_ID} in {resources}")
            });

        assert_distribution(
            &router,
            case,
            "as created",
            &distribution_id,
            row,
            &(row.expected_origin_domain)(&bucket_a, &bucket_b, BUCKET_A),
        )
        .await;

        // The UPDATE submits the configuration WHOLE, so it is where a member the handler
        // supplies decides whether the deployment completes at all: the same declaration,
        // with the parameters the row changes.
        let (status, updated) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", &stack_name),
                ("UsePreviousTemplate", "true"),
                ("Parameters.member.1.ParameterKey", ACTIVE_BUCKET_PARAMETER),
                ("Parameters.member.1.ParameterValue", BUCKET_B),
                ("Parameters.member.2.ParameterKey", COMMENT_PARAMETER),
                ("Parameters.member.2.ParameterValue", COMMENT_ON_UPDATE),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {updated}"
        );
        let described = settled_stack(&router, &stack_name, case).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("UPDATE_COMPLETE"),
            "case {case:?}: expected the stack to reach UPDATE_COMPLETE, got {:?} \u{2014} {}",
            opt_element(&described, "StackStatus"),
            resource_reasons(&router, &stack_name).await
        );

        assert_distribution(
            &router,
            case,
            "after the update",
            &distribution_id,
            row,
            &(row.expected_origin_domain)(&bucket_a, &bucket_b, BUCKET_B),
        )
        .await;

        // The change the update carried reached the distribution, which is what says the
        // configuration was really written rather than left as the create stored it.
        let (_, distribution) = get_distribution(&router, &distribution_id).await;
        let config = block_of(&distribution, "DistributionConfig").unwrap_or_default();
        assert_eq!(
            opt_element(config, "Comment").as_deref(),
            Some(COMMENT_ON_UPDATE),
            "case {case:?}: expected the changed comment to reach CloudFront, got {config}"
        );
    }
}

/// What CloudFront must hold for the row's distribution at this point of the deployment: the
/// origin's domain name and ports, and the configuration's price class.
async fn assert_distribution(
    router: &MultiServiceRouter,
    case: &str,
    when: &str,
    distribution_id: &str,
    row: &Row,
    expected_origin_domain: &str,
) {
    let (status, distribution) = get_distribution(router, distribution_id).await;
    assert_eq!(
        status, 200,
        "case {case:?} ({when}): expected HTTP 200 from GetDistribution, got {status} with \
         body {distribution}"
    );
    let config = block_of(&distribution, "DistributionConfig").unwrap_or_default();
    let origins = block_of(config, "Origins").unwrap_or_default();
    assert_eq!(
        opt_element(origins, "DomainName").as_deref(),
        Some(expected_origin_domain),
        "case {case:?} ({when}): expected the origin {expected_origin_domain:?}, got {origins}"
    );
    let custom_origin = block_of(origins, "CustomOriginConfig");
    for (member, expected) in [
        ("HTTPPort", row.expected_http_port),
        ("HTTPSPort", row.expected_https_port),
    ] {
        let reported = custom_origin.and_then(|custom| opt_element(custom, member));
        assert_eq!(
            reported.as_deref(),
            expected,
            "case {case:?} ({when}): expected {member} {expected:?}, got {reported:?} in \
             {origins}"
        );
    }
    // `PriceClass` follows the `Logging` element, and a configuration carries other members
    // of that name nowhere else, so the configuration's own is read from the whole document.
    //
    // Read AFTER the update is what this fix turns on: CloudFront resolves a missing price
    // class to `PriceClass_All` when it STORES a create, so the create-side reading of this
    // member holds whether or not the document carried it — what the document carried is what
    // decides whether the UPDATE is taken at all, and the row's `UPDATE_COMPLETE` assertion
    // and these port readings are the rest of the proof.
    let reported = opt_element(config, "PriceClass");
    assert_eq!(
        reported.as_deref(),
        row.expected_price_class,
        "case {case:?} ({when}): expected PriceClass {:?}, got {reported:?} in {config}",
        row.expected_price_class
    );
}
