// SPDX-License-Identifier: CC0-1.0

//! Which url the services of this emulator report for ONE queue — over the service set
//! mounted together, which is the only place the question can be put at all.
//!
//! The RULE under test: **a queue's handle is the url its OWNING service reports — an
//! address on the endpoint this emulator is reached at, followed by the queue's
//! `/<accountId>/<QueueName>` path — and every component that reports that queue reports
//! that same string**: the operations of Amazon SQS itself (`CreateQueue`, `GetQueueUrl`,
//! `ListQueues`), the handle a CloudFormation stack publishes for a declared queue (its
//! physical resource id, which is what `Ref` resolves to, and its `QueueUrl` attribute), and
//! the identifier the Cloud Control API reports for the `AWS::SQS::Queue` resource (whose
//! `primaryIdentifier` is `/properties/QueueUrl`). A client joins those strings — a stack
//! output against a `GetQueueUrl`, a tracked `ProgressEvent` identifier against the queue it
//! is holding — so two components that answer with two different urls for one queue
//! describe, as far as any such join can tell, two different resources. A component that
//! COMPOSES the handle instead of taking the one that service reported is how they drift.
//!
//! The handle leads back to this emulator, which is what makes it usable: a client sends the
//! string it was handed straight back, and each reported url is asserted to be a WORKING
//! handle here by sending a message through the exact string that was reported. Which
//! authority a url carries decides nothing about WHICH queue it addresses — that is its
//! `/<accountId>/<QueueName>` path alone — so a url built against another endpoint of this
//! emulator resolves to the same queue, which the rows below also assert.
//!
//! The rows differ along what the rule generalises over: WHICH API created the queue (Amazon
//! SQS's own `CreateQueue`, a CloudFormation stack's `AWS::SQS::Queue`, and a Cloud Control
//! `CreateResource`), the REGION the queue lives in, the queue's NAME (its shortest form,
//! every character class the member admits, the published 80-character bound, and a FIFO
//! name), whether the queue is standard or FIFO, whether a stack's template NAMES the queue
//! or leaves the name to CloudFormation, and the ADDRESS the call arrived on (this
//! emulator's own endpoint, and one of AWS's own endpoint hosts — which is answered the
//! address this process is served at, never a host AWS publishes).
//!
//! Scope: what each service does with a queue of its own — the Amazon SQS crate's
//! `queue_url_endpoint_form` suite for the form every one of its own answers carries, the
//! Cloud Control crate's `sqs_queue_read_through` for the resource model, and this target's
//! `cloudformation_provisions_sqs_queues` for the stack lifecycle — is asserted by those
//! suites. What only this one can reach is the AGREEMENT between them.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, outputs_of, physical_ids_of};

/// The endpoint prefix Amazon SQS publishes its regional endpoints under (that service's
/// `endpointPrefix`, `sqs`), which is the first label of the authority a queue url carries.
/// Spelled out here rather than read from the crate, so a row fails if a service starts
/// reporting urls on some other host.
const ENDPOINT_PREFIX: &str = "sqs";

/// The registry type whose resources live in the Amazon SQS service's own store, and whose
/// `primaryIdentifier` is `/properties/QueueUrl` — the url that service assigned.
const QUEUE_TYPE: &str = "AWS::SQS::Queue";

/// The Cloud Control API's signing name and `X-Amz-Target` prefix, from its model.
const CLOUD_CONTROL_SIGNING_NAME: &str = "cloudcontrolapi";
const CLOUD_CONTROL_TARGET_PREFIX: &str = "CloudApiService";

/// Amazon SQS's `X-Amz-Target` prefix, from its model (service shape `AmazonSQS`, which
/// speaks `awsJson1_0`).
const SQS_TARGET_PREFIX: &str = "AmazonSQS";

/// The `OperationStatus` values a Cloud Control resource request reports before it has
/// finished, and the terminal ones.
const STATUS_PENDING: &str = "PENDING";
const STATUS_IN_PROGRESS: &str = "IN_PROGRESS";
const STATUS_SUCCESS: &str = "SUCCESS";
const STATUS_FAILED: &str = "FAILED";
const STATUS_CANCEL_COMPLETE: &str = "CANCEL_COMPLETE";

/// The suffix every status a stack operation is still running carries.
const IN_PROGRESS_SUFFIX: &str = "_IN_PROGRESS";

