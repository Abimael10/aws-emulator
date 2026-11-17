// SPDX-License-Identifier: CC0-1.0

//! What a completed stack's `Outputs` carry, read back through `DescribeStacks`.
//!
//! RULE under test: **a stack that reaches a successful terminal status reports every output
//! its deployed template declares — and that the conditions in force keep — with BOTH its
//! `OutputKey` and the `OutputValue` its declared `Value` resolved to, so a client can build
//! `{OutputKey: OutputValue}` over the whole list. An output the template declares under a
//! condition that does not hold is not reported at all, rather than reported without a
//! value.**
//!
//! Why it matters: a stack's outputs are the documented hand-off between a deployment and
//! everything downstream of it (a queue URL, a function name, a bucket name). An entry with a
//! key and no value makes that map impossible to build — a client indexing `OutputValue`
//! raises on it — and a stack reporting a terminal success while its outputs are unusable
//! fails every following call far from the cause.
//!
//! Several crates have to agree for the rule to hold: CloudFormation resolves each `Value`
//! against the stack's parameters, its pseudo parameters and the identities the services that
//! OWN the declared resources minted, and those services are what mint them — so the suite
//! lives here rather than in any one crate.
//!
//! The rows differ along what the rule generalises over: the FORM of the declared `Value` (a
//! `Ref` to a resource, to a parameter and to each pseudo parameter, an `Fn::GetAtt` on a
//! resource attribute, and the functions that compose one — `Fn::Sub` in both of its forms,
//! `Fn::Join`, `Fn::Select` over `Fn::Split`, `Fn::FindInMap`, `Fn::Base64`, `Fn::If`), which
//! service owns the resource an output names, whether the output also carries a `Description`
//! and an `Export`, and — in the tests beside the table — whether the value comes from another
//! stack's export and whether the output's own condition holds. The remaining source of an
//! output's value, a custom resource's provider `Data` read by an `Fn::GetAtt`, is already
//! driven end to end by `cloudformation_custom_resource_data_feeds_dependents`, which asserts
//! the stack's `Published` output against what the provider answered; nothing here repeats it,
//! so no case of this suite has to run a handler.
//!
//! Every expected value is computed here from what the row declares, from the Region each
//! request is signed for and from the account every ARN this emulator mints carries — never
//! read back out of the answer being checked.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`).
use crate::query_wire::{elements, form_encode, members_of, opt_element};

/// The Region every request below is signed for, and so the Region the stacks belong to.
pub(crate) const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
pub(crate) const ACCOUNT: &str = "000000000000";

/// The statuses the rule is stated about.
pub(crate) const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long a condition the clock owes is waited for, and how often it is looked at. The wait
/// is on the CONDITION — the deployment reaching a terminal status — so nothing here sleeps a
/// fixed span on a service's behalf, and the deadline is far longer than the spans this
/// emulator gives these transitions, so a loaded host cannot decide the outcome.
pub(crate) const SETTLE_DEADLINE: Duration = Duration::from_secs(240);
pub(crate) const POLL: Duration = Duration::from_millis(50);

/// The logical ids the table's template declares its resources under.
const QUEUE: &str = "Queue";
const BUCKET: &str = "Bucket";
const TOPIC: &str = "Topic";

/// The parameter the table's template declares, and the value the deployment passes for it.
const PARAMETER: &str = "Tier";
const PARAMETER_VALUE: &str = "production";

/// The mapping the table's template declares, and the key an `Fn::FindInMap` reads out of it.
const MAP: &str = "Sizes";
const MAP_KEY: &str = "production";
const MAP_ENTRY: &str = "Capacity";
const MAP_VALUE: &str = "large";

/// The condition the table's template declares, which holds for the parameter value above.
const CONDITION: &str = "IsProduction";

/// The names the table's template gives the resources it declares, so every expected value is
/// computed from the declaration rather than read back out of the answer.
const QUEUE_NAME: &str = "stack-output-values-queue";
const BUCKET_NAME: &str = "stack-output-values-bucket";
const TOPIC_NAME: &str = "stack-output-values-topic";

/// One answered request: its status and its body verbatim.
pub(crate) struct Answered {
    pub(crate) status: u16,
    pub(crate) body: Bytes,
}

