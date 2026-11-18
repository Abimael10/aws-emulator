// SPDX-License-Identifier: CC0-1.0

//! Every one of AppConfig's AWS-authored deployment-event notification extensions DELIVERS
//! a deployment's lifecycle to the destination that extension's own published behaviour
//! names — asserted through the WHOLE emulator, where a client observes it.
//!
//! The RULE under test: **an extension AppConfig itself authors publishes no `Action$Uri`
//! ("the extension URI associated to the action point in the extension definition" is
//! OPTIONAL, `smithy_query(query="shape",shape="Action")`), so each action it binds resolves
//! its destination from what the extension itself publishes — for a notification extension
//! that names a parameter, the ARN the bound ASSOCIATION supplies for that parameter
//! ("parameter values are specified in an extension association",
//! `smithy_query(query="doc",shape="Parameter")`), and for the one whose published behaviour
//! names the account's default Amazon EventBridge event bus, that bus — and it delivers the
//! lifecycle event at each action point it binds, carrying the action point in PascalCase
//! (`Type`), the `DeploymentNumber`, the `ConfigurationVersion`, the `Application`,
//! `Environment` and `ConfigurationProfile` the deployment runs on and the association's own
//! `Parameters`. The association IS the binding: once it is deleted the next deployment
//! delivers nothing (while still deploying), and re-creating it restores delivery for the
//! deployments that follow.**
//!
//! Only a suite over the production service set can state that: the destination of an
//! authored notification extension is a resource of ANOTHER service — an Amazon SQS queue, an
//! Amazon SNS topic, the default EventBridge bus — the delivery AppConfig makes is an
//! in-process call addressed the way a client's own call to that service is addressed, so
//! which mounted model claims it is only decidable with every service mounted, and what
//! arrived is read where a consumer reads it (off the queue, off the queue subscribed to the
//! topic, off the queue a rule on the bus targets). The AppConfig crate's own suites
//! (`tests/all/authored_notification_action_points.rs`,
//! `tests/all/deployment_event_notifications_to_sqs.rs`) pin the queue extension's own points
//! and body over tables of stages and binding levels with a hand-mounted pair of services;
//! what only this suite reaches is the same contract for EVERY destination KIND the published
//! catalogue has, over the service set the binary assembles.
//!
//! The rows differ along what the rule generalises over: which published destination the
//! extension's action resolves to and how (a queue ARN supplied for a parameter, a topic ARN
//! supplied for another parameter, the default event bus supplied by nothing), which
//! identifier form the association names the extension by (its ARN, its published name, its
//! published id), and the configuration version each of a row's three deployments rolls out.
//! Each row runs on a FRESH emulator, so no row's wiring can deliver another row's events.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    given_topic_with_subscriber, Grant,
};
use crate::ecr_scan_on_push_event_reaches_its_queue_target::{
    authorization, events, member, queue, router, sqs, target, DEFAULT_BUS, REGION,
};
use crate::query_wire::form_encode;

/// The ARN field an extension AppConfig itself authors leaves EMPTY, because no account owns
/// it — the marker a client picks the authored entries out of `ListExtensions` by.
const AUTHORED_ARN_MARKER: &str = "::extension/";

/// The action points AppConfig's deployment-event notification extensions bind, as the
/// `Actions` map of such an extension is keyed (members of the model's `ActionPoint` enum,
/// `smithy_query(query="shape",shape="ActionPoint")`). Spelled out here rather than imported
/// from the service crate, so a wrong constant in the implementation cannot make this suite
/// pass.
const LIFECYCLE_ACTION_POINTS: &[&str] = &[
    "ON_DEPLOYMENT_COMPLETE",
    "ON_DEPLOYMENT_ROLLED_BACK",
    "ON_DEPLOYMENT_START",
];

