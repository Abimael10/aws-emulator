// SPDX-License-Identifier: CC0-1.0

//! A stack's declared `AWS::CloudFront::VpcOrigin` is a VPC origin Amazon CloudFront really
//! holds, addressable by the identifier the stack publishes for it.
//!
//! The rule under test: **a resource type CloudFormation publishes is one a template may
//! declare — the `Template format error: Unrecognized resource types: [<types>]` refusal is
//! reserved for a type CloudFormation does not know — and a declared type this emulator
//! provisions is created by calling the OWNING service's own create, so the physical resource
//! id the stack publishes is the identifier that service minted, its `Fn::GetAtt` values are
//! the ones that service reports, and the stack's delete takes the resource away there.** For a
//! VPC origin the create is `CreateVpcOrigin` (`POST /2020-05-31/vpc-origin`), the read is
//! `GetVpcOrigin` (`GET /2020-05-31/vpc-origin/{Id}`), which answers 200 with the stored
//! `VpcOrigin`, and `EntityNotFound` (HTTP 404) is modelled ONLY for an id that names no VPC
//! origin — so a 404 for an id CloudFormation itself reported is a defect.
//!
//! The rows differ along what the rule generalises over: both ports declared and both left out
//! (so the defaults the model documents for them are asserted at and past their boundary), each
//! value `OriginProtocolPolicy` admits, an `OriginSSLProtocols` wrapper absent and carrying one
//! and several protocols, and whether the declaration carries tags. Every expected value is
//! computed here from what the row declares, never read back out of the answer being checked.
//!
//! The endpoint each origin fronts is an INTERNAL Application Load Balancer the same template
//! declares, named by `!Ref`, so the configuration CloudFront stores is bound to a load
//! balancer Elastic Load Balancing really holds rather than to a hand-written ARN.
//!
//! Everything goes over the wire each service speaks — awsQuery for CloudFormation and Elastic
//! Load Balancing, restXml for Amazon CloudFront — through the one router `harness.rs` builds,
//! which mounts them all on one engine.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{cloudfront, physical_resource_id, settled_stack, CLOUDFRONT_API_VERSION};
use crate::stack_lifecycle::{
    call, create_stack, member, output_value, router, xml_blocks, xml_text, xml_texts,
    ACCOUNT_ID, REGION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::status;

/// The identifier label CloudFront writes a VPC origin id under, as the model's own VPC origin
/// examples spell one.
const VPC_ORIGIN_ID_LABEL: &str = "vo_";

/// The resource type a VPC origin's ARN spells, as the model's own VPC origin examples write it
/// (`arn:aws:cloudfront::<account>:vpcorigin/<Id>`).
const VPC_ORIGIN_ARN_TYPE: &str = "vpcorigin";

/// The ports CloudFront's model documents for an endpoint configuration that states neither:
/// `VpcOriginEndpointConfig$HTTPPort` "The default value is `80`" and `$HTTPSPort` "The default
/// value is `443`" (`smithy_query(query="shape",shape="VpcOriginEndpointConfig")`).
const DOCUMENTED_HTTP_PORT: i64 = 80;
const DOCUMENTED_HTTPS_PORT: i64 = 443;

// ---------------------------------------------------------------------------
// The declarations under test
// ---------------------------------------------------------------------------

/// What a row's `AWS::CloudFront::VpcOrigin` declares.
#[derive(Debug, Clone)]
struct Declared {
    case: &'static str,
    stack: &'static str,
    /// The `Name` the load balancer the origin fronts is created under, which is short enough
    /// for the name an Elastic Load Balancing ARN carries.
    load_balancer: &'static str,
    /// `VpcOriginEndpointConfig$Name`: the name the account knows the origin by.
    name: &'static str,
    /// The two ports, where the declaration states them.
    http_port: Option<i64>,
    https_port: Option<i64>,
    /// One of `OriginProtocolPolicy`'s values (`http-only`, `match-viewer`, `https-only`, from
    /// `smithy_query(query="shape",shape="OriginProtocolPolicy")`).
    origin_protocol_policy: &'static str,
    /// The `SslProtocol` values the configuration lists, or `None` where it names no
    /// `OriginSSLProtocols` wrapper at all (`SSLv3`, `TLSv1`, `TLSv1.1`, `TLSv1.2`, from
    /// `smithy_query(query="shape",shape="SslProtocol")`).
    ssl_protocols: Option<&'static [&'static str]>,
    /// The tags the declaration asks for, as `(Key, Value)`.
    tags: &'static [(&'static str, &'static str)],
}