impl Answered {
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    pub(crate) fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// The services these suites exercise, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: a stack's declared resources are really created in
/// the services that own them, which is where the identities its outputs report come from.
///
/// The services are named rather than the whole emulator assembled, so a case carries only the
/// stores it drives: CloudFormation, the four services that own the declared resources (Amazon
/// SQS, Amazon S3, Amazon SNS and AWS Lambda) and IAM, which answers for the execution role a
/// declared function names.
pub(crate) fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suites exercise the services, not SigV4: every call carries a credential scope so
    // the router can name the target service, and the signature itself is not under test.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request and what each handler reads its Region from.
pub(crate) fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
pub(crate) async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = Box::pin(router.dispatch(builder.body(body).expect("a well-formed request")))
        .await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        body,
    }
}

/// Call one CloudFormation operation over its awsQuery wire.
pub(crate) async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> Answered {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        "POST",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("cloudformation")),
        ],
        Bytes::from(body),
    )
    .await
}

/// `CreateStack` for `template`, asserting the submission itself was accepted.
pub(crate) async fn create_stack(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
    parameters: &[(&str, &str)],
) {
    let mut members: Vec<(String, String)> = vec![
        ("StackName".to_string(), stack.to_string()),
        ("TemplateBody".to_string(), template.to_string()),
        (
            "Capabilities.member.1".to_string(),
            "CAPABILITY_NAMED_IAM".to_string(),
        ),
    ];
    for (index, (key, value)) in parameters.iter().enumerate() {
        let at = index + 1;
        members.push((format!("Parameters.member.{at}.ParameterKey"), (*key).to_string()));
        members.push((
            format!("Parameters.member.{at}.ParameterValue"),
            (*value).to_string(),
        ));
    }
    let borrowed: Vec<(&str, &str)> = members
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let created = cloudformation(router, "CreateStack", &borrowed).await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected HTTP 200 from CreateStack({stack:?}), got {} with {:?}",
        created.status,
        created.text()
    );
}