/// The `Type` of each notification a deployment nobody stopped owes, in the order its stages
/// happen: the action point in PascalCase, NOT the SCREAMING_SNAKE key the `Actions` map is
/// keyed by.
const DELIVERED_TYPES: &[&str] = &["OnDeploymentStart", "OnDeploymentComplete"];

/// The `source` an AppConfig event carries on the default event bus: every AWS service
/// publishes its own events under `aws.<service>`.
const APPCONFIG_SOURCE: &str = "aws.appconfig";

/// The service principal AppConfig performs its delivery into a queue as, which is what that
/// queue's own policy has to allow.
const APPCONFIG_PRINCIPAL: &str = "appconfig.amazonaws.com";

/// The `LocationUri` that selects AppConfig's own hosted configuration store.
const HOSTED: &str = "hosted";

/// The media type the configuration documents below travel with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The `DeploymentState` a deployment that reached every target and baked for no time ends
/// in (`smithy_query(query="shape",shape="DeploymentState")`).
const COMPLETE: &str = "COMPLETE";

/// How long a leg waits for what it expects — a deployment to settle, the notifications its
/// stages owe to arrive — and how often it looks again.
///
/// A GENEROUS ceiling on the WAIT, not a statement about how long anything takes: the spans a
/// rollout is paced by belong to AppConfig, so nothing here sleeps one on its behalf and
/// nothing asserts on elapsed time. Every look is a look at the CONDITION.
const OBSERVED_BY: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// How many further looks a leg that expects NOTHING takes, each re-reading the deployment —
/// which is what would dispatch an action point still owed — and then the destination.
///
/// Every stage of a deployment that has reached a terminal state has already happened, so
/// this bounds the LOOKING rather than waiting for a span to pass.
const SILENT_ROUNDS: usize = 5;

/// Where one authored notification extension's action delivers, and so how a consumer reads
/// what arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Destination {
    /// The Amazon SQS queue the association's parameter names: the message body IS the
    /// notification.
    Queue,
    /// The Amazon SNS topic the association's parameter names, read off a queue subscribed to
    /// it: the body is the SNS envelope and the notification is its `Message`.
    Topic,
    /// The account's default EventBridge event bus, read off a queue a rule on that bus
    /// targets: the body is the event envelope and the notification is its `detail`.
    DefaultBus,
}

/// Which of the three forms an `ExtensionIdentifier` accepts the association names the
/// extension by: "the name, the ID, or the Amazon Resource Name (ARN) of the extension"
/// (`smithy_query(query="doc",shape="CreateExtensionAssociationRequest",
/// member="ExtensionIdentifier")`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamedBy {
    Arn,
    Name,
    Id,
}

/// One row: one published notification extension, and the destination kind its action
/// resolves to.
struct Row {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    /// What the published NAME of this extension says about where it delivers, which is how a
    /// client that does not know the catalogue picks it out of `ListExtensions`.
    names: &'static str,
    /// Where its action delivers.
    destination: Destination,
    /// The published parameter the association supplies the destination's ARN for; `None` for
    /// the extension whose published destination the association supplies nothing for.
    parameter: Option<&'static str>,
    /// How the association names the extension.
    named_by: NamedBy,
}

/// The three rows, one per destination kind the published catalogue has. Each is a row of the
/// SAME rule: the destination comes from what the extension publishes, never from an
/// `Action$Uri` an authored extension does not have.
const TO_QUEUE: Row = Row {
    case: "the queue the association's parameter names",
    names: "SQS",
    destination: Destination::Queue,
    parameter: Some("queueArn"),
    named_by: NamedBy::Arn,
};
const TO_TOPIC: Row = Row {
    case: "the topic the association's parameter names",
    names: "SNS",
    destination: Destination::Topic,
    parameter: Some("topicArn"),
    named_by: NamedBy::Name,
};
const TO_DEFAULT_BUS: Row = Row {
    case: "the account's default event bus, which no parameter names",
    names: "EventBridge",
    destination: Destination::DefaultBus,
    parameter: None,
    named_by: NamedBy::Id,
};