impl Declared {
    /// The `HTTPPort` the origin is reached on: the one declared, or the documented default.
    fn effective_http_port(&self) -> i64 {
        self.http_port.unwrap_or(DOCUMENTED_HTTP_PORT)
    }

    /// The `HTTPSPort` the origin is reached on, by the same rule.
    fn effective_https_port(&self) -> i64 {
        self.https_port.unwrap_or(DOCUMENTED_HTTPS_PORT)
    }

    /// The whole template this row deploys: the network, the internal Application Load Balancer
    /// the origin fronts, the VPC origin itself, and the outputs a client reads the deployment's
    /// identifiers out of.
    fn template(&self) -> String {
        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": {
                "Vpc": {
                    "Type": "AWS::EC2::VPC",
                    "Properties": {"CidrBlock": "10.40.0.0/16"},
                },
                "SubnetA": {
                    "Type": "AWS::EC2::Subnet",
                    "Properties": {
                        "VpcId": {"Ref": "Vpc"},
                        "CidrBlock": "10.40.1.0/24",
                        "AvailabilityZone": "us-east-1a",
                    },
                },
                "SubnetB": {
                    "Type": "AWS::EC2::Subnet",
                    "Properties": {
                        "VpcId": {"Ref": "Vpc"},
                        "CidrBlock": "10.40.2.0/24",
                        "AvailabilityZone": "us-east-1b",
                    },
                },
                "Alb": {
                    "Type": "AWS::ElasticLoadBalancingV2::LoadBalancer",
                    "Properties": {
                        "Name": self.load_balancer,
                        "Type": "application",
                        "Scheme": "internal",
                        "Subnets": [{"Ref": "SubnetA"}, {"Ref": "SubnetB"}],
                    },
                },
                "Origin": {
                    "Type": "AWS::CloudFront::VpcOrigin",
                    "Properties": self.properties(),
                },
            },
            "Outputs": {
                "AlbArn": {"Value": {"Ref": "Alb"}},
                "OriginRef": {"Value": {"Ref": "Origin"}},
                "OriginId": {"Value": {"Fn::GetAtt": ["Origin", "Id"]}},
                "OriginArn": {"Value": {"Fn::GetAtt": ["Origin", "Arn"]}},
                "OriginStatus": {"Value": {"Fn::GetAtt": ["Origin", "Status"]}},
            },
        })
        .to_string()
    }

    /// The declared properties of the VPC origin: the members the row states and no others.
    fn properties(&self) -> Value {
        let mut config = Map::new();
        config.insert("Name".to_string(), json!(self.name));
        // The endpoint the origin fronts is the load balancer the same template declares:
        // "Ref returns the ARN of the load balancer", which is what
        // `VpcOriginEndpointConfig$Arn` names it by.
        config.insert("Arn".to_string(), json!({"Ref": "Alb"}));
        if let Some(port) = self.http_port {
            config.insert("HTTPPort".to_string(), json!(port));
        }
        if let Some(port) = self.https_port {
            config.insert("HTTPSPort".to_string(), json!(port));
        }
        config.insert(
            "OriginProtocolPolicy".to_string(),
            json!(self.origin_protocol_policy),
        );
        if let Some(protocols) = self.ssl_protocols {
            config.insert(
                "OriginSSLProtocols".to_string(),
                json!({"Quantity": protocols.len(), "Items": protocols}),
            );
        }
        let mut properties = Map::new();
        properties.insert(
            "VpcOriginEndpointConfig".to_string(),
            Value::Object(config),
        );
        if !self.tags.is_empty() {
            properties.insert(
                "Tags".to_string(),
                Value::Array(
                    self.tags
                        .iter()
                        .map(|(key, value)| json!({"Key": key, "Value": value}))
                        .collect(),
                ),
            );
        }
        Value::Object(properties)
    }
}

