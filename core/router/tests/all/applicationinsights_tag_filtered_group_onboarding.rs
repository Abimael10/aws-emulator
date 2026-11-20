// SPDX-License-Identifier: CC0-1.0

//! A resource group whose definition is a `TAG_FILTERS_1_0` resource query really
//! holds the TAGGED RESOURCES OF THE ACCOUNT, whichever emulated service owns
//! them — and an Application Insights application created over such a group
//! onboards exactly those resources as its components.
//!
//! The RULE pinned here: the account's tagged resources are the ones the services
//! that OWN them hold tags for, so a tag query selects a resource that carries
//! its tags however that resource came to carry them — a bucket tagged through
//! Amazon S3's own tagging call, an instance tagged by its launch, a VPC tagged
//! by `CreateTags`, a queue created with tags — and nothing that does not carry
//! them. Creating an application over the group then ONBOARDS every resource the
//! group resolves to as a DEFAULT (standalone) component named by that resource's
//! own ARN and unmonitored, with a resource that is PART OF another onboarded
//! resource (an EBS volume attached to an onboarded instance) carried inside that
//! resource's component rather than being a component of its own; a resource
//! belongs to ONE component, so grouping it into a custom component takes its
//! standalone component away, a second grouping of it is refused, and ungrouping
//! gives it back.
//!
//! Four crates have to agree for this to hold — the services that own the tagged
//! resources, the resource-tagging view of them, Resource Groups, which resolves
//! a query against that view, and Application Insights, which onboards what the
//! group resolves to — so the suite lives here rather than in any one of them.
//! Nothing below reaches into a store: every resource is created through its own
//! service's wire, and every assertion reads what another service reports.
//!
//! The rows of the first test differ along what the rule generalises over: which
//! service owns the resource and which type it is, whether the tag arrived with
//! the resource or afterwards, how many values a filter names, a key named with
//! no value, which types the query admits and how many, and a key nothing in the
//! account carries. The decoys — a resource carrying the same key with another
//! value, and one carrying no tag at all — may appear in no answer.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built —
//! and the requests driven — on a sized thread. The Application Insights wire is
//! `crate::applicationinsights_wire`, shared with the other suites of this
//! service pair.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::applicationinsights_wire::{resource_groups, Answer, BOOT_STACK_SIZE};
use crate::query_wire::{element, elements, form_encode};

/// The Region every request below is signed for, and so the Region the resources,
/// the groups and the application live in. `us-east-1` is the one Region a
/// `CreateBucket` needs no `CreateBucketConfiguration` for.
const REGION: &str = "us-east-1";

/// The EC2 API version an ec2Query body carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// The AMI the launches below run from, in the long form EC2 issues AMI ids in.
const AMI: &str = "ami-0123456789abcdef0";

/// The one query type a tag-filtered group is defined by
/// (`smithy_query(query="shape",shape="QueryType")`).
const TAG_FILTERS: &str = "TAG_FILTERS_1_0";

/// The `ResourceTypeFilters` value that admits every supported type, which is the
/// value a query naming no type restriction carries.
const ALL_SUPPORTED: &str = "AWS::AllSupported";

/// How long a poll keeps looking for the state a call owes. Generous by design:
/// it bounds nothing a row asserts and nothing here measures elapsed time — it
/// only stops a broken emulator from hanging, so a loaded host cannot fail it.
const POLL_DEADLINE: Duration = Duration::from_secs(180);

/// How long a poll waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// A SigV4 `Authorization` header whose credential scope names `signing_name`,
/// which is what routes the request and tells the handler its Region; the
/// signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Where one request goes: the signing name whose credential scope routes it,
/// the method and path it is addressed to, the content type its protocol frames
/// its body in, and the `X-Amz-Target` an RPC protocol names its operation with.
struct Wire<'a> {
    signing_name: &'a str,
    method: &'a str,
    path: &'a str,
    content_type: &'a str,
    target: Option<String>,
}