/// What one dispatched REST call answered.
struct Answered {
    status: u16,
    body: Bytes,
}

impl Answered {
    /// The body as JSON, or `Value::Null` for an empty body.
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&self.body).to_string()))
    }

    /// The body as text, for a failure message.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// One REST call into the emulator: the endpoint host it is addressed at and the credential
/// scope that routes it to the service claiming it.
async fn rest(
    router: &MultiServiceRouter,
    service: &str,
    signing_name: &str,
    method: &str,
    path: &str,
    content_type: &str,
    body: &[u8],
) -> Answered {
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host.clone())
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type)
        .body(Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        body,
    }
}

/// One AppConfig CONTROL-plane call carrying a JSON body.
async fn appconfig(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &Value,
) -> Answered {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    rest(
        router,
        "appconfig",
        "appconfig",
        method,
        path,
        JSON_CONTENT_TYPE,
        &payload,
    )
    .await
}

/// One AppConfig DATA-plane call, as a polling client makes one. Both of AppConfig's APIs
/// sign as `appconfig`, whichever endpoint a client addresses.
async fn appconfigdata(router: &MultiServiceRouter, method: &str, path: &str) -> Answered {
    rest(
        router,
        "appconfigdata",
        "appconfig",
        method,
        path,
        JSON_CONTENT_TYPE,
        &[],
    )
    .await
}

/// One minted id out of a 201 answer.
fn minted(case: &str, what: &str, answered: &Answered) -> String {
    assert_eq!(
        answered.status,
        201,
        "case {case:?}: {what} must be created, got {} with {}",
        answered.status,
        answered.text()
    );
    answered
        .json()
        .get("Id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: {what} answered no Id: {}", answered.text()))
        .to_string()
}

/// An Amazon SQS queue whose own `Policy` grants `principal` the send on it — a cross-service
/// delivery is made as the delivering service's principal, so the queue's resource policy is
/// what admits it. Answers `(url, arn)`.
async fn given_granted_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    principal: &str,
) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({"QueueName": name})).await;
    let url = member(case, &created, "/QueueUrl");
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = member(case, &attributes, "/Attributes/QueueArn");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowAppConfigDelivery",
            "Effect": "Allow",
            "Principal": {"Service": principal},
            "Action": "sqs:SendMessage",
            "Resource": &arn,
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
    )
    .await;
    (url, arn)
}

/// The queue a consumer of this row reads what arrived off, and — for the two rows whose
/// destination an association names — the ARN the association supplies for the extension's
/// parameter.
async fn given_destination(
    router: &MultiServiceRouter,
    row: &Row,
    name: &str,
) -> (String, Option<String>) {
    let case = row.case;
    match row.destination {
        Destination::Queue => {
            let (url, arn) = given_granted_queue(router, case, name, APPCONFIG_PRINCIPAL).await;
            (url, Some(arn))
        }
        Destination::Topic => {
            let (topic, url) =
                given_topic_with_subscriber(router, case, REGION, name, Grant::Unscoped).await;
            (url, Some(topic))
        }
        Destination::DefaultBus => {
            // The bus itself needs no creating: the account has it. What a TEST needs is a
            // consumer on it — an enabled rule matching AppConfig's own events, delivering to
            // a queue that granted EventBridge the send.
            let (url, arn) = queue(router, case, name).await;
            events(
                router,
                case,
                "PutRule",
                &json!({
                    "Name": name,
                    "EventBusName": DEFAULT_BUS,
                    "EventPattern": json!({"source": [APPCONFIG_SOURCE]}).to_string(),
                    "State": "ENABLED",
                }),
            )
            .await;
            target(router, case, name, &arn).await;
            (url, None)
        }
    }
}