/// The declarations the rule above is asserted over.
fn declarations() -> Vec<Declared> {
    vec![
        Declared {
            case: "both ports at the unprivileged bounds, several SSL protocols, tagged",
            stack: "cfn-vpc-origin-bounds",
            load_balancer: "vo-alb-bounds",
            name: "cfn-vpc-origin-bounds",
            // The lowest and the highest port CloudFront connects to an origin on, past the
            // documented defaults in both directions.
            http_port: Some(1_024),
            https_port: Some(65_535),
            origin_protocol_policy: "https-only",
            ssl_protocols: Some(&["TLSv1.2", "TLSv1.1"]),
            tags: &[("managed-by", "cloudformation"), ("tier", "edge")],
        },
        Declared {
            case: "neither port declared, so the documented defaults travel, no SSL protocols",
            stack: "cfn-vpc-origin-defaults",
            load_balancer: "vo-alb-defaults",
            name: "cfn-vpc-origin-defaults",
            http_port: None,
            https_port: None,
            origin_protocol_policy: "http-only",
            ssl_protocols: None,
            tags: &[],
        },
        Declared {
            case: "the documented ports stated explicitly, one SSL protocol, no tags",
            stack: "cfn-vpc-origin-documented",
            load_balancer: "vo-alb-documented",
            name: "cfn-vpc-origin-documented",
            http_port: Some(DOCUMENTED_HTTP_PORT),
            https_port: Some(DOCUMENTED_HTTPS_PORT),
            origin_protocol_policy: "match-viewer",
            ssl_protocols: Some(&["TLSv1"]),
            tags: &[],
        },
    ]
}

// ---------------------------------------------------------------------------
// Calling the two services
// ---------------------------------------------------------------------------

/// `GET /2020-05-31/vpc-origin/{Id}`.
async fn get_vpc_origin(router: &MultiServiceRouter, id: &str) -> (u16, String) {
    cloudfront(
        router,
        "GET",
        &format!("/{CLOUDFRONT_API_VERSION}/vpc-origin/{id}"),
    )
    .await
}

/// `GET /2020-05-31/tagging?Resource={arn}`.
///
/// The ARN travels as it is: every character a CloudFront ARN uses is legal in a query value,
/// and none of them is `&`, `=` or `%`.
async fn list_tags_for_resource(router: &MultiServiceRouter, arn: &str) -> (u16, String) {
    cloudfront(
        router,
        "GET",
        &format!("/{CLOUDFRONT_API_VERSION}/tagging?Resource={arn}"),
    )
    .await
}

/// Read a stack until it reports a status that is no longer an operation in flight, and answer
/// that read's whole `DescribeStacks` body.
///
/// The ONE waiter this target already has (`harness::settled_stack`), rather than a copy of its
/// poll loop: the WAIT IS FOR THE CONDITION, under that helper's own generous deadline.
async fn settle(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    settled_stack(case, router, REGION, stack).await
}

// ---------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------