/// The status a stack whose creation finished rests in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long a wait for work ANOTHER service performs runs before the case fails.
///
/// A stack's create and a Cloud Control request each run a deployment of their own — further
/// requests of this emulator, each with the span the clock owes it — so the budget is far
/// above anything those flows take, including on a host running the whole workspace's tests
/// at once. No case waits for a span: each re-reads until what it is after is there or this
/// deadline passes.
const BUDGET: Duration = Duration::from_secs(180);

/// How often such a wait re-reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The longest a `QueueName` may be ("A queue name can have up to 80 characters").
const MAX_QUEUE_NAME_LENGTH: usize = 80;

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them, and SERVED at the address a client dials — the
/// way the core records its data port on boot, which is what an in-process call of one
/// service by another is announced when the request it is answering named one of AWS's own
/// hosts.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudcontrol_cloudapiservice::EmulatedService::new(engine),
    );
    ::emulator_server::EmulatorService::serving_at(&router, SERVED_AT);
    router
}

/// The account id every url and ARN this emulator mints carries, from the core's single
/// source of truth rather than a literal.
fn account() -> &'static str {
    engine::emulator_account_id()
}

/// The base url this emulator is SERVED at, recorded the way the core records its data port
/// on boot.
const SERVED_AT: &str = "https://127.0.0.1:9297";

/// The address a client on the machine this emulator runs on reaches it at — the authority
/// of [`SERVED_AT`], which is what every url reported here has to name.
const CLIENT_ADDRESS: &str = "127.0.0.1:9297";

/// The url a queue named `name` must be reported at by EVERY component:
/// `https://<the address this emulator is reached at>/<accountId>/<QueueName>`.
///
/// Composed here from the parts the rule names — the address the caller reached, the account
/// this emulator answers for and the queue's own name — so no row is satisfied by a string
/// read back out of an answer being checked.
fn expected_url(name: &str) -> String {
    format!("https://{CLIENT_ADDRESS}/{}/{name}", account())
}

/// The host a client that resolves AWS's own names addresses `prefix`'s service at in
/// `region`.
fn aws_endpoint(prefix: &str, region: &str) -> String {
    format!("{prefix}.{region}.amazonaws.com")
}

/// A SigV4 `Authorization` header whose credential scope names `region` and `service`, which
/// is what routes the request AND what says which region the call addressed. The signature
/// is never checked: this router runs with request authentication off.
fn authorization(region: &str, service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one Amazon SQS operation over its `awsJson1_0` wire, in `region`, from a client at
/// `address`.
async fn sqs(
    router: &MultiServiceRouter,
    address: &str,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", address)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{SQS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(region, ENDPOINT_PREFIX))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// The same call, required to be served.
async fn sqs_ok(
    router: &MultiServiceRouter,
    case: &str,
    address: &str,
    region: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answered) = sqs(router, address, region, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation} in {region} on \
         {address}, got {status} with {answered}"
    );
    answered
}