/// The authored extension in the account's catalogue whose published name says it delivers to
/// `row`'s destination, as `(id, name, arn)` — found the way a client finds it: by walking
/// `ListExtensions` and keeping the entries no account owns.
async fn discover(router: &MultiServiceRouter, row: &Row) -> (String, String, String) {
    let case = row.case;
    let listed = appconfig(router, "GET", "/extensions?max_results=50", &Value::Null).await;
    assert_eq!(
        listed.status,
        200,
        "case {case:?}: ListExtensions must succeed, got {} with {}",
        listed.status,
        listed.text()
    );
    let items = listed.json()["Items"].as_array().cloned().unwrap_or_default();
    let matching: Vec<Value> = items
        .iter()
        .filter(|item| {
            item["Arn"]
                .as_str()
                .is_some_and(|arn| arn.contains(AUTHORED_ARN_MARKER))
                && item["Name"]
                    .as_str()
                    .is_some_and(|name| name.contains(row.names))
        })
        .cloned()
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "case {case:?}: expected exactly one AWS-authored extension whose name names {:?}, got \
         {matching:?} out of {items:?}",
        row.names
    );
    let found = &matching[0];
    (
        found["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the entry reports no Id: {found}"))
            .to_string(),
        found["Name"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the entry reports no Name: {found}"))
            .to_string(),
        found["Arn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the entry reports no Arn: {found}"))
            .to_string(),
    )
}

/// Assert the extension `id` binds exactly the deployment-lifecycle action points and
/// publishes no `Uri` on any of them.
async fn assert_binds_the_lifecycle(router: &MultiServiceRouter, case: &str, id: &str) {
    let read = appconfig(router, "GET", &format!("/extensions/{id}"), &Value::Null).await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetExtension({id}) must succeed, got {} with {}",
        read.status,
        read.text()
    );
    let document = read.json();
    let bound: BTreeSet<String> = document["Actions"]
        .as_object()
        .cloned()
        .unwrap_or_default()
        .keys()
        .cloned()
        .collect();
    let expected: BTreeSet<String> = LIFECYCLE_ACTION_POINTS
        .iter()
        .map(|point| (*point).to_string())
        .collect();
    assert_eq!(
        bound, expected,
        "case {case:?}: expected the deployment-lifecycle action points, got {document}"
    );
    for (point, actions) in document["Actions"].as_object().cloned().unwrap_or_default() {
        for action in actions.as_array().cloned().unwrap_or_default() {
            assert_eq!(
                action.get("Uri"),
                None,
                "case {case:?} / {point}: an authored extension publishes no Uri, got {action}"
            );
        }
    }
}

