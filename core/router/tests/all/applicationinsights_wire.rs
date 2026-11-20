// SPDX-License-Identifier: CC0-1.0

//! The wire every Application Insights suite in this test crate drives, written
//! ONCE: the signed request builder, the answer it parses, the calls those suites
//! make — one Application Insights operation, the Resource Groups `CreateGroup`
//! that gives an application a group to be created over, the group's membership —
//! and the estate a suite onboards an application over: a tagged bucket, a tagged
//! instance and the storage Amazon EC2 reports attached to it.
//!
//! A suite that writes its own drifts from the others: a credential scope that
//! names a different service, a body read under a different casing, a group
//! created in a different form, or an instance launched without the tag its query
//! selects changes what the suite is actually asserting. The services are the same
//! in every one of them, so the plumbing lives here and each suite states only its
//! own rows.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode};

/// Keep this at or above `boot_serve.rs`'s stack: assembling the whole router is
/// what every suite below does first.
pub(crate) const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The `X-Amz-Target` prefix this service's awsJson1_1 operations are named by.
const TARGET_PREFIX: &str = "EC2WindowsBarleyService";

/// What one call answered: the HTTP status and the parsed JSON body
/// (`Value::Null` for a body that is empty or not JSON).
#[derive(Debug)]
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) body: Value,
}

impl Answer {
    /// The awsJson error code, without its namespace.
    pub(crate) fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .rsplit(['#', ':'])
            .next()
            .unwrap_or("")
            .to_string()
    }

    /// The error message, whichever casing the envelope carries it under.
    pub(crate) fn error_message(&self) -> String {
        self.body
            .get("Message")
            .or_else(|| self.body.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }
}

/// Dispatch one request addressed to `signing_name` in `region`, the way a signed
/// SDK call is: the credential scope is what names the service and the region.
pub(crate) fn send(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: &str,
    region: &str,
    path: &str,
    target: Option<&str>,
    body: Value,
) -> Answer {
    let host = format!("{signing_name}.{region}.amazonaws.com");
    let content_type = if target.is_some() {
        "application/x-amz-json-1.1"
    } else {
        "application/json"
    };
    let mut request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", content_type)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{region}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        );
    if let Some(target) = target {
        request = request.header("x-amz-target", format!("{TARGET_PREFIX}.{target}"));
    }
    let request = request
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("serialize body"),
        ))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer { status, body }
}

/// One Application Insights operation, over its awsJson1_1 wire.
pub(crate) fn application_insights(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    operation: &str,
    body: Value,
) -> Answer {
    send(
        router,
        runtime,
        "applicationinsights",
        region,
        "/",
        Some(operation),
        body,
    )
}

/// One Resource Groups operation, over its REST wire.
pub(crate) fn resource_groups(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    path: &str,
    body: Value,
) -> Answer {
    send(router, runtime, "resource-groups", region, path, None, body)
}

/// The EC2 API version an ec2Query body carries.
pub(crate) const EC2_API_VERSION: &str = "2016-11-15";

/// The AMI a launch names, in the long form EC2 issues AMI ids in; `RunInstances`
/// only has to accept it, and no suite asserts anything about it.
pub(crate) const AMI: &str = "ami-0123456789abcdef0";

/// The one query type a tag-filtered group is defined by
/// (`smithy_query(query="shape",shape="QueryType")`).
pub(crate) const TAG_FILTERS: &str = "TAG_FILTERS_1_0";

/// The `ResourceTypeFilters` value that admits every supported type, which is the
/// value a query naming no type restriction carries.
pub(crate) const ALL_SUPPORTED: &str = "AWS::AllSupported";

/// How long [`poll_until`] keeps looking for the state a call owes. Generous by
/// design: it bounds nothing a suite asserts and nothing here measures elapsed
/// time — it only stops a broken emulator from hanging, so a loaded host cannot
/// fail a row.
pub(crate) const POLL_DEADLINE: Duration = Duration::from_secs(180);

/// How long [`poll_until`] waits before looking again.
pub(crate) const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// Where one non-JSON request goes: the signing name whose credential scope
/// routes it and the Region that scope names, the method and path it is addressed
/// to, and the content type its protocol frames its body in.
pub(crate) struct Addressed<'a> {
    pub(crate) signing_name: &'a str,
    pub(crate) region: &'a str,
    pub(crate) method: &'a str,
    pub(crate) path: &'a str,
    pub(crate) content_type: &'a str,
}