/// Every declared VPC origin is accepted, EXISTS in Amazon CloudFront under the physical
/// resource id the stack reports for it, carries the configuration the template declared bound
/// to the load balancer the same stack created, and goes away with the stack.
#[tokio::test]
async fn declared_vpc_origins_are_created_read_and_deleted_in_cloudfront() {
    let router = router();
    let mut ids: BTreeSet<String> = BTreeSet::new();

    for declared in declarations() {
        let case = declared.case;
        let (code, body) = create_stack(&router, declared.stack, &declared.template(), &[]).await;
        // The issue's own observable: a template declaring the type must not be refused as a
        // template whose resource types CloudFormation does not recognise.
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must be accepted, got {code} {body}"
        );
        let stack_id = xml_text(&body, "StackId").unwrap_or_default();
        assert!(
            !stack_id.is_empty(),
            "case {case:?}: CreateStack must answer the stack's id, got {body}"
        );

        let described = settle(&router, case, &stack_id).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::CREATE_COMPLETE),
            "case {case:?}: the stack must reach CREATE_COMPLETE, got {described}"
        );

        let id = physical_resource_id(&router, case, declared.stack, "Origin").await;
        assert!(
            id.starts_with(VPC_ORIGIN_ID_LABEL) && id.len() > VPC_ORIGIN_ID_LABEL.len(),
            "case {case:?}: expected the id CloudFront mints for a VPC origin, got {id:?}"
        );
        assert!(
            ids.insert(id.clone()),
            "case {case:?}: every origin must get an identifier of its own, {id} was already \
             published"
        );

        // The read CloudFormation's physical id is FOR.
        let (code, origin) = get_vpc_origin(&router, &id).await;
        assert_eq!(
            code, 200,
            "case {case:?}: GetVpcOrigin({id}) must answer 200, got {code} {origin}"
        );
        assert_eq!(
            xml_text(&origin, "Id").as_deref(),
            Some(id.as_str()),
            "case {case:?}: GetVpcOrigin must report the published id, got {origin}"
        );

        let expected_arn = format!("arn:aws:cloudfront::{ACCOUNT_ID}:{VPC_ORIGIN_ARN_TYPE}/{id}");
        assert_eq!(
            xml_text(&origin, "Arn").as_deref(),
            Some(expected_arn.as_str()),
            "case {case:?}: expected the ARN CloudFront builds from the id, got {origin}"
        );

        // The stack's published return values are the ones CloudFront reports.
        let album = identifier(case, &described, "AlbArn");
        for (key, expected) in [
            ("OriginRef", id.as_str()),
            ("OriginId", id.as_str()),
            ("OriginArn", expected_arn.as_str()),
        ] {
            assert_eq!(
                identifier(case, &described, key),
                expected,
                "case {case:?}: the {key:?} output must be the value CloudFront minted, got \
                 {described}"
            );
        }
        let reported_status = xml_text(&origin, "Status").unwrap_or_default();
        assert_eq!(
            identifier(case, &described, "OriginStatus"),
            reported_status,
            "case {case:?}: the OriginStatus output must be the status CloudFront reports, got \
             {origin}"
        );

        assert_declared_config(case, &declared, &album, &origin);

        // The tags the declaration asked for are the tags the tagging surface reports for the
        // origin's ARN, so the resource the stack tagged is this one.
        if !declared.tags.is_empty() {
            let (code, tagging) = list_tags_for_resource(&router, &expected_arn).await;
            assert_eq!(
                code, 200,
                "case {case:?}: ListTagsForResource({expected_arn}) must answer 200, got {code} \
                 {tagging}"
            );
            let keys = xml_texts(&tagging, "Key");
            let values = xml_texts(&tagging, "Value");
            for (key, value) in declared.tags {
                let found = keys
                    .iter()
                    .zip(values.iter())
                    .any(|(held, borne)| held == key && borne == value);
                assert!(
                    found,
                    "case {case:?}: expected the declared tag {key}={value} on the origin, got \
                     {tagging}"
                );
            }
        }

        // The teardown: the stack's delete takes the origin away in CloudFront.
        let (code, body) = call(&router, "DeleteStack", &[member("StackName", &stack_id)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: DeleteStack must answer 200, got {code} {body}"
        );
        let described = settle(&router, case, &stack_id).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::DELETE_COMPLETE),
            "case {case:?}: the teardown must reach DELETE_COMPLETE, got {described}"
        );

        let (code, answered) = get_vpc_origin(&router, &id).await;
        assert_eq!(
            code, 404,
            "case {case:?}: expected the origin to be gone with the stack, got {code} {answered}"
        );
        assert_eq!(
            xml_text(&answered, "Code").as_deref(),
            Some("EntityNotFound"),
            "case {case:?}: expected EntityNotFound once the stack deleted it, got {answered}"
        );
    }
}