/// Bind `extension` to `resource_arn` with `parameters`, and assert the association reports
/// them back. Answers the association's id.
async fn associate(
    router: &MultiServiceRouter,
    case: &str,
    extension: &str,
    resource_arn: &str,
    parameters: &Value,
) -> String {
    let mut body = json!({
        "ExtensionIdentifier": extension,
        "ResourceIdentifier": resource_arn,
    });
    if !parameters.as_object().is_none_or(serde_json::Map::is_empty) {
        body["Parameters"] = parameters.clone();
    }
    let created = appconfig(router, "POST", "/extensionassociations", &body).await;
    let id = minted(case, "the extension association", &created);
    let read = appconfig(
        router,
        "GET",
        &format!("/extensionassociations/{id}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetExtensionAssociation must succeed, got {} with {}",
        read.status,
        read.text()
    );
    assert_eq!(
        read.json()["Parameters"],
        parameters.clone(),
        "case {case:?}: the association must report the parameters it was created with, got {}",
        read.text()
    );
    id
}

/// Delete one association and assert it is gone: the modelled `ResourceNotFoundException`
/// (HTTP 404) whose `Message` names the kind and the account the lookup was scoped to.
async fn disassociate(router: &MultiServiceRouter, case: &str, association: &str) {
    let path = format!("/extensionassociations/{association}");
    let deleted = appconfig(router, "DELETE", &path, &Value::Null).await;
    assert_eq!(
        deleted.status,
        204,
        "case {case:?}: DeleteExtensionAssociation must succeed, got {} with {}",
        deleted.status,
        deleted.text()
    );
    let read = appconfig(router, "GET", &path, &Value::Null).await;
    assert_eq!(
        read.status,
        404,
        "case {case:?}: a deleted association is gone, got {} with {}",
        read.status,
        read.text()
    );
    let code = read.json()["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        code, "ResourceNotFoundException",
        "case {case:?}: expected the modelled refusal, got {}",
        read.text()
    );
    let expected = format!(
        "ExtensionAssociation could not be found for account {}.",
        ::engine::emulator_account_id()
    );
    assert_eq!(
        read.json()["Message"].as_str(),
        Some(expected.as_str()),
        "case {case:?}: expected the refusal {expected:?}, got {}",
        read.text()
    );
}

/// One application tree with three distinguishable configuration versions, and the strategy
/// every deployment of the row uses. Answers `(application, environment, profile, strategy)`.
async fn given_tree(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String, String, String) {
    let application = minted(
        case,
        "the application",
        &appconfig(router, "POST", "/applications", &json!({"Name": name})).await,
    );
    let environment = minted(
        case,
        "the environment",
        &appconfig(
            router,
            "POST",
            &format!("/applications/{application}/environments"),
            &json!({"Name": name}),
        )
        .await,
    );
    let profile = minted(
        case,
        "the configuration profile",
        &appconfig(
            router,
            "POST",
            &format!("/applications/{application}/configurationprofiles"),
            &json!({"Name": name, "LocationUri": HOSTED}),
        )
        .await,
    );
    for version in 1..=3 {
        let written = rest(
            router,
            "appconfig",
            "appconfig",
            "POST",
            &format!(
                "/applications/{application}/configurationprofiles/{profile}\
                 /hostedconfigurationversions"
            ),
            JSON_CONTENT_TYPE,
            deployed_bytes(version).as_bytes(),
        )
        .await;
        assert_eq!(
            written.status,
            201,
            "case {case:?}: writing version {version} must succeed, got {} with {}",
            written.status,
            written.text()
        );
    }
    // A strategy that declares a rollout window, so the deployment is still in flight when
    // the start answers and the stages after it are reached by the CLOCK rather than by the
    // call — which is the shape a notification of a deployment's lifecycle is about. It
    // reaches every target in one growth step and monitors for no bake time, so a deployment
    // of it settles without any leg waiting on another's.
    let strategy = minted(
        case,
        "the deployment strategy",
        &appconfig(
            router,
            "POST",
            "/deploymentstrategies",
            &json!({
                "Name": name,
                "DeploymentDurationInMinutes": 1,
                "FinalBakeTimeInMinutes": 0,
                "GrowthFactor": 100,
                "GrowthType": "LINEAR",
                "ReplicateTo": "NONE",
            }),
        )
        .await,
    );
    (application, environment, profile, strategy)
}

/// The bytes of one configuration version, distinguishable per version so the data plane
/// says WHICH version it is serving.
fn deployed_bytes(version: i64) -> String {
    format!(r#"{{"version":{version}}}"#)
}

/// Start a deployment of `version` and answer the deployment number it was issued.
async fn start(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    profile: &str,
    strategy: &str,
    version: i64,
) -> i64 {
    let started = appconfig(
        router,
        "POST",
        &format!("/applications/{application}/environments/{environment}/deployments"),
        &json!({
            "DeploymentStrategyId": strategy,
            "ConfigurationProfileId": profile,
            "ConfigurationVersion": version.to_string(),
        }),
    )
    .await;
    assert_eq!(
        started.status,
        201,
        "case {case:?}: StartDeployment of version {version} must be accepted, got {} with {}",
        started.status,
        started.text()
    );
    started.json()["DeploymentNumber"]
        .as_i64()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: StartDeployment answered no DeploymentNumber: {}",
                started.text()
            )
        })
}