/// One request through the whole router, as its status and its raw body.
fn dispatch(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    wire: Wire<'_>,
    body: Bytes,
) -> (u16, String) {
    let host = format!("{}.{REGION}.amazonaws.com", wire.signing_name);
    let mut request = http::Request::builder()
        .method(wire.method)
        .uri(format!("https://{host}{}", wire.path))
        .header("host", host)
        .header("content-type", wire.content_type)
        .header("authorization", authorization(wire.signing_name));
    if let Some(target) = &wire.target {
        request = request.header("x-amz-target", target);
    }
    let request = request.body(body).expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// One Amazon S3 call, as its status and the body it answered.
fn s3(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, String) {
    dispatch(
        router,
        runtime,
        Wire {
            signing_name: "s3",
            method,
            path,
            content_type: "application/xml",
            target: None,
        },
        Bytes::from(body.to_string()),
    )
}

/// One Amazon EC2 action over its ec2Query wire, which must be served, answered
/// with its XML body.
fn ec2(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
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
    let (status, answered) = dispatch(
        router,
        runtime,
        Wire {
            signing_name: "ec2",
            method: "POST",
            path: "/",
            content_type: "application/x-www-form-urlencoded",
            target: None,
        },
        Bytes::from(body),
    );
    assert_eq!(
        status, 200,
        "setup: ec2:{action} must be served, got {status} with {answered}"
    );
    answered
}

/// One form field of an ec2Query body.
fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One Amazon SQS call over its awsJson1_0 wire, which must be served, answered
/// with its parsed body.
fn sqs(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    operation: &str,
    body: Value,
) -> Value {
    let (status, answered) = dispatch(
        router,
        runtime,
        Wire {
            signing_name: "sqs",
            method: "POST",
            path: "/",
            content_type: "application/x-amz-json-1.0",
            target: Some(format!("AmazonSQS.{operation}")),
        },
        Bytes::from(body.to_string()),
    );
    assert_eq!(
        status, 200,
        "setup: sqs:{operation} must be served, got {status} with {answered}"
    );
    serde_json::from_str(&answered).unwrap_or(Value::Null)
}

/// One Resource Groups Tagging API call over its awsJson1_1 wire, which must be
/// served, answered with its parsed body.
fn tagging(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    operation: &str,
    body: Value,
) -> Value {
    let (status, answered) = dispatch(
        router,
        runtime,
        Wire {
            signing_name: "tagging",
            method: "POST",
            path: "/",
            content_type: "application/x-amz-json-1.1",
            target: Some(format!(
                "ResourceGroupsTaggingAPI_20170126.{operation}"
            )),
        },
        Bytes::from(body.to_string()),
    );
    assert_eq!(
        status, 200,
        "tagging:{operation} must be served, got {status} with {answered}"
    );
    serde_json::from_str(&answered).unwrap_or(Value::Null)
}

/// One Application Insights operation of [`REGION`], over its awsJson1_1 wire.
fn application_insights(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    operation: &str,
    body: Value,
) -> Answer {
    crate::applicationinsights_wire::application_insights(router, runtime, REGION, operation, body)
}

/// A bucket created through Amazon S3's own `CreateBucket`, with `tags` written
/// onto it afterwards by `PutBucketTagging` — the way a client labels a bucket it
/// already has. Answers the ARN S3 reports such a bucket under.
fn bucket(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
    tags: &[(&str, &str)],
) -> String {
    let (status, body) = s3(router, runtime, "PUT", &format!("/{name}"), "");
    assert_eq!(
        status, 200,
        "setup: CreateBucket({name:?}) must succeed, got {status} with {body}"
    );
    if !tags.is_empty() {
        let mut document = String::from("<Tagging><TagSet>");
        for (key, value) in tags {
            document.push_str(&format!(
                "<Tag><Key>{key}</Key><Value>{value}</Value></Tag>"
            ));
        }
        document.push_str("</TagSet></Tagging>");
        let (status, body) = s3(
            router,
            runtime,
            "PUT",
            &format!("/{name}?tagging"),
            &document,
        );
        assert_eq!(
            status, 204,
            "setup: PutBucketTagging({name:?}) must succeed, got {status} with {body}"
        );
    }
    format!("arn:aws:s3:::{name}")
}

/// One instance launched through Amazon EC2's own `RunInstances`, carrying `tags`
/// in the launch's `TagSpecification` — a tag that arrives WITH the resource.
/// Answers `(instance id, instance ARN)`.
fn instance(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    tags: &[(&str, &str)],
) -> (String, String) {
    let mut params = vec![
        param("ImageId", AMI),
        param("MinCount", "1"),
        param("MaxCount", "1"),
    ];
    if !tags.is_empty() {
        params.push(param("TagSpecification.1.ResourceType", "instance"));
        for (position, (key, value)) in tags.iter().enumerate() {
            let index = position + 1;
            params.push(param(&format!("TagSpecification.1.Tag.{index}.Key"), key));
            params.push(param(&format!("TagSpecification.1.Tag.{index}.Value"), value));
        }
    }
    let launched = ec2(router, runtime, "RunInstances", &params);
    let instance_id = element(&launched, "instanceId");
    (
        instance_id.clone(),
        format!(
            "arn:aws:ec2:{REGION}:{}:instance/{instance_id}",
            engine::emulator_account_id()
        ),
    )
}

/// One VPC created through Amazon EC2's own `CreateVpc` and tagged afterwards by
/// `CreateTags` — a second EC2 resource kind, tagged after it was created.
/// Answers its ARN.
fn vpc(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    cidr: &str,
    tags: &[(&str, &str)],
) -> String {
    let created = ec2(router, runtime, "CreateVpc", &[param("CidrBlock", cidr)]);
    let vpc_id = element(&created, "vpcId");
    if !tags.is_empty() {
        let mut params = vec![param("ResourceId.1", &vpc_id)];
        for (position, (key, value)) in tags.iter().enumerate() {
            let index = position + 1;
            params.push(param(&format!("Tag.{index}.Key"), key));
            params.push(param(&format!("Tag.{index}.Value"), value));
        }
        ec2(router, runtime, "CreateTags", &params);
    }
    format!(
        "arn:aws:ec2:{REGION}:{}:vpc/{vpc_id}",
        engine::emulator_account_id()
    )
}

/// One queue created through Amazon SQS's own `CreateQueue` with `tags`, as the
/// ARN that service minted for it — the control row: an owner whose tagged
/// resources a query already selected before this rule was stated for every
/// owner.
fn queue(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
    tags: &[(&str, &str)],
) -> String {
    let pairs: serde_json::Map<String, Value> = tags
        .iter()
        .map(|(key, value)| ((*key).to_string(), json!(value)))
        .collect();
    let created = sqs(
        router,
        runtime,
        "CreateQueue",
        json!({ "QueueName": name, "tags": Value::Object(pairs) }),
    );
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateQueue({name:?}) reported no URL: {created}"));
    let attributes = sqs(
        router,
        runtime,
        "GetQueueAttributes",
        json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    );
    attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: the queue must report its ARN, got {attributes}"))
        .to_string()
}

/// The ARNs of the EBS volumes Amazon EC2 reports as attached to `instance_id`.
///
/// Read through EC2's own `DescribeVolumes` with the documented
/// `attachment.instance-id` filter, so what this suite calls "the instance's
/// volume" is what the service that owns volumes says it is.
fn attached_volumes(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    instance_id: &str,
) -> BTreeSet<String> {
    let described = ec2(
        router,
        runtime,
        "DescribeVolumes",
        &[
            param("Filter.1.Name", "attachment.instance-id"),
            param("Filter.1.Value.1", instance_id),
        ],
    );
    elements(&described, "volumeId")
        .into_iter()
        .map(|volume_id| {
            format!(
                "arn:aws:ec2:{REGION}:{}:volume/{volume_id}",
                engine::emulator_account_id()
            )
        })
        .collect()
}

/// `POST /groups` — Resource Groups' own `CreateGroup`, for a group DEFINED BY a
/// tag query: the resources it holds are the ones that query selects.
fn create_tag_filtered_group(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
    resource_type_filters: &[&str],
    tag_filters: Value,
) {
    let query = json!({
        "ResourceTypeFilters": resource_type_filters,
        "TagFilters": tag_filters,
    });
    let answer = resource_groups(
        router,
        runtime,
        REGION,
        "/groups",
        json!({
            "Name": name,
            "ResourceQuery": {
                "Type": TAG_FILTERS,
                "Query": query.to_string(),
            },
        }),
    );
    assert_eq!(
        answer.status, 200,
        "setup: CreateGroup({name:?}) must succeed, got {} / {:?}",
        answer.status, answer.body
    );
}

/// The ARNs `ListGroupResources` reports for the group `name`.
fn group_members(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
) -> BTreeSet<String> {
    let answer = resource_groups(
        router,
        runtime,
        REGION,
        "/list-group-resources",
        json!({ "Group": name }),
    );
    assert_eq!(
        answer.status, 200,
        "ListGroupResources({name:?}) must be served, got {} / {:?}",
        answer.status, answer.body
    );
    answer.body["Resources"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item["Identifier"]["ResourceArn"].as_str().map(str::to_string))
        .collect()
}

/// The components `ListComponents` reports for the application over `group`.
fn components(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    group: &str,
) -> Vec<Value> {
    let listed = application_insights(
        router,
        runtime,
        "ListComponents",
        json!({ "ResourceGroupName": group }),
    );
    assert_eq!(
        listed.status, 200,
        "ListComponents({group:?}) must be served, got {} / {:?}",
        listed.status, listed.body
    );
    listed.body["ApplicationComponentList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The `ComponentName`s of those components.
fn component_names(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    group: &str,
) -> BTreeSet<String> {
    components(router, runtime, group)
        .iter()
        .filter_map(|component| component["ComponentName"].as_str().map(str::to_string))
        .collect()
}

/// The `LifeCycle` `DescribeApplication` reports for the application over `group`.
fn life_cycle(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    group: &str,
) -> String {
    let described = application_insights(
        router,
        runtime,
        "DescribeApplication",
        json!({ "ResourceGroupName": group }),
    );
    assert_eq!(
        described.status, 200,
        "DescribeApplication({group:?}) must be served, got {} / {:?}",
        described.status, described.body
    );
    described.body["ApplicationInfo"]["LifeCycle"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The `ResourceList` `DescribeComponent` reports for one component.
fn component_resources(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    group: &str,
    component: &str,
) -> BTreeSet<String> {
    let described = application_insights(
        router,
        runtime,
        "DescribeComponent",
        json!({ "ResourceGroupName": group, "ComponentName": component }),
    );
    assert_eq!(
        described.status, 200,
        "DescribeComponent({component:?}) must be served, got {} / {:?}",
        described.status, described.body
    );
    described.body["ResourceList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|arn| arn.as_str().map(str::to_string))
        .collect()
}

/// Look again until `ready` holds, or until [`POLL_DEADLINE`] passes — the
/// onboarding a create starts is owed by the clock, so a reader waits for the
/// CONDITION rather than for a duration.
fn poll_until(case: &str, what: &str, mut ready: impl FnMut() -> bool) {
    let since = Instant::now();
    loop {
        if ready() {
            return;
        }
        assert!(
            since.elapsed() < POLL_DEADLINE,
            "case {case:?}: {what} did not hold while polling"
        );
        std::thread::sleep(LOOK_AGAIN);
    }
}

/// One row of the query table: the group to define, the query that defines it,
/// and which of the account's resources it must resolve to.
struct Row {
    case: &'static str,
    group: &'static str,
    resource_type_filters: Vec<&'static str>,
    tag_filters: Value,
    expected: Vec<String>,
}

/// The tag key the matching resources and one decoy carry, and the two values.
const SCOPE_KEY: &str = "Scope";
const SCOPE_MATCH: &str = "fleet-one";
const SCOPE_OTHER: &str = "fleet-two";

#[test]
fn a_tag_query_selects_the_tagged_resources_of_every_service_that_owns_one() {
    std::thread::Builder::new()
        .name("tag-query-owned-resources".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("a current-thread runtime");

            // The account's estate, each resource created through the service
            // that OWNS it, with its tags written the way that service takes
            // them.
            let match_tag = [(SCOPE_KEY, SCOPE_MATCH)];
            let tagged_bucket = bucket(&router, &runtime, "tag-query-bucket", &match_tag);
            let (_, tagged_instance) = instance(&router, &runtime, &match_tag);
            let tagged_vpc = vpc(&router, &runtime, "10.20.0.0/16", &match_tag);
            let tagged_queue = queue(&router, &runtime, "tag-query-queue", &match_tag);
            // The decoys: the same key with another value, and no tag at all.
            let other_bucket = bucket(
                &router,
                &runtime,
                "tag-query-other-bucket",
                &[(SCOPE_KEY, SCOPE_OTHER)],
            );
            let untagged_bucket = bucket(&router, &runtime, "tag-query-plain-bucket", &[]);

            let rows = vec![
                Row {
                    case: "one key with one value, over every supported type",
                    group: "tag-query-match",
                    resource_type_filters: vec![ALL_SUPPORTED],
                    tag_filters: json!([{ "Key": SCOPE_KEY, "Values": [SCOPE_MATCH] }]),
                    expected: vec![
                        tagged_bucket.clone(),
                        tagged_instance.clone(),
                        tagged_vpc.clone(),
                        tagged_queue.clone(),
                    ],
                },
                Row {
                    case: "the same key with the other value",
                    group: "tag-query-other",
                    resource_type_filters: vec![ALL_SUPPORTED],
                    tag_filters: json!([{ "Key": SCOPE_KEY, "Values": [SCOPE_OTHER] }]),
                    expected: vec![other_bucket.clone()],
                },
                Row {
                    case: "a key named with no value, which any value satisfies",
                    group: "tag-query-key-only",
                    resource_type_filters: vec![ALL_SUPPORTED],
                    tag_filters: json!([{ "Key": SCOPE_KEY }]),
                    expected: vec![
                        tagged_bucket.clone(),
                        tagged_instance.clone(),
                        tagged_vpc.clone(),
                        tagged_queue.clone(),
                        other_bucket.clone(),
                    ],
                },
                Row {
                    case: "one key with both values",
                    group: "tag-query-both-values",
                    resource_type_filters: vec![ALL_SUPPORTED],
                    tag_filters: json!([
                        { "Key": SCOPE_KEY, "Values": [SCOPE_MATCH, SCOPE_OTHER] }
                    ]),
                    expected: vec![
                        tagged_bucket.clone(),
                        tagged_instance.clone(),
                        tagged_vpc.clone(),
                        tagged_queue.clone(),
                        other_bucket.clone(),
                    ],
                },
                Row {
                    case: "the matching tag, narrowed to one resource type",
                    group: "tag-query-one-type",
                    resource_type_filters: vec!["AWS::EC2::Instance"],
                    tag_filters: json!([{ "Key": SCOPE_KEY, "Values": [SCOPE_MATCH] }]),
                    expected: vec![tagged_instance.clone()],
                },
                Row {
                    case: "the matching tag, narrowed to two resource types",
                    group: "tag-query-two-types",
                    resource_type_filters: vec!["AWS::S3::Bucket", "AWS::SQS::Queue"],
                    tag_filters: json!([{ "Key": SCOPE_KEY, "Values": [SCOPE_MATCH] }]),
                    expected: vec![tagged_bucket.clone(), tagged_queue.clone()],
                },
                Row {
                    case: "a key nothing in the account carries",
                    group: "tag-query-absent-key",
                    resource_type_filters: vec![ALL_SUPPORTED],
                    tag_filters: json!([{ "Key": "NoSuchScope", "Values": [SCOPE_MATCH] }]),
                    expected: Vec::new(),
                },
            ];

            for row in &rows {
                create_tag_filtered_group(
                    &router,
                    &runtime,
                    row.group,
                    &row.resource_type_filters,
                    row.tag_filters.clone(),
                );
                let expected: BTreeSet<String> = row.expected.iter().cloned().collect();
                let resolved = group_members(&router, &runtime, row.group);
                assert_eq!(
                    resolved, expected,
                    "case {:?}: expected the group to hold {expected:?}, got {resolved:?}",
                    row.case
                );
                assert!(
                    !resolved.contains(&untagged_bucket),
                    "case {:?}: an untagged resource is selected by no tag query, got {resolved:?}",
                    row.case
                );
            }

            // The membership and the account's own tag view are ONE projection:
            // the tagging API selects the same resources for the same filter.
            let reported = tagging(
                &router,
                &runtime,
                "GetResources",
                json!({ "TagFilters": [{ "Key": SCOPE_KEY, "Values": [SCOPE_MATCH] }] }),
            );
            let indexed: BTreeSet<String> = reported["ResourceTagMappingList"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|entry| entry["ResourceARN"].as_str().map(str::to_string))
                .collect();
            let expected: BTreeSet<String> = [
                tagged_bucket.clone(),
                tagged_instance.clone(),
                tagged_vpc.clone(),
                tagged_queue.clone(),
            ]
            .into_iter()
            .collect();
            assert_eq!(
                indexed, expected,
                "case \"the account's tag view\": expected {expected:?}, got {indexed:?}"
            );
        })
        .expect("spawn the tag-query thread")
        .join()
        .expect("a tag query must select the tagged resources of their owning services");
}

#[test]
fn an_application_over_a_tag_filtered_group_onboards_and_regroups_its_resources() {
    std::thread::Builder::new()
        .name("tag-query-application-onboarding".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("a current-thread runtime");

            let group = "tag-query-application";
            let fleet = [("Fleet", "blue")];
            let bucket_arn = bucket(&router, &runtime, "tag-query-app-bucket", &fleet);
            let (instance_id, instance_arn) = instance(&router, &runtime, &fleet);
            // The storage that instance runs on, as the service that owns volumes
            // reports it: part of the instance, not a resource beside it.
            let volumes = attached_volumes(&router, &runtime, &instance_id);
            assert!(
                !volumes.is_empty(),
                "setup: the launched instance must hold an EBS volume, got {volumes:?}"
            );

            create_tag_filtered_group(
                &router,
                &runtime,
                group,
                &[ALL_SUPPORTED],
                json!([{ "Key": "Fleet", "Values": ["blue"] }]),
            );
            let onboarded: BTreeSet<String> =
                [bucket_arn.clone(), instance_arn.clone()].into_iter().collect();
            let resolved = group_members(&router, &runtime, group);
            assert_eq!(
                resolved, onboarded,
                "setup: expected the group to resolve to {onboarded:?}, got {resolved:?}"
            );

            let created = application_insights(
                &router,
                &runtime,
                "CreateApplication",
                json!({ "ResourceGroupName": group, "AutoConfigEnabled": false }),
            );
            assert_eq!(
                created.status, 200,
                "setup: CreateApplication({group:?}) must succeed, got {} / {:?}",
                created.status, created.body
            );

            // Every resource the group resolved to is a standalone component
            // named by its own ARN, and the instance's volume is covered by the
            // instance's component rather than being one of its own.
            poll_until("onboarding", "every resolved resource became a component", || {
                component_names(&router, &runtime, group) == onboarded
            });
            for component in components(&router, &runtime, group) {
                assert_eq!(
                    component["Monitor"],
                    json!(false),
                    "case \"onboarding\": a discovered component is unmonitored until a \
                     configuration says so, got {component:?}"
                );
            }
            let covered = component_resources(&router, &runtime, group, &instance_arn);
            let mut expected_cover: BTreeSet<String> = volumes.clone();
            expected_cover.insert(instance_arn.clone());
            assert_eq!(
                covered, expected_cover,
                "case \"onboarding\": the instance's component must cover the instance and its \
                 volumes, got {covered:?}"
            );

            // Nothing is monitored, so the onboarded application settles on
            // NOT_CONFIGURED, and one monitored component makes it ACTIVE with no
            // further call.
            poll_until("lifecycle", "the application settled off CREATING", || {
                life_cycle(&router, &runtime, group) != "CREATING"
            });
            let settled = life_cycle(&router, &runtime, group);
            assert_eq!(
                settled, "NOT_CONFIGURED",
                "case \"lifecycle\": an onboarded application with nothing monitored reports \
                 NOT_CONFIGURED, got {settled:?}"
            );
            let configured = application_insights(
                &router,
                &runtime,
                "UpdateComponentConfiguration",
                json!({
                    "ResourceGroupName": group,
                    "ComponentName": bucket_arn,
                    "Monitor": true,
                    "Tier": "DEFAULT",
                    "ComponentConfiguration": "{\"subComponents\":[]}",
                }),
            );
            assert_eq!(
                configured.status, 200,
                "case \"lifecycle\": UpdateComponentConfiguration must succeed, got {} / {:?}",
                configured.status, configured.body
            );
            let active = life_cycle(&router, &runtime, group);
            assert_eq!(
                active, "ACTIVE",
                "case \"lifecycle\": a monitored component makes the application ACTIVE, got \
                 {active:?}"
            );

            // A custom component groups EC2 instances only.
            let refused = application_insights(
                &router,
                &runtime,
                "CreateComponent",
                json!({
                    "ResourceGroupName": group,
                    "ComponentName": "storage_tier",
                    "ResourceList": [bucket_arn.clone()],
                }),
            );
            assert_eq!(
                (refused.status, refused.error_code().as_str()),
                (400, "ValidationException"),
                "case \"grouping a bucket\": expected the 400 ValidationException, got {} / {:?}",
                refused.status,
                refused.body
            );
            assert_eq!(
                refused.error_message(),
                "Only EC2 Instance can be grouped.",
                "case \"grouping a bucket\": expected the only-instances refusal, got {:?}",
                refused.body
            );

            // Grouping the instance takes its standalone component away, and the
            // custom component covers the instance and its volumes.
            let grouped = application_insights(
                &router,
                &runtime,
                "CreateComponent",
                json!({
                    "ResourceGroupName": group,
                    "ComponentName": "web_tier",
                    "ResourceList": [instance_arn.clone()],
                }),
            );
            assert_eq!(
                grouped.status, 200,
                "case \"grouping the instance\": CreateComponent must succeed, got {} / {:?}",
                grouped.status, grouped.body
            );
            let after_grouping: BTreeSet<String> =
                [bucket_arn.clone(), "web_tier".to_string()].into_iter().collect();
            let listed = component_names(&router, &runtime, group);
            assert_eq!(
                listed, after_grouping,
                "case \"grouping the instance\": expected the components {after_grouping:?}, got \
                 {listed:?}"
            );
            let custom_cover = component_resources(&router, &runtime, group, "web_tier");
            assert_eq!(
                custom_cover, expected_cover,
                "case \"grouping the instance\": the custom component must cover the instance and \
                 its volumes, got {custom_cover:?}"
            );

            // A resource one custom component holds is not grouped into another,
            // and the refused create made nothing.
            let twice = application_insights(
                &router,
                &runtime,
                "CreateComponent",
                json!({
                    "ResourceGroupName": group,
                    "ComponentName": "web_tier_2",
                    "ResourceList": [instance_arn.clone()],
                }),
            );
            assert_eq!(
                (twice.status, twice.error_code().as_str()),
                (400, "ValidationException"),
                "case \"grouping it twice\": expected the 400 ValidationException, got {} / {:?}",
                twice.status,
                twice.body
            );
            assert_eq!(
                twice.error_message(),
                format!("Component {instance_arn} is grouped in another component."),
                "case \"grouping it twice\": expected the already-grouped refusal, got {:?}",
                twice.body
            );
            let missing = application_insights(
                &router,
                &runtime,
                "DescribeComponent",
                json!({ "ResourceGroupName": group, "ComponentName": "web_tier_2" }),
            );
            assert_eq!(
                missing.error_code(),
                "ResourceNotFoundException",
                "case \"grouping it twice\": the refused component must not exist, got {} / {:?}",
                missing.status,
                missing.body
            );

            // Ungrouping gives the instance back as its own component, covering
            // its volumes again, and it is groupable once more.
            let ungrouped = application_insights(
                &router,
                &runtime,
                "DeleteComponent",
                json!({ "ResourceGroupName": group, "ComponentName": "web_tier" }),
            );
            assert_eq!(
                ungrouped.status, 200,
                "case \"ungrouping\": DeleteComponent must succeed, got {} / {:?}",
                ungrouped.status, ungrouped.body
            );
            let restored = component_names(&router, &runtime, group);
            assert_eq!(
                restored, onboarded,
                "case \"ungrouping\": the instance must be a standalone component again, got \
                 {restored:?}"
            );
            let restored_cover = component_resources(&router, &runtime, group, &instance_arn);
            assert_eq!(
                restored_cover, expected_cover,
                "case \"ungrouping\": the restored component must cover the instance and its \
                 volumes, got {restored_cover:?}"
            );
            let regrouped = application_insights(
                &router,
                &runtime,
                "CreateComponent",
                json!({
                    "ResourceGroupName": group,
                    "ComponentName": "web_tier_3",
                    "ResourceList": [instance_arn.clone()],
                }),
            );
            assert_eq!(
                regrouped.status, 200,
                "case \"regrouping\": CreateComponent must succeed once the resource is free \
                 again, got {} / {:?}",
                regrouped.status,
                regrouped.body
            );

            // Only a CUSTOM component is ungrouped: a name the application holds
            // no custom component for is refused, and nothing is removed.
            let absent = application_insights(
                &router,
                &runtime,
                "DeleteComponent",
                json!({ "ResourceGroupName": group, "ComponentName": "no_such_component" }),
            );
            assert_eq!(
                absent.error_code(),
                "ResourceNotFoundException",
                "case \"ungrouping a name it does not hold\": expected the missing-component \
                 refusal, got {} / {:?}",
                absent.status,
                absent.body
            );
            assert_eq!(
                absent.error_message(),
                format!("Custom component no_such_component does not exist for {group}"),
                "case \"ungrouping a name it does not hold\": expected the missing-component \
                 sentence, got {:?}",
                absent.body
            );
        })
        .expect("spawn the application-onboarding thread")
        .join()
        .expect("an application must onboard the resources a tag query resolves to");
}
