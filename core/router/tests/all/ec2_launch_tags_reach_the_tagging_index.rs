// SPDX-License-Identifier: CC0-1.0

//! Every EC2 resource the account TAGGED is a row of the Resource Groups Tagging index —
//! the ones a create tagged through its own `TagSpecifications`, and the ones a LAUNCH
//! created and tagged beside the instance.
//!
//! The rule under test: **the tag set the service that OWNS a resource holds for it is what
//! the tagging index reports for that resource, keyed by the ARN that service mints for it**
//! — so `GetResources` answers one `ResourceTagMapping` per tagged resource of the Region,
//! selected by its `TagFilters` (AND-ed across filters, each filter's `Values` OR-ed, a
//! filter with no values matching the key with any value) and narrowed by
//! `ResourceTypeFilters` in the `service[:resourceType]` spelling the ARN carries. A
//! resource carrying no tag is in no answer: "GetResources does not return untagged
//! resources" (that operation's documentation).
//!
//! For Amazon EC2 that tag set is written in two ways, and both have to reach the index: a
//! create's own `TagSpecifications` (a VPC, a subnet, a security group) and a LAUNCH's — "the
//! tags to apply to the resources that are created during instance launch"
//! (`RunInstancesRequest$TagSpecifications`), which tags the instance, the EBS volume the
//! launch creates for it and the network interface it creates, each from the entries of its
//! own `ResourceType`.
//!
//! Two crates have to agree for that to hold — Amazon EC2 owns and stores the tags, the
//! tagging API projects them — so the suite lives here, on a router with both services
//! mounted on ONE engine, which is how the emulator runs.
//!
//! The queries span the filter forms: a key with no value, a key with one value, a key with
//! several, the bare service token, one qualified type, several qualified types, a type
//! nothing answers to, and no filter at all. The resources span six kinds and both ways a
//! tag reaches one.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2 as owner_ec2;
use service_com_amazonaws_resourcegroupstaggingapi_resourcegroupstaggingapi_20170126 as tagging_api;

use super::query_wire::{element, form_encode};

/// The Region the estate is created in and the Region the index is queried in.
const REGION: &str = "us-west-2";

/// The account segment every ARN this emulator mints carries.
const ACCOUNT_ID: &str = engine::EMULATOR_ACCOUNT_ID;

/// The EC2 API version an ec2Query body carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// The SigV4 signing names the router routes a request to a service by.
const TAGGING: &str = "tagging";
const EC2: &str = "ec2";

/// The tag key every resource of the estate carries, and the value each carries for it — one
/// key, distinct values, so a filter on the key selects the whole estate and a filter on one
/// value selects one resource.
const KEY: &str = "Name";

/// The router carrying Amazon EC2 and the tagging API over ONE engine, the arrangement
/// `core/router` builds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises handler behaviour, not SigV4: the `Authorization` header below is
    // present because its credential scope is what routes a request to a signing name and
    // names the Region, not to be verified.
    router.set_request_authentication(false);
    router.register_service(owner_ec2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(tagging_api::EmulatedService::new(engine));
    router
}

/// The credential scope that routes a request to `signing_name` in [`REGION`].
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One Amazon EC2 call over its ec2Query wire: `Action=<action>` with `params` as extra form
/// fields, which must answer 200.
async fn ec2_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(EC2))
        .body(Bytes::from(body))
        .expect("a well-formed ec2Query request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answer = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {answer}"
    );
    answer
}

/// One tagging-API call, which must answer 200, as its parsed body.
async fn tagging_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("ResourceGroupsTaggingAPI_20170126.{operation}"),
        )
        .header("authorization", authorization(TAGGING))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answer: Value = serde_json::from_slice(response.body()).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: {operation} answered a body that is not JSON ({error}): {}",
            String::from_utf8_lossy(response.body())
        )
    });
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with body {answer}"
    );
    answer
}

/// One `TagSpecification` form entry at `index`, naming `resource_type` and one `KEY` tag.
fn tag_specification(index: usize, resource_type: &str, value: &str) -> Vec<(String, String)> {
    vec![
        (
            format!("TagSpecification.{index}.ResourceType"),
            resource_type.to_string(),
        ),
        (format!("TagSpecification.{index}.Tag.1.Key"), KEY.to_string()),
        (
            format!("TagSpecification.{index}.Tag.1.Value"),
            value.to_string(),
        ),
    ]
}

/// One form field.
fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// The ARN Amazon EC2 mints for one resource of this account in [`REGION`].
fn arn(resource_type: &str, resource_id: &str) -> String {
    format!("arn:aws:ec2:{REGION}:{ACCOUNT_ID}:{resource_type}/{resource_id}")
}