/// Read one deployment once — which is also what gives the action points the clock has made
/// due somewhere to run, the way a client watching its own rollout does.
async fn read_deployment(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i64,
) -> Value {
    let read = appconfig(
        router,
        "GET",
        &format!("/applications/{application}/environments/{environment}/deployments/{number}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetDeployment({number}) must succeed, got {} with {}",
        read.status,
        read.text()
    );
    read.json()
}

/// Read one deployment again until it reports `COMPLETE`, or the deadline passes. The wait is
/// on the CONDITION.
async fn completed(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i64,
) {
    let deadline = Instant::now() + OBSERVED_BY;
    loop {
        let document = read_deployment(router, case, application, environment, number).await;
        let state = document["State"].as_str().unwrap_or_default().to_string();
        if state == COMPLETE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: deployment {number} never completed, it is still {state:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The notification one message carries, read out of the envelope its destination kind
/// delivers it in.
fn notification(case: &str, destination: Destination, body: &str) -> Value {
    let document: Value = serde_json::from_str(body).unwrap_or_else(|error| {
        panic!("case {case:?}: a delivery must be a JSON document ({error}): {body:?}")
    });
    match destination {
        Destination::Queue => document,
        Destination::Topic => {
            let inner = document["Message"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: expected the SNS envelope's Message: {document}")
            });
            serde_json::from_str(inner).unwrap_or_else(|error| {
                panic!("case {case:?}: the notification must be JSON ({error}): {inner:?}")
            })
        }
        Destination::DefaultBus => {
            assert_eq!(
                document["source"],
                json!(APPCONFIG_SOURCE),
                "case {case:?}: expected AppConfig's own source, got {document}"
            );
            let detail = document["detail"].clone();
            // The envelope's `detail-type` is the action point the event belongs to, exactly
            // as the notification's own `Type` spells it.
            assert_eq!(
                document["detail-type"], detail["Type"],
                "case {case:?}: expected the detail type to be the action point, got {document}"
            );
            detail
        }
    }
}

/// Every notification waiting on `url` right now, each DELETED as it is read, so a later look
/// sees only what arrived since and nothing is counted twice.
async fn take(
    router: &MultiServiceRouter,
    case: &str,
    destination: Destination,
    url: &str,
) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut collected = Vec::with_capacity(messages.len());
    for message in &messages {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
        collected.push(notification(
            case,
            destination,
            message["Body"].as_str().unwrap_or_default(),
        ));
    }
    collected
}

/// `GetQueueAttributes`' `ApproximateNumberOfMessages`, as the string Amazon SQS reports it.
async fn queued(router: &MultiServiceRouter, case: &str, url: &str) -> String {
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["ApproximateNumberOfMessages"]}),
    )
    .await;
    member(case, &attributes, "/Attributes/ApproximateNumberOfMessages")
}

/// The notifications of one deployment, collected by looking again — and re-reading the
/// deployment, which is what drives the points the clock has made due — until as many as its
/// stages owe have arrived, or the deadline passes.
async fn delivered(
    router: &MultiServiceRouter,
    row: &Row,
    application: &str,
    environment: &str,
    number: i64,
    url: &str,
) -> Vec<Value> {
    let case = row.case;
    let deadline = Instant::now() + OBSERVED_BY;
    let mut collected: Vec<Value> = Vec::new();
    loop {
        collected.extend(take(router, case, row.destination, url).await);
        if collected.len() >= DELIVERED_TYPES.len() {
            return collected;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {} notifications of deployment {number}, got {collected:?}",
            DELIVERED_TYPES.len()
        );
        tokio::time::sleep(LOOK).await;
        read_deployment(router, case, application, environment, number).await;
    }
}