/// Poll `stack` until it reports a status that is not `*_IN_PROGRESS`, and answer that status
/// with the answer it was read from.
pub(crate) async fn settled_status(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let answered = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        let body = answered.text();
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        let settled = answered.status != 200
            || (!reported.is_empty() && !reported.ends_with("_IN_PROGRESS"));
        if settled || Instant::now() >= giving_up_at {
            return (reported, body);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Every resource failure the stack reported, with the reason it gave — what a row that did
/// not reach the status it must quotes.
pub(crate) async fn reported_failures(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let answered = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    let body = answered.text();
    members_of(&body)
        .into_iter()
        .filter_map(|member| {
            let status = elements(member, "ResourceStatus").into_iter().next()?;
            if !status.contains("FAILED") {
                return None;
            }
            let logical = elements(member, "LogicalResourceId")
                .into_iter()
                .next()
                .unwrap_or_default();
            let reason = elements(member, "ResourceStatusReason")
                .into_iter()
                .next()
                .unwrap_or_default();
            Some(format!("{logical} {status}: {reason}"))
        })
        .collect()
}

/// Deploy `template` as `stack` and answer the `DescribeStacks` body of the completed stack,
/// failing with every reason the deployment recorded when it did not complete.
pub(crate) async fn completed_stack(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
    parameters: &[(&str, &str)],
) -> String {
    create_stack(router, case, stack, template, parameters).await;
    let (settled, described) = settled_status(router, stack).await;
    if settled != CREATE_COMPLETE {
        let failures = reported_failures(router, stack).await;
        panic!(
            "case {case:?}: expected {stack:?} to settle at {CREATE_COMPLETE}, got {settled:?}; \
             the failures it reported: {failures:?}; last answer {described}"
        );
    }
    described
}

/// Each `Outputs` entry of a `DescribeStacks` answer as the pair of members a client reads —
/// `(OutputKey, OutputValue)`, each absent where the entry carries none.
///
/// Read from the `Outputs` block and then per `<member>`, so a key is never paired with
/// another entry's value and no element of another list in the envelope is swept up: an entry
/// carrying a key and NO value is exactly what this suite is about, so it has to come back as
/// such rather than being dropped.
pub(crate) fn output_entries(described: &str) -> Vec<(Option<String>, Option<String>)> {
    let Some(block) = crate::query_wire::block_of(described, "Outputs") else {
        return Vec::new();
    };
    members_of(block)
        .into_iter()
        .map(|member| {
            (
                elements(member, "OutputKey").into_iter().next(),
                elements(member, "OutputValue").into_iter().next(),
            )
        })
        .collect()
}

/// Hold a completed stack's whole `Outputs` list to the rule — every entry carries a key AND a
/// value — and answer it as the map a client builds out of it.
///
/// `expected_keys` is what the deployed template publishes, so an output the stack dropped and
/// one it invented are both reported.
pub(crate) fn outputs_of_a_completed_stack(
    case: &str,
    described: &str,
    expected_keys: &[&str],
) -> BTreeMap<String, String> {
    let entries = output_entries(described);
    let valueless: Vec<String> = entries
        .iter()
        .filter(|(_, value)| value.is_none())
        .map(|(key, _)| key.clone().unwrap_or_default())
        .collect();
    assert!(
        valueless.is_empty(),
        "case {case:?}: expected every Outputs entry of a {CREATE_COMPLETE} stack to carry its \
         OutputValue, so a client can build {{OutputKey: OutputValue}} over the whole list; \
         {} of them carry none: {valueless:?} in {described}",
        valueless.len()
    );
    let keyless = entries.iter().filter(|(key, _)| key.is_none()).count();
    assert_eq!(
        keyless, 0,
        "case {case:?}: expected every Outputs entry to carry its OutputKey, got {keyless} \
         without one in {described}"
    );
    let built: BTreeMap<String, String> = entries
        .into_iter()
        .filter_map(|(key, value)| Some((key?, value?)))
        .collect();
    let reported: BTreeSet<&str> = built.keys().map(String::as_str).collect();
    let declared: BTreeSet<&str> = expected_keys.iter().copied().collect();
    assert_eq!(
        reported, declared,
        "case {case:?}: expected the stack to report exactly the outputs its template \
         publishes, got {reported:?} for {declared:?} in {described}"
    );
    built
}

/// What one row's output value must be.
enum Expected {
    /// The value, computed here from what the row declares.
    Exactly(String),
    /// A value this suite cannot compute in full — an address this emulator decides the host
    /// of — which must still carry what the declaration fixes about it.
    Containing(String),
}

/// One row of the table: an output, the `Value` its declaration carries, and what it must
/// resolve to.
struct Row {
    case: &'static str,
    key: &'static str,
    /// The declared `Value`, as the template spells it.
    value: fn() -> Value,
    /// Whether the declaration also carries a `Description` and an `Export`.
    exported: bool,
    expected: fn() -> Expected,
}

/// The ARN AWS SQS gives the queue the template declares, built from the declaration.
fn queue_arn() -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{QUEUE_NAME}")
}

/// The ARN Amazon SNS gives the topic the template declares, built from the declaration.
fn topic_arn() -> String {
    format!("arn:aws:sns:{REGION}:{ACCOUNT}:{TOPIC_NAME}")
}

const ROWS: &[Row] = &[
    Row {
        case: "a Ref to a queue, whose value is the URL a client calls it by",
        key: "QueueUrl",
        value: || json!({ "Ref": QUEUE }),
        exported: false,
        // "Ref returns the queue URL": the host is this emulator's own endpoint, so what the
        // declaration fixes about it is the queue's name.
        expected: || Expected::Containing(QUEUE_NAME.to_string()),
    },
    Row {
        case: "an Fn::GetAtt on a queue's ARN",
        key: "QueueArn",
        value: || json!({ "Fn::GetAtt": [QUEUE, "Arn"] }),
        exported: true,
        expected: || Expected::Exactly(queue_arn()),
    },
    Row {
        case: "an Fn::GetAtt on a queue's name",
        key: "QueueName",
        value: || json!({ "Fn::GetAtt": [QUEUE, "QueueName"] }),
        exported: false,
        expected: || Expected::Exactly(QUEUE_NAME.to_string()),
    },
    Row {
        case: "a Ref to a bucket, whose value is the bucket's name",
        key: "BucketName",
        value: || json!({ "Ref": BUCKET }),
        exported: false,
        expected: || Expected::Exactly(BUCKET_NAME.to_string()),
    },
    Row {
        case: "an Fn::GetAtt on a bucket's ARN, a resource of another service",
        key: "BucketArn",
        value: || json!({ "Fn::GetAtt": [BUCKET, "Arn"] }),
        exported: false,
        expected: || Expected::Exactly(format!("arn:aws:s3:::{BUCKET_NAME}")),
    },
    Row {
        case: "a Ref to a topic, whose value is the topic's ARN",
        key: "TopicArn",
        value: || json!({ "Ref": TOPIC }),
        exported: false,
        expected: || Expected::Exactly(topic_arn()),
    },
    Row {
        case: "an Fn::GetAtt on a topic's name",
        key: "TopicName",
        value: || json!({ "Fn::GetAtt": [TOPIC, "TopicName"] }),
        exported: false,
        expected: || Expected::Exactly(TOPIC_NAME.to_string()),
    },
    Row {
        case: "a Ref to a parameter the deployment passed a value for",
        key: "Tier",
        value: || json!({ "Ref": PARAMETER }),
        exported: false,
        expected: || Expected::Exactly(PARAMETER_VALUE.to_string()),
    },
    Row {
        case: "a Ref to the AWS::Region pseudo parameter",
        key: "Region",
        value: || json!({ "Ref": "AWS::Region" }),
        exported: false,
        expected: || Expected::Exactly(REGION.to_string()),
    },
    Row {
        case: "a Ref to the AWS::AccountId pseudo parameter",
        key: "Account",
        value: || json!({ "Ref": "AWS::AccountId" }),
        exported: false,
        expected: || Expected::Exactly(ACCOUNT.to_string()),
    },
    Row {
        case: "a Ref to the AWS::Partition pseudo parameter",
        key: "Partition",
        value: || json!({ "Ref": "AWS::Partition" }),
        exported: false,
        expected: || Expected::Exactly("aws".to_string()),
    },
    Row {
        case: "a Ref to the AWS::URLSuffix pseudo parameter",
        key: "UrlSuffix",
        value: || json!({ "Ref": "AWS::URLSuffix" }),
        exported: false,
        expected: || Expected::Exactly("amazonaws.com".to_string()),
    },
    Row {
        case: "a Ref to the AWS::StackId pseudo parameter",
        key: "StackId",
        value: || json!({ "Ref": "AWS::StackId" }),
        exported: false,
        // The id is minted per stack, so what the declaration fixes about it is the form every
        // stack id carries.
        expected: || {
            Expected::Containing(format!("arn:aws:cloudformation:{REGION}:{ACCOUNT}:stack/"))
        },
    },
    Row {
        case: "an Fn::Sub over a pseudo parameter and a parameter",
        key: "Subbed",
        value: || json!({ "Fn::Sub": format!("${{AWS::Region}}/${{{PARAMETER}}}") }),
        exported: false,
        expected: || Expected::Exactly(format!("{REGION}/{PARAMETER_VALUE}")),
    },
    Row {
        case: "an Fn::Sub in its two-argument form, over a variable it declares itself",
        key: "SubbedWithVariables",
        value: || {
            json!({ "Fn::Sub": [
                "${Who} reads ${Name}",
                { "Who": "the-consumer", "Name": { "Fn::GetAtt": [QUEUE, "QueueName"] } },
            ] })
        },
        exported: false,
        expected: || Expected::Exactly(format!("the-consumer reads {QUEUE_NAME}")),
    },
    Row {
        case: "an Fn::Join over two resolved references",
        key: "Joined",
        value: || json!({ "Fn::Join": ["|", [{ "Ref": BUCKET }, { "Fn::GetAtt": [QUEUE, "Arn"] }]] }),
        exported: false,
        expected: || Expected::Exactly(format!("{BUCKET_NAME}|{}", queue_arn())),
    },
    Row {
        case: "an Fn::Select over an Fn::Split of a resolved ARN",
        key: "Selected",
        value: || {
            json!({ "Fn::Select": ["3", { "Fn::Split": [":", { "Fn::GetAtt": [QUEUE, "Arn"] }] }] })
        },
        exported: false,
        // The fourth segment of an ARN is its Region.
        expected: || Expected::Exactly(REGION.to_string()),
    },
    Row {
        case: "an Fn::FindInMap over the template's own mapping",
        key: "Mapped",
        value: || json!({ "Fn::FindInMap": [MAP, { "Ref": PARAMETER }, MAP_ENTRY] }),
        exported: false,
        expected: || Expected::Exactly(MAP_VALUE.to_string()),
    },
    Row {
        case: "an Fn::Base64 of the text it is given",
        key: "Encoded",
        value: || json!({ "Fn::Base64": "payload" }),
        exported: false,
        // The base64 of "payload", which is what the function answers for it.
        expected: || Expected::Exactly("cGF5bG9hZA==".to_string()),
    },
    Row {
        case: "an Fn::If whose condition holds",
        key: "Chosen",
        value: || json!({ "Fn::If": [CONDITION, { "Ref": BUCKET }, "not-production"] }),
        exported: false,
        expected: || Expected::Exactly(BUCKET_NAME.to_string()),
    },
];

/// The template the table deploys: the three resources its outputs name, the parameter, the
/// mapping and the condition they read, and one output per row.
fn table_template(stack: &str) -> String {
    let mut outputs = Map::new();
    for row in ROWS {
        let mut declared = json!({ "Value": (row.value)() });
        if row.exported {
            declared["Description"] = json!(format!("what {} publishes", row.key));
            declared["Export"] = json!({ "Name": format!("{stack}-{}", row.key) });
        }
        outputs.insert(row.key.to_string(), declared);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Parameters": { PARAMETER: { "Type": "String" } },
        "Mappings": { MAP: { MAP_KEY: { MAP_ENTRY: MAP_VALUE } } },
        "Conditions": {
            CONDITION: { "Fn::Equals": [{ "Ref": PARAMETER }, PARAMETER_VALUE] },
        },
        "Resources": {
            QUEUE: {
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": QUEUE_NAME },
            },
            BUCKET: {
                "Type": "AWS::S3::Bucket",
                "Properties": { "BucketName": BUCKET_NAME },
            },
            TOPIC: {
                "Type": "AWS::SNS::Topic",
                "Properties": { "TopicName": TOPIC_NAME },
            },
        },
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// Hold one output to what its row declares it must be.
fn assert_expected(case: &str, key: &str, reported: &str, expected: &Expected) {
    match expected {
        Expected::Exactly(value) => assert_eq!(
            reported, value,
            "case {case:?}: expected the output {key:?} to resolve to {value:?}, got {reported:?}"
        ),
        Expected::Containing(part) => assert!(
            reported.contains(part.as_str()),
            "case {case:?}: expected the output {key:?} to carry {part:?}, got {reported:?}"
        ),
    }
}

#[tokio::test]
async fn every_declared_output_of_a_completed_stack_carries_its_resolved_value() {
    let router = emulator();
    let stack = "stack-output-values";
    let keys: Vec<&str> = ROWS.iter().map(|row| row.key).collect();

    let described = completed_stack(
        &router,
        "the output forms one template publishes",
        stack,
        &table_template(stack),
        &[(PARAMETER, PARAMETER_VALUE)],
    )
    .await;

    // The rule itself: the whole list is a map a client can build, with one entry per declared
    // output and nothing valueless in it.
    let outputs = outputs_of_a_completed_stack(
        "the output forms one template publishes",
        &described,
        &keys,
    );

    // ...and each entry carries the value its own declaration resolves to.
    for row in ROWS {
        let reported = outputs.get(row.key).unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the stack to report the output {:?}, got {outputs:?}",
                row.case, row.key
            )
        });
        assert_expected(row.case, row.key, reported, &(row.expected)());
    }
}