/// The value one output carries.
fn identifier(case: &str, described: &str, key: &str) -> String {
    output_value(described, key)
        .unwrap_or_else(|| panic!("case {case:?}: no {key:?} output, got {described}"))
}

/// Assert the `VpcOrigin` document reports the endpoint configuration `declared` states, with
/// the ports resolved by the documented rule and the endpoint bound to `load_balancer_arn`.
fn assert_declared_config(
    case: &str,
    declared: &Declared,
    load_balancer_arn: &str,
    document: &str,
) {
    let config = xml_blocks(document, "VpcOriginEndpointConfig")
        .into_iter()
        .next()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a VpcOriginEndpointConfig, got {document}")
        });
    assert_eq!(
        xml_text(&config, "Name").as_deref(),
        Some(declared.name),
        "case {case:?}: the origin must carry the declared Name, got {config}"
    );
    assert_eq!(
        xml_text(&config, "Arn").as_deref(),
        Some(load_balancer_arn),
        "case {case:?}: the endpoint must name the load balancer the stack created, got {config}"
    );
    for (name, expected) in [
        ("HTTPPort", declared.effective_http_port()),
        ("HTTPSPort", declared.effective_https_port()),
    ] {
        assert_eq!(
            xml_text(&config, name).as_deref(),
            Some(expected.to_string().as_str()),
            "case {case:?}: the origin's {name} must be {expected}, got {config}"
        );
    }
    assert_eq!(
        xml_text(&config, "OriginProtocolPolicy").as_deref(),
        Some(declared.origin_protocol_policy),
        "case {case:?}: the origin must carry the declared OriginProtocolPolicy, got {config}"
    );
    match declared.ssl_protocols {
        Some(protocols) => {
            let wrapper = xml_blocks(&config, "OriginSslProtocols")
                .into_iter()
                .next()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected an OriginSslProtocols, got {config}")
                });
            assert_eq!(
                xml_text(&wrapper, "Quantity").as_deref(),
                Some(protocols.len().to_string().as_str()),
                "case {case:?}: the wrapper must state the declared list's length, got {wrapper}"
            );
            assert_eq!(
                xml_texts(&wrapper, "SslProtocol"),
                protocols
                    .iter()
                    .map(|protocol| (*protocol).to_string())
                    .collect::<Vec<String>>(),
                "case {case:?}: the origin must list the declared protocols, got {wrapper}"
            );
        }
        None => assert!(
            xml_blocks(&config, "OriginSslProtocols").is_empty(),
            "case {case:?}: a declaration naming no protocols must store none, got {config}"
        ),
    }
}

/// The complement, so the refusal the issue reported is still the refusal for a type
/// CloudFormation really does not know: a template declaring one is answered `ValidationError`
/// with `Template format error: Unrecognized resource types: [<types>]` and creates nothing.
#[tokio::test]
async fn a_cloudfront_type_cloudformation_does_not_publish_is_still_unrecognized() {
    let router = router();
    // Rows: a misspelling of the type this suite deploys, and a CloudFront resource
    // CloudFormation publishes no type for at all.
    for (case, spelled) in [
        ("a misspelling of the published type", "AWS::CloudFront::VPCOrigin"),
        ("a CloudFront resource with no type", "AWS::CloudFront::VpcOriginNothing"),
    ] {
        let template = json!({
            "Resources": {"Origin": {"Type": spelled, "Properties": {}}}
        })
        .to_string();
        let (code, body) = create_stack(&router, "cfn-vpc-origin-unknown", &template, &[]).await;
        assert_eq!(
            code, 400,
            "case {case:?}: an unrecognized type must be refused, got {code} {body}"
        );
        assert_eq!(
            xml_text(&body, "Code").as_deref(),
            Some("ValidationError"),
            "case {case:?}: the refusal must be a ValidationError, got {body}"
        );
        assert_eq!(
            xml_text(&body, "Message").as_deref(),
            Some(
                format!("Template format error: Unrecognized resource types: [{spelled}]")
                    .as_str()
            ),
            "case {case:?}: the refusal must name the type that is not recognised, got {body}"
        );
    }
}