/// Assert `events` are the notifications a deployment nobody stopped owes, each naming that
/// deployment, the version it rolled out, the resources it ran on, and echoing the
/// association's own parameters.
fn assert_notifications(
    row: &Row,
    number: i64,
    version: i64,
    ids: (&str, &str, &str),
    parameters: &Value,
    events: &[Value],
) {
    let case = row.case;
    let types: Vec<String> = events
        .iter()
        .map(|event| event["Type"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        types,
        DELIVERED_TYPES
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<String>>(),
        "case {case:?}: expected the start and the completion, in the order the stages \
         happened, each in PascalCase, got {types:?}"
    );
    let (application, environment, profile) = ids;
    for event in events {
        let stage = event["Type"].as_str().unwrap_or_default();
        assert_eq!(
            event["DeploymentNumber"].as_i64(),
            Some(number),
            "case {case:?} / {stage}: expected deployment number {number}, got {event}"
        );
        assert_eq!(
            event["ConfigurationVersion"].as_str(),
            Some(version.to_string().as_str()),
            "case {case:?} / {stage}: expected configuration version {version}, got {event}"
        );
        for (member, id) in [
            ("Application", application),
            ("Environment", environment),
            ("ConfigurationProfile", profile),
        ] {
            assert_eq!(
                event[member]["Id"].as_str(),
                Some(id),
                "case {case:?} / {stage}: expected {member}.Id {id:?}, got {event}"
            );
        }
        // The association's own parameter values travel with the event, which is how the
        // destination knows which association it was delivered for.
        assert_eq!(
            event["Parameters"], *parameters,
            "case {case:?} / {stage}: expected the association's parameters {parameters}, got \
             {event}"
        );
    }
}

/// The configuration AppConfig's data plane serves ONE client right now, as its bytes.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    profile: &str,
) -> Vec<u8> {
    let session = rest(
        router,
        "appconfigdata",
        "appconfig",
        "POST",
        "/configurationsessions",
        JSON_CONTENT_TYPE,
        json!({
            "ApplicationIdentifier": application,
            "EnvironmentIdentifier": environment,
            "ConfigurationProfileIdentifier": profile,
        })
        .to_string()
        .as_bytes(),
    )
    .await;
    assert_eq!(
        session.status,
        201,
        "case {case:?}: StartConfigurationSession must succeed, got {} with {}",
        session.status,
        session.text()
    );
    let token = session.json()["InitialConfigurationToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: StartConfigurationSession answered no token: {}",
                session.text()
            )
        })
        .to_string();
    let read = appconfigdata(
        router,
        "GET",
        &format!("/configuration?configuration_token={}", form_encode(&token)),
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetLatestConfiguration must succeed, got {} with {}",
        read.status,
        read.text()
    );
    read.body.to_vec()
}