/// The export name the publishing stack below publishes its queue's ARN under.
const EXPORT_NAME: &str = "stack-output-values-exported-queue-arn";

/// An output reading ANOTHER stack's exported value carries it too: the publishing stack's own
/// exported output and the importing stack's `Fn::ImportValue` are both values, so neither
/// stack reports a key with nothing under it.
#[tokio::test]
async fn an_exported_value_and_the_output_that_imports_it_both_carry_values() {
    let case = "an exported output and the output of the stack that imports it";
    let router = emulator();
    let publisher = "stack-output-values-publisher";
    let consumer = "stack-output-values-consumer";
    let queue = "stack-output-values-exported-queue";
    let arn = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{queue}");

    let publishing = json!({
        "Resources": {
            QUEUE: { "Type": "AWS::SQS::Queue", "Properties": { "QueueName": queue } },
        },
        "Outputs": {
            "QueueArn": {
                "Value": { "Fn::GetAtt": [QUEUE, "Arn"] },
                "Description": "the queue the consumer reads",
                "Export": { "Name": EXPORT_NAME },
            },
        },
    })
    .to_string();
    let described = completed_stack(&router, case, publisher, &publishing, &[]).await;
    let published = outputs_of_a_completed_stack(case, &described, &["QueueArn"]);
    assert_expected(
        case,
        "QueueArn",
        published
            .get("QueueArn")
            .map(String::as_str)
            .unwrap_or_default(),
        &Expected::Exactly(arn.clone()),
    );

    let importing = json!({
        "Resources": {
            TOPIC: {
                "Type": "AWS::SNS::Topic",
                "Properties": { "TopicName": "stack-output-values-consumer-topic" },
            },
        },
        "Outputs": {
            "ImportedArn": { "Value": { "Fn::ImportValue": EXPORT_NAME } },
            "ComposedArn": {
                "Value": { "Fn::Join": ["/", ["from", { "Fn::ImportValue": EXPORT_NAME }]] },
            },
        },
    })
    .to_string();
    let described = completed_stack(&router, case, consumer, &importing, &[]).await;
    let imported = outputs_of_a_completed_stack(case, &described, &["ImportedArn", "ComposedArn"]);
    assert_expected(
        case,
        "ImportedArn",
        imported
            .get("ImportedArn")
            .map(String::as_str)
            .unwrap_or_default(),
        &Expected::Exactly(arn.clone()),
    );
    assert_expected(
        case,
        "ComposedArn",
        imported
            .get("ComposedArn")
            .map(String::as_str)
            .unwrap_or_default(),
        &Expected::Exactly(format!("from/{arn}")),
    );
}