/// Call one Cloud Control API operation over its `awsJson1_0` wire, in `region`, from a
/// client at `address`.
async fn cloud_control(
    router: &MultiServiceRouter,
    address: &str,
    region: &str,
    operation: &str,
    payload: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", address)
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{CLOUD_CONTROL_TARGET_PREFIX}.{operation}"),
        )
        .header(
            "authorization",
            authorization(region, CLOUD_CONTROL_SIGNING_NAME),
        )
        .body(Bytes::from(payload.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// The same call, required to be served.
async fn cc_ok(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    operation: &str,
    payload: Value,
) -> Value {
    let (status, answered) =
        cloud_control(router, CLIENT_ADDRESS, region, operation, payload).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Cloud Control {operation} in {region}, got \
         {status} with {answered}"
    );
    answered
}

/// Call one CloudFormation operation over its awsQuery wire, in `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
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
        .header("host", CLIENT_ADDRESS)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(region, "cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The same call, required to be served.
async fn cfn_ok(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = cloudformation(router, region, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudFormation {action}, got {status} with \
         {body}"
    );
    body
}

/// A string member of an answered document.
fn text(value: &Value, member: &str) -> String {
    value
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `QueueUrls` of a `ListQueues` answer — a listing that reports nothing carries no
/// member at all.
fn listed_urls(answered: &Value) -> Vec<String> {
    answered
        .get("QueueUrls")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The url the OWNING service reports for the queue called `name` to a client at `address`,
/// read both ways it can be: the name lookup and the listing filtered to that name, which
/// must agree.
async fn url_sqs_reports(
    router: &MultiServiceRouter,
    case: &str,
    address: &str,
    region: &str,
    name: &str,
) -> String {
    let resolved = sqs_ok(
        router,
        case,
        address,
        region,
        "GetQueueUrl",
        &json!({ "QueueName": name }),
    )
    .await;
    let looked_up = text(&resolved, "QueueUrl");
    let listed = sqs_ok(
        router,
        case,
        address,
        region,
        "ListQueues",
        &json!({ "QueueNamePrefix": name }),
    )
    .await;
    assert_eq!(
        listed_urls(&listed),
        vec![looked_up.clone()],
        "case {case:?}: expected ListQueues to report the url GetQueueUrl reported \
         ({looked_up:?}), got {listed}"
    );
    looked_up
}

/// The identifier the Cloud Control API reports for the queue the url `url` names, read both
/// ways it can be: the resource read, and the listing of the type.
///
/// Answers the identifier of the resource read, having asserted that the listing holds it
/// too and that the resource model's own `QueueUrl` property carries the same string.
async fn identifier_cloud_control_reports(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    url: &str,
) -> String {
    let read = cc_ok(
        router,
        case,
        region,
        "GetResource",
        json!({ "TypeName": QUEUE_TYPE, "Identifier": url }),
    )
    .await;
    let description = read["ResourceDescription"].clone();
    let identifier = text(&description, "Identifier");
    let properties: Value =
        serde_json::from_str(&text(&description, "Properties")).unwrap_or(Value::Null);
    assert_eq!(
        text(&properties, "QueueUrl"),
        identifier,
        "case {case:?}: expected the resource model's QueueUrl to be the identifier \
         ({identifier:?}), got {properties}"
    );
    let listed = cc_ok(
        router,
        case,
        region,
        "ListResources",
        json!({ "TypeName": QUEUE_TYPE }),
    )
    .await;
    let identifiers: Vec<String> = listed["ResourceDescriptions"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .map(|description| text(description, "Identifier"))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        identifiers.contains(&identifier),
        "case {case:?}: expected ListResources to report {identifier:?}, got {identifiers:?}"
    );
    identifier
}

/// Assert that `url` is a working handle on the queue called `name`: the ARN read through it
/// is that queue's, and a message sent through it is accepted.
async fn assert_addresses_the_queue(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
    url: &str,
) {
    let reported = sqs_ok(
        router,
        case,
        CLIENT_ADDRESS,
        region,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let expected_arn = format!("arn:aws:sqs:{region}:{}:{name}", account());
    assert_eq!(
        reported["Attributes"]["QueueArn"].as_str(),
        Some(expected_arn.as_str()),
        "case {case:?}: expected the url {url:?} to address the queue it names, got {reported}"
    );
    // A FIFO queue takes a message only with the group its ordering is keyed by.
    let mut request = json!({ "QueueUrl": url, "MessageBody": "through the reported url" });
    if name.ends_with(".fifo") {
        request["MessageGroupId"] = json!("one-url");
        request["MessageDeduplicationId"] = json!(format!("{name}-dedup"));
    }
    let sent = sqs_ok(router, case, CLIENT_ADDRESS, region, "SendMessage", &request).await;
    assert!(
        sent.get("MessageId").is_some(),
        "case {case:?}: expected a message sent through {url:?} to be accepted, got {sent}"
    );
}

/// One row of the table: a queue created through Amazon SQS's own API.
struct NativeRow {
    case: &'static str,
    region: &'static str,
    name: String,
    attributes: Value,
    /// The address the row's requests arrive on, which the url they are answered with may
    /// not depend on: a client on this machine for most rows, and AWS's own endpoint host
    /// for the one that puts the same question as a caller resolving AWS's names.
    address: Address,
}

/// Which address a row's requests carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Address {
    /// Loopback with the emulator's own port ([`CLIENT_ADDRESS`]).
    Client,
    /// The service's own regional endpoint host.
    AwsEndpoint,
}

/// The rows, spanning the REGION the queue lives in and the shape of its NAME: the shortest
/// name, every character class `CreateQueueRequest$QueueName` admits ("alphanumeric
/// characters, hyphens (-), and underscores (_)", case-significant), the published
/// 80-character bound, and a FIFO name — none of them a name any recorded call used.
fn native_rows() -> Vec<NativeRow> {
    let at_the_bound = format!(
        "one-url-{}",
        "a".repeat(MAX_QUEUE_NAME_LENGTH - "one-url-".len())
    );
    assert_eq!(
        at_the_bound.chars().count(),
        MAX_QUEUE_NAME_LENGTH,
        "the length row must sit on the published bound, got {}",
        at_the_bound.chars().count()
    );
    vec![
        NativeRow {
            case: "the shortest name a queue can carry",
            region: "us-east-1",
            name: "q".to_string(),
            attributes: json!({}),
            address: Address::Client,
        },
        NativeRow {
            case: "hyphens, underscores, digits and mixed case",
            region: "us-east-1",
            name: "One_Url-Queue_07".to_string(),
            attributes: json!({}),
            address: Address::Client,
        },
        NativeRow {
            case: "a FIFO queue",
            region: "eu-central-1",
            name: "one-url-ordered.fifo".to_string(),
            attributes: json!({ "FifoQueue": "true" }),
            address: Address::Client,
        },
        NativeRow {
            case: "the published length bound, in a third region",
            region: "ap-southeast-2",
            name: at_the_bound,
            attributes: json!({}),
            address: Address::Client,
        },
        NativeRow {
            case: "a call arriving on AWS's own endpoint host",
            region: "us-east-1",
            name: "one-url-aws-host".to_string(),
            attributes: json!({}),
            address: Address::AwsEndpoint,
        },
    ]
}

/// RULE: a queue created through Amazon SQS's own API is reported at ONE url — the service's
/// regional endpoint and the queue's `/<accountId>/<QueueName>` path — by that service's
/// create, its name lookup and its listing, and by the Cloud Control API's resource read and
/// resource listing; and that url addresses the queue.
#[tokio::test]
async fn every_service_reports_one_url_for_a_queue_sqs_created() {
    let router = build_router();
    for NativeRow {
        case,
        region,
        name,
        attributes,
        address,
    } in native_rows()
    {
        let address = match address {
            Address::Client => CLIENT_ADDRESS.to_string(),
            Address::AwsEndpoint => aws_endpoint(ENDPOINT_PREFIX, region),
        };
        let mut request = json!({ "QueueName": &name });
        if attributes.as_object().is_some_and(|map| !map.is_empty()) {
            request["Attributes"] = attributes;
        }
        let created = sqs_ok(&router, case, &address, region, "CreateQueue", &request).await;
        let minted = text(&created, "QueueUrl");
        let expected = expected_url(&name);
        assert_eq!(
            minted, expected,
            "case {case:?} on {address}: expected the create to report {expected:?}, got \
             {minted:?}"
        );

        let reported = url_sqs_reports(&router, case, &address, region, &name).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the owning service's reads to report {expected:?}, got \
             {reported:?}"
        );

        let identifier = identifier_cloud_control_reports(&router, case, region, &minted).await;
        assert_eq!(
            identifier, expected,
            "case {case:?}: expected the Cloud Control identifier to be the url Amazon SQS \
             serves ({expected:?}), got {identifier:?}"
        );

        assert_addresses_the_queue(&router, case, region, &name, &minted).await;
    }
}

/// One row of the stack table: the logical id of a declared queue, the `QueueName` the
/// template states for it (none, for a queue CloudFormation names itself), and whether the
/// declaration makes it a FIFO queue.
struct StackRow {
    case: &'static str,
    logical_id: &'static str,
    declared_name: Option<&'static str>,
    fifo: bool,
}

const STACK_ROWS: &[StackRow] = &[
    StackRow {
        case: "a queue the template names",
        logical_id: "Named",
        declared_name: Some("one-url-stack-named"),
        fifo: false,
    },
    StackRow {
        case: "a queue CloudFormation names itself",
        logical_id: "Generated",
        declared_name: None,
        fifo: false,
    },
    StackRow {
        case: "a FIFO queue the template names",
        logical_id: "Ordered",
        declared_name: Some("one-url-stack-ordered.fifo"),
        fifo: true,
    },
];

/// The template the stack is deployed from: one `AWS::SQS::Queue` per row, with an `Outputs`
/// entry for the row's `Ref` and one for its `Fn::GetAtt` `QueueUrl`, which are the two ways
/// a template hands a queue's handle to its caller.
fn stack_template() -> String {
    let mut resources = serde_json::Map::new();
    let mut outputs = serde_json::Map::new();
    for row in STACK_ROWS {
        let mut properties = serde_json::Map::new();
        if let Some(name) = row.declared_name {
            properties.insert("QueueName".to_string(), json!(name));
        }
        if row.fifo {
            properties.insert("FifoQueue".to_string(), json!(true));
        }
        resources.insert(
            row.logical_id.to_string(),
            json!({ "Type": QUEUE_TYPE, "Properties": Value::Object(properties) }),
        );
        outputs.insert(
            format!("{}Ref", row.logical_id),
            json!({ "Value": { "Ref": row.logical_id } }),
        );
        outputs.insert(
            format!("{}Attribute", row.logical_id),
            json!({ "Value": { "Fn::GetAtt": [row.logical_id, "QueueUrl"] } }),
        );
    }
    json!({ "Resources": Value::Object(resources), "Outputs": Value::Object(outputs) }).to_string()
}

/// Read the stack `name` until CloudFormation reports it in a status that is not in
/// progress, and answer that status.
async fn settled_stack(router: &MultiServiceRouter, case: &str, region: &str, name: &str) -> String {
    let started = Instant::now();
    loop {
        let described = cfn_ok(
            router,
            case,
            region,
            "DescribeStacks",
            &[("StackName", name)],
        )
        .await;
        let status = element(&described, "StackStatus");
        if !status.ends_with(IN_PROGRESS_SUFFIX) {
            return status;
        }
        assert!(
            started.elapsed() < BUDGET,
            "case {case:?}: expected the stack to settle within {BUDGET:?}, it is still \
             {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: the handle a CloudFormation stack publishes for a declared queue — its physical
/// resource id, which is what `Ref` resolves to, and its `QueueUrl` attribute — is the url
/// the owning service serves for that queue, which the Cloud Control API reports as the
/// resource's identifier too.
#[tokio::test]
async fn a_stacks_published_queue_handle_is_the_url_sqs_serves() {
    const REGION: &str = "us-east-1";
    const STACK: &str = "one-url-stack";
    let router = build_router();
    let case = "the stack's declared queues";

    cfn_ok(
        &router,
        case,
        REGION,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &stack_template())],
    )
    .await;
    let status = settled_stack(&router, case, REGION, STACK).await;
    assert_eq!(
        status, CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {status:?}"
    );

    let described = cfn_ok(
        &router,
        case,
        REGION,
        "DescribeStacks",
        &[("StackName", STACK)],
    )
    .await;
    let published = outputs_of(&described);
    let resources = cfn_ok(
        &router,
        case,
        REGION,
        "DescribeStackResources",
        &[("StackName", STACK)],
    )
    .await;
    let physical_ids: BTreeMap<String, String> = physical_ids_of(&resources);

    for row in STACK_ROWS {
        let case = row.case;
        let physical_id = physical_ids
            .get(row.logical_id)
            .unwrap_or_else(|| panic!("case {case:?}: no physical id for {}", row.logical_id))
            .clone();
        // The NAME is the template's where it states one, and otherwise the one
        // CloudFormation chose — read off the url's last path segment rather than guessed,
        // which is what makes the lookup below a question about the same queue.
        let name = row.declared_name.map(ToString::to_string).unwrap_or_else(|| {
            physical_id
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string()
        });
        assert!(
            !name.is_empty(),
            "case {case:?}: expected the published handle to carry the queue's name, got \
             {physical_id:?}"
        );
        assert_eq!(
            physical_id,
            expected_url(&name),
            "case {case:?}: expected the stack's handle to be the url Amazon SQS reports for \
             the queue, got {physical_id:?}"
        );

        // Both ways the template hands the handle to its caller carry that same string.
        for member in [
            format!("{}Ref", row.logical_id),
            format!("{}Attribute", row.logical_id),
        ] {
            let value = published
                .get(&member)
                .unwrap_or_else(|| panic!("case {case:?}: no {member} output in {described}"));
            assert_eq!(
                value, &physical_id,
                "case {case:?}: expected the {member} output to be the resource's handle \
                 ({physical_id:?}), got {value:?}"
            );
        }

        // ...and so does the owning service, and the Cloud Control API over its store.
        let reported = url_sqs_reports(&router, case, CLIENT_ADDRESS, REGION, &name).await;
        assert_eq!(
            reported, physical_id,
            "case {case:?}: expected Amazon SQS to serve the queue at the url the stack \
             published ({physical_id:?}), got {reported:?}"
        );
        let identifier = identifier_cloud_control_reports(&router, case, REGION, &physical_id).await;
        assert_eq!(
            identifier, physical_id,
            "case {case:?}: expected the Cloud Control identifier to be the stack's handle \
             ({physical_id:?}), got {identifier:?}"
        );

        assert_addresses_the_queue(&router, case, REGION, &name, &physical_id).await;
    }
}

/// Read the Cloud Control request `token` names until it reports a terminal status, and
/// answer the `ProgressEvent` it settled as.
async fn settled_request(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    token: &str,
) -> Value {
    let started = Instant::now();
    loop {
        let answered = cc_ok(
            router,
            case,
            region,
            "GetResourceRequestStatus",
            json!({ "RequestToken": token }),
        )
        .await;
        let event = answered["ProgressEvent"].clone();
        let status = text(&event, "OperationStatus");
        if [STATUS_SUCCESS, STATUS_FAILED, STATUS_CANCEL_COMPLETE].contains(&status.as_str()) {
            return event;
        }
        assert!(
            status == STATUS_PENDING || status == STATUS_IN_PROGRESS,
            "case {case:?}: expected a modelled operation status, got {event}"
        );
        assert!(
            started.elapsed() < BUDGET,
            "case {case:?}: expected the request to settle within {BUDGET:?}, it is still \
             {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: the identifier the Cloud Control API tracks for a queue IT created is the url the
/// owning service serves for that queue — the same string on the progress event the create
/// answered, on the event the request's own status read reports, and from Amazon SQS.
#[tokio::test]
async fn a_queue_cloud_control_created_is_tracked_at_the_url_sqs_serves() {
    const REGION: &str = "eu-west-1";
    let router = build_router();
    // Standard and FIFO, so no row passes through one queue type alone.
    let rows: Vec<(&str, String, Value)> = vec![
        (
            "a standard queue",
            "one-url-cc-standard".to_string(),
            json!({}),
        ),
        (
            "a FIFO queue",
            "one-url-cc-ordered.fifo".to_string(),
            json!({ "FifoQueue": true }),
        ),
    ];

    for (case, name, extra) in rows {
        let mut desired = json!({ "QueueName": &name });
        if let Some(members) = extra.as_object() {
            for (property, value) in members {
                desired[property] = value.clone();
            }
        }
        let accepted = cc_ok(
            &router,
            case,
            REGION,
            "CreateResource",
            json!({ "TypeName": QUEUE_TYPE, "DesiredState": desired.to_string() }),
        )
        .await;
        let event = accepted["ProgressEvent"].clone();
        let token = text(&event, "RequestToken");
        let settled = settled_request(&router, case, REGION, &token).await;
        assert_eq!(
            text(&settled, "OperationStatus"),
            STATUS_SUCCESS,
            "case {case:?}: expected the create to succeed, got {settled}"
        );

        let expected = expected_url(&name);
        let tracked = text(&settled, "Identifier");
        assert_eq!(
            tracked, expected,
            "case {case:?}: expected the tracked identifier to be the url Amazon SQS serves \
             ({expected:?}), got {tracked:?}"
        );
        let reported = url_sqs_reports(&router, case, CLIENT_ADDRESS, REGION, &name).await;
        assert_eq!(
            reported, tracked,
            "case {case:?}: expected Amazon SQS to serve the queue at the tracked identifier \
             ({tracked:?}), got {reported:?}"
        );
        let identifier = identifier_cloud_control_reports(&router, case, REGION, &tracked).await;
        assert_eq!(
            identifier, tracked,
            "case {case:?}: expected the resource read to report the tracked identifier \
             ({tracked:?}), got {identifier:?}"
        );

        assert_addresses_the_queue(&router, case, REGION, &name, &tracked).await;
    }
}