/// Dispatch one request whose body is NOT JSON — a form-encoded query action, an
/// XML document — as its status and its raw body.
///
/// The JSON counterpart of [`send`], for the services an Application Insights
/// suite builds its estate out of; the credential scope is what routes it and
/// tells the handler its Region, exactly as a signed SDK request does.
pub(crate) fn send_raw(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    to: Addressed<'_>,
    body: Bytes,
) -> (u16, String) {
    let Addressed {
        signing_name,
        region,
        method,
        path,
        content_type,
    } = to;
    let host = format!("{signing_name}.{region}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", content_type)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{region}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(body)
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// One form field of an ec2Query body.
pub(crate) fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One Amazon EC2 action over its ec2Query wire in `region`, which must be served,
/// answered with its XML body.
pub(crate) fn ec2(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
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
    let (status, answered) = send_raw(
        router,
        runtime,
        Addressed {
            signing_name: "ec2",
            region,
            method: "POST",
            path: "/",
            content_type: "application/x-www-form-urlencoded",
        },
        Bytes::from(body),
    );
    assert_eq!(
        status, 200,
        "setup: ec2:{action} must be served, got {status} with {answered}"
    );
    answered
}

/// A bucket created through Amazon S3's own `CreateBucket` in `region`, with
/// `tags` written onto it afterwards by `PutBucketTagging` — the way a client
/// labels a bucket it already has. Answers the ARN S3 reports such a bucket under.
///
/// Outside `us-east-1` the create names its `LocationConstraint`, which is what S3
/// requires of a bucket addressed to any other Region's endpoint.
pub(crate) fn tagged_bucket(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    name: &str,
    tags: &[(&str, &str)],
) -> String {
    let configuration = if region == "us-east-1" {
        String::new()
    } else {
        format!(
            "<CreateBucketConfiguration><LocationConstraint>{region}</LocationConstraint>\
             </CreateBucketConfiguration>"
        )
    };
    let (status, body) = send_raw(
        router,
        runtime,
        Addressed {
            signing_name: "s3",
            region,
            method: "PUT",
            path: &format!("/{name}"),
            content_type: "application/xml",
        },
        Bytes::from(configuration),
    );
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
        let (status, body) = send_raw(
            router,
            runtime,
            Addressed {
                signing_name: "s3",
                region,
                method: "PUT",
                path: &format!("/{name}?tagging"),
                content_type: "application/xml",
            },
            Bytes::from(document),
        );
        assert_eq!(
            status, 204,
            "setup: PutBucketTagging({name:?}) must succeed, got {status} with {body}"
        );
    }
    format!("arn:aws:s3:::{name}")
}

/// One instance launched through Amazon EC2's own `RunInstances` in `region`,
/// carrying `tags` in the launch's `TagSpecification` — a tag that arrives WITH
/// the resource. Answers `(instance id, instance ARN)`.
pub(crate) fn launch_instance(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
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
    let launched = ec2(router, runtime, region, "RunInstances", &params);
    let instance_id = element(&launched, "instanceId");
    (
        instance_id.clone(),
        format!(
            "arn:aws:ec2:{region}:{}:instance/{instance_id}",
            engine::emulator_account_id()
        ),
    )
}

/// The ARNs of the EBS volumes Amazon EC2 reports as attached to `instance_id` in
/// `region`.
///
/// Read through EC2's own `DescribeVolumes` with the documented
/// `attachment.instance-id` filter, so what a suite calls "the instance's storage"
/// is what the service that owns volumes says it is.
pub(crate) fn attached_volumes(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    instance_id: &str,
) -> BTreeSet<String> {
    let described = ec2(
        router,
        runtime,
        region,
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
                "arn:aws:ec2:{region}:{}:volume/{volume_id}",
                engine::emulator_account_id()
            )
        })
        .collect()
}

/// `POST /groups` — Resource Groups' own `CreateGroup`, for a group DEFINED BY a
/// tag query: the resources it holds are the ones that query selects.
pub(crate) fn create_tag_filtered_group(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
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
        region,
        "/groups",
        json!({
            "Name": name,
            "ResourceQuery": { "Type": TAG_FILTERS, "Query": query.to_string() },
        }),
    );
    assert_eq!(
        answer.status, 200,
        "setup: CreateGroup({name:?}) must succeed, got {} / {:?}",
        answer.status, answer.body
    );
}

/// The ARNs `ListGroupResources` reports for the group `name`.
pub(crate) fn group_members(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    name: &str,
) -> BTreeSet<String> {
    let answer = resource_groups(
        router,
        runtime,
        region,
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
pub(crate) fn components(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    group: &str,
) -> Vec<Value> {
    let listed = application_insights(
        router,
        runtime,
        region,
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
pub(crate) fn component_names(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    group: &str,
) -> BTreeSet<String> {
    components(router, runtime, region, group)
        .iter()
        .filter_map(|component| component["ComponentName"].as_str().map(str::to_string))
        .collect()
}

/// The `ResourceList` `DescribeComponent` reports for one component, which must be
/// served.
pub(crate) fn component_resources(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    group: &str,
    component: &str,
) -> BTreeSet<String> {
    let described = application_insights(
        router,
        runtime,
        region,
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
pub(crate) fn poll_until(case: &str, what: &str, mut ready: impl FnMut() -> bool) {
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

/// `POST /groups` — Resource Groups' own `CreateGroup`, in `region`, with the
/// service configuration the caller needs (none, for a group nothing is grouped
/// into by hand).
pub(crate) fn create_group(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    name: &str,
    configuration: Option<Value>,
) {
    let mut body = json!({ "Name": name });
    if let Some(configuration) = configuration {
        body.as_object_mut()
            .expect("the request is a JSON object")
            .insert("Configuration".to_string(), configuration);
    }
    let answer = resource_groups(router, runtime, region, "/groups", body);
    assert_eq!(
        answer.status, 200,
        "setup: CreateGroup({name:?}) in {region} must succeed, got {} / {:?}",
        answer.status, answer.body
    );
}