/// An output the template declares under a condition is reported with its value when the
/// condition holds and not reported at all when it does not — "output values that are
/// conditionally created" — so a client never reads a key with nothing under it either way.
#[tokio::test]
async fn a_conditional_output_is_reported_with_its_value_or_not_at_all() {
    // (case, the parameter value the deployment passes, the outputs the stack must report)
    let rows: [(&str, &str, &[&str]); 2] = [
        (
            "the condition holds",
            PARAMETER_VALUE,
            &["Always", "WhenProduction"],
        ),
        ("the condition does not hold", "staging", &["Always"]),
    ];

    // ONE emulator for both rows: each names its own stack and bucket, so the rows cannot
    // reach one another's state and the assembled router is built once.
    let router = emulator();
    for (index, (case, tier, expected_keys)) in rows.into_iter().enumerate() {
        let stack = format!("stack-output-values-conditional-{index}");
        let template = json!({
            "Parameters": { PARAMETER: { "Type": "String" } },
            "Conditions": {
                CONDITION: { "Fn::Equals": [{ "Ref": PARAMETER }, PARAMETER_VALUE] },
            },
            "Resources": {
                BUCKET: {
                    "Type": "AWS::S3::Bucket",
                    "Properties": { "BucketName": format!("{stack}-bucket") },
                },
            },
            "Outputs": {
                "Always": { "Value": { "Ref": BUCKET } },
                "WhenProduction": {
                    "Condition": CONDITION,
                    "Value": { "Fn::GetAtt": [BUCKET, "Arn"] },
                },
            },
        })
        .to_string();

        let described =
            completed_stack(&router, case, &stack, &template, &[(PARAMETER, tier)]).await;
        let outputs = outputs_of_a_completed_stack(case, &described, expected_keys);
        assert_expected(
            case,
            "Always",
            outputs.get("Always").map(String::as_str).unwrap_or_default(),
            &Expected::Exactly(format!("{stack}-bucket")),
        );
        if expected_keys.contains(&"WhenProduction") {
            assert_expected(
                case,
                "WhenProduction",
                outputs
                    .get("WhenProduction")
                    .map(String::as_str)
                    .unwrap_or_default(),
                &Expected::Exactly(format!("arn:aws:s3:::{stack}-bucket")),
            );
        }
    }
}