/// The `(ARN -> tags)` map one `GetResources` answer reports.
fn reported(case: &str, body: &Value) -> BTreeMap<String, BTreeMap<String, String>> {
    body["ResourceTagMappingList"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no ResourceTagMappingList in {body}"))
        .iter()
        .map(|mapping| {
            let arn = mapping["ResourceARN"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a mapping with no ResourceARN: {mapping}"))
                .to_string();
            let tags = mapping["Tags"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: a mapping with no Tags: {mapping}"))
                .iter()
                .map(|tag| {
                    (
                        tag["Key"].as_str().unwrap_or_default().to_string(),
                        tag["Value"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            (arn, tags)
        })
        .collect()
}

/// The ARNs one `GetResources` request selects, in ARN order.
async fn selected(router: &MultiServiceRouter, case: &str, request: Value) -> Vec<String> {
    let body = tagging_ok(router, case, "GetResources", request).await;
    reported(case, &body).into_keys().collect()
}

/// The `attachment.instance-id` filter fields that narrow a report to the resources attached
/// to one instance.
fn attached_to(instance_id: &str) -> Vec<(String, String)> {
    vec![
        p("Filter.1.Name", "attachment.instance-id"),
        p("Filter.1.Value.1", instance_id),
    ]
}

/// The estate this suite tags: one resource of each kind, each carrying `KEY` with a value of
/// its own, half of them tagged by their own create and half by the launch.
struct Estate {
    router: MultiServiceRouter,
    /// `(ARN, value of KEY)` per resource, in creation order.
    resources: Vec<(String, String)>,
    /// A subnet of the same VPC that NOTHING tagged.
    untagged: String,
}

impl Estate {
    /// Create and tag the estate.
    async fn create(case: &str) -> Self {
        let router = emulator();
        let mut params = vec![p("CidrBlock", "10.63.0.0/16")];
        params.extend(tag_specification(1, "vpc", "probe-vpc"));
        let vpc = element(&ec2_ok(&router, case, "CreateVpc", &params).await, "vpcId");

        let mut params = vec![
            p("VpcId", &vpc),
            p("CidrBlock", "10.63.1.0/24"),
            p("AvailabilityZone", &format!("{REGION}a")),
        ];
        params.extend(tag_specification(1, "subnet", "probe-subnet"));
        let subnet = element(
            &ec2_ok(&router, case, "CreateSubnet", &params).await,
            "subnetId",
        );

        let mut params = vec![
            p("VpcId", &vpc),
            p("GroupName", "probe-group"),
            p("GroupDescription", "a group the tagging index must report"),
        ];
        params.extend(tag_specification(1, "security-group", "probe-group"));
        let group = element(
            &ec2_ok(&router, case, "CreateSecurityGroup", &params).await,
            "groupId",
        );

        // One launch, tagging each kind of resource it creates from the entries of that
        // kind's own type.
        let mut params = vec![
            p("ImageId", "ami-0123456789abcdef0"),
            p("MinCount", "1"),
            p("MaxCount", "1"),
            p("SubnetId", &subnet),
        ];
        params.extend(tag_specification(1, "instance", "probe-instance"));
        params.extend(tag_specification(2, "volume", "probe-volume"));
        params.extend(tag_specification(3, "network-interface", "probe-eni"));
        let instance = element(
            &ec2_ok(&router, case, "RunInstances", &params).await,
            "instanceId",
        );

        // The volume and the interface the launch created, read back out of Amazon EC2's own
        // reports rather than guessed. The volume reports the ARN that service MINTED for it
        // (`Volume$VolumeArn`), which is the key the index must report it under; an interface
        // carries no ARN member, so its ARN is the one its namespace, Region, account and
        // `<type>/<id>` pair make.
        let volumes = ec2_ok(&router, case, "DescribeVolumes", &attached_to(&instance)).await;
        let volume_arn = element(&volumes, "volumeArn");
        let interfaces = ec2_ok(
            &router,
            case,
            "DescribeNetworkInterfaces",
            &attached_to(&instance),
        )
        .await;
        let interface = element(&interfaces, "networkInterfaceId");

        // A resource of the same VPC that NOTHING tagged: "GetResources does not return
        // untagged resources".
        let untagged = element(
            &ec2_ok(
                &router,
                case,
                "CreateSubnet",
                &[
                    p("VpcId", &vpc),
                    p("CidrBlock", "10.63.2.0/24"),
                    p("AvailabilityZone", &format!("{REGION}a")),
                ],
            )
            .await,
            "subnetId",
        );

        Self {
            router,
            resources: vec![
                (arn("vpc", &vpc), "probe-vpc".to_string()),
                (arn("subnet", &subnet), "probe-subnet".to_string()),
                (arn("security-group", &group), "probe-group".to_string()),
                (arn("instance", &instance), "probe-instance".to_string()),
                (volume_arn, "probe-volume".to_string()),
                (
                    arn("network-interface", &interface),
                    "probe-eni".to_string(),
                ),
            ],
            untagged: arn("subnet", &untagged),
        }
    }

    /// The ARN of the resource whose `KEY` value is `value`.
    fn arn_of(&self, value: &str) -> String {
        self.resources
            .iter()
            .find(|(_, tag_value)| tag_value == value)
            .map(|(arn, _)| arn.clone())
            .unwrap_or_else(|| panic!("the estate holds no resource tagged {value:?}"))
    }

    /// Every ARN of the estate, in ARN order.
    fn every_arn(&self) -> Vec<String> {
        let mut arns: Vec<String> = self.resources.iter().map(|(arn, _)| arn.clone()).collect();
        arns.sort();
        arns
    }

    /// The ARNs of the resources whose `KEY` values are `values`, in ARN order.
    fn arns_of(&self, values: &[&str]) -> Vec<String> {
        let mut arns: Vec<String> = values.iter().map(|value| self.arn_of(value)).collect();
        arns.sort();
        arns
    }
}

/// THE RULE: every tagged resource of the estate is a row of the index, carrying the tags its
/// owning service holds for it, and no untagged resource is.
#[tokio::test]
async fn every_tagged_ec2_resource_is_a_row_of_the_tagging_index() {
    let case = "the tagged EC2 estate";
    let estate = Estate::create(case).await;

    let body = tagging_ok(
        &estate.router,
        case,
        "GetResources",
        json!({ "TagFilters": [{ "Key": KEY }] }),
    )
    .await;
    let index = reported(case, &body);

    for (arn, value) in &estate.resources {
        let tags = index
            .get(arn)
            .unwrap_or_else(|| panic!("case {case:?}: the index must report {arn}, got {index:?}"));
        let expected: BTreeMap<String, String> =
            [(KEY.to_string(), value.clone())].into_iter().collect();
        assert_eq!(
            tags, &expected,
            "case {case:?}: expected {arn} to carry {expected:?}, got {tags:?}"
        );
    }
    let selected: Vec<String> = index.keys().cloned().collect();
    assert_eq!(
        selected,
        estate.every_arn(),
        "case {case:?}: expected the index to report exactly the tagged estate"
    );
    assert!(
        !index.contains_key(&estate.untagged),
        "case {case:?}: GetResources must not report the untagged {}, got {index:?}",
        estate.untagged
    );
}

/// THE RULE's selection: each filter form selects exactly the resources it names, over the
/// one estate.
#[tokio::test]
async fn the_filters_select_the_resources_they_name() {
    let case = "the tagged EC2 estate";
    let estate = Estate::create(case).await;
    let rows: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "no filter at all",
            json!({}),
            estate.every_arn(),
        ),
        (
            "the tag key with one value",
            json!({ "TagFilters": [{ "Key": KEY, "Values": ["probe-volume"] }] }),
            estate.arns_of(&["probe-volume"]),
        ),
        (
            "the tag key with three values",
            json!({ "TagFilters": [{
                "Key": KEY,
                "Values": ["probe-volume", "probe-eni", "probe-instance"],
            }] }),
            estate.arns_of(&["probe-volume", "probe-eni", "probe-instance"]),
        ),
        (
            "a tag key nothing carries",
            json!({ "TagFilters": [{ "Key": "no-such-key" }] }),
            Vec::new(),
        ),
        (
            "two filters, one of which nothing carries",
            json!({ "TagFilters": [{ "Key": KEY }, { "Key": "no-such-key" }] }),
            Vec::new(),
        ),
        (
            "the bare service token",
            json!({ "ResourceTypeFilters": ["ec2"] }),
            estate.every_arn(),
        ),
        (
            "one qualified resource type",
            json!({ "ResourceTypeFilters": ["ec2:volume"] }),
            estate.arns_of(&["probe-volume"]),
        ),
        (
            "two qualified resource types",
            json!({ "ResourceTypeFilters": ["ec2:vpc", "ec2:subnet"] }),
            estate.arns_of(&["probe-vpc", "probe-subnet"]),
        ),
        (
            "the resource type the launch created beside the instance",
            json!({ "ResourceTypeFilters": ["ec2:network-interface"] }),
            estate.arns_of(&["probe-eni"]),
        ),
        (
            "a resource type nothing of the estate answers to",
            json!({ "ResourceTypeFilters": ["ec2:snapshot"] }),
            Vec::new(),
        ),
        (
            "a resource type narrowed by a tag value",
            json!({
                "ResourceTypeFilters": ["ec2"],
                "TagFilters": [{ "Key": KEY, "Values": ["probe-eni"] }],
            }),
            estate.arns_of(&["probe-eni"]),
        ),
        (
            "the ARN list naming two resources",
            json!({ "ResourceARNList": [
                estate.arn_of("probe-volume"),
                estate.arn_of("probe-subnet"),
            ] }),
            estate.arns_of(&["probe-volume", "probe-subnet"]),
        ),
    ];

    for (row, request, expected) in rows {
        let answer = selected(&estate.router, row, request.clone()).await;
        assert_eq!(
            answer, expected,
            "case {row:?}: expected GetResources {request} to select {expected:?}, got \
             {answer:?}"
        );
    }
}