/// Assert the data plane really serves `version`'s bytes, looking again until it does or the
/// deadline passes: a deployment that delivered no notification must still have DEPLOYED.
async fn assert_in_service(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    profile: &str,
    version: i64,
) {
    let expected = deployed_bytes(version);
    let deadline = Instant::now() + OBSERVED_BY;
    loop {
        let bytes = served(router, case, application, environment, profile).await;
        if bytes == expected.as_bytes() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected version {version} in service ({expected:?}), got {:?}",
            String::from_utf8_lossy(&bytes)
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Run one row: the published extension's delivery, its absence once the association is
/// deleted, and its restoration once the association is re-created.
async fn run(row: &Row) {
    let case = row.case;
    let router = router();
    let name = "authored-notifications";
    let (url, destination_arn) = given_destination(&router, row, name).await;
    let (id, published_name, arn) = discover(&router, row).await;
    assert_binds_the_lifecycle(&router, case, &id).await;
    let identifier = match row.named_by {
        NamedBy::Arn => arn,
        NamedBy::Name => published_name,
        NamedBy::Id => id,
    };
    let parameters = match (row.parameter, destination_arn.as_deref()) {
        (Some(parameter), Some(value)) => json!({parameter: value}),
        // The extension whose published destination is the account's own default event bus:
        // its association supplies nothing, so it reports — and its events carry — an empty
        // parameter map.
        _ => json!({}),
    };

    let (application, environment, profile, strategy) = given_tree(&router, case, name).await;
    let environment_arn = ::engine::emulator_arn(
        "appconfig",
        REGION,
        &format!("application/{application}/environment/{environment}"),
    );

    // Bound: both stages of the deployment are delivered to the destination the extension
    // publishes.
    let association = associate(&router, case, &identifier, &environment_arn, &parameters).await;
    let first = start(
        &router, case, &application, &environment, &profile, &strategy, 1,
    )
    .await;
    completed(&router, case, &application, &environment, first).await;
    let events = delivered(&router, row, &application, &environment, first, &url).await;
    assert_notifications(
        row,
        first,
        1,
        (&application, &environment, &profile),
        &parameters,
        &events,
    );

    // Deleted: the next deployment delivers NOTHING — and it still deploys, the notification
    // being an observer of the deployment rather than a part of it.
    disassociate(&router, case, &association).await;
    let second = start(
        &router, case, &application, &environment, &profile, &strategy, 2,
    )
    .await;
    completed(&router, case, &application, &environment, second).await;
    assert_in_service(&router, case, &application, &environment, &profile, 2).await;
    for round in 1..=SILENT_ROUNDS {
        read_deployment(&router, case, &application, &environment, second).await;
        let extra = take(&router, case, row.destination, &url).await;
        assert!(
            extra.is_empty(),
            "case {case:?}: round {round}: an association that no longer exists delivers \
             nothing, got {extra:?}"
        );
    }
    let waiting = queued(&router, case, &url).await;
    assert_eq!(
        waiting, "0",
        "case {case:?}: expected the destination to be empty, got {waiting:?} messages"
    );

    // Re-created: delivery comes back, for the NEXT deployment, naming its own number.
    let restored = associate(&router, case, &identifier, &environment_arn, &parameters).await;
    assert_ne!(
        restored, association,
        "case {case:?}: a re-created association is a new resource, got {restored}"
    );
    let third = start(
        &router, case, &application, &environment, &profile, &strategy, 3,
    )
    .await;
    completed(&router, case, &application, &environment, third).await;
    let events = delivered(&router, row, &application, &environment, third, &url).await;
    assert!(
        third > second && second > first,
        "case {case:?}: expected three deployments in sequence, got {first}, {second}, {third}"
    );
    assert_notifications(
        row,
        third,
        3,
        (&application, &environment, &profile),
        &parameters,
        &events,
    );
    // And the third deployment really deployed too: what a client is served is its version.
    assert_in_service(&router, case, &application, &environment, &profile, 3).await;
}

/// RULE: the authored extension whose published behaviour names an Amazon SQS queue delivers
/// the deployment lifecycle to the queue the association's parameter names.
#[tokio::test]
async fn the_authored_sqs_extension_delivers_to_the_queue_its_association_names() {
    run(&TO_QUEUE).await;
}

/// RULE: the authored extension whose published behaviour names an Amazon SNS topic delivers
/// the same lifecycle to the topic the association's parameter names, so a queue subscribed
/// to that topic receives it.
#[tokio::test]
async fn the_authored_sns_extension_delivers_to_the_topic_its_association_names() {
    run(&TO_TOPIC).await;
}

/// RULE: the authored extension whose published behaviour names Amazon EventBridge delivers
/// the same lifecycle to the account's DEFAULT event bus, which no association parameter
/// names, so a rule on that bus delivers it to the rule's target.
#[tokio::test]
async fn the_authored_eventbridge_extension_delivers_to_the_default_event_bus() {
    run(&TO_DEFAULT_BUS).await;
}
