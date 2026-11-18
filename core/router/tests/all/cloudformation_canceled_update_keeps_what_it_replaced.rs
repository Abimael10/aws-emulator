// SPDX-License-Identifier: CC0-1.0

//! An `UpdateStack` CloudFormation CANCELS because another live stack imports one of the
//! stack's exported output values destroys NOTHING in the services that own the stack's
//! resources — not even the resource the update would have REPLACED.
//!
//! The rule under test: **an exported output value another live stack imports pins the VALUE,
//! so an update of the publishing stack is cancelled only where that value would move or the
//! export would go — an update that leaves every in-use export's value unchanged is carried
//! through, edits and all. And the physical resources an update supersedes — the old half of a
//! resource it replaces, and a resource it no longer declares — are taken down in the update's
//! cleanup phase, after the change itself has gone through, so an update that does NOT go
//! through takes none of them down: the service that owns each one still holds it, the stack's
//! record goes back onto the resource that is still in force rather than onto a new one, and
//! every value the stack publishes for it (its `PhysicalResourceId`, the outputs resolved from
//! it and the exports those outputs publish) is the value it published before the update.**
//!
//! Why it matters: the pre-condition that cancels an update which would move an exported value
//! another stack imports exists to keep that value published. An update that takes the
//! exporting resource down before the cancellation is decided moves the value anyway — the
//! rollback has nothing left to put back and can only create a resource under a NEW
//! identifier — so every consumer that imported the old handle is left calling the owning
//! service with an identifier it answers nothing for, which is exactly what the pin exists to
//! prevent. Two crates have to agree for the rule to hold (CloudFormation decides, the owning
//! service holds the resource), so the suite lives here: a CloudFormation-only test can see
//! the status, the reason and the published ids, but not whether the knowledge base or the
//! topic behind them survived.
//!
//! The rows differ along what the rule generalises over: WHAT the cancelled update would have
//! taken down (the old half of a replacement, a resource the revision drops, both at once),
//! an in-place revision of the exporting resource that moves no exported value and is carried
//! through while the export is pinned, and — as the other side of the rule — the same
//! replacing revision with nothing importing the export, which completes and does take the
//! replaced resource down. Driven over `AWS::Bedrock::KnowledgeBase`, whose `knowledgeBaseId`
//! Amazon Bedrock Agent mints and whose embedding model is the create-only value inside its
//! conditionally-replacing configuration, with an `AWS::SNS::Topic` as the resource a revision
//! drops.
//!
//! The awsQuery form encoder and the XML element readers are the test crate's own
//! ([`crate::query_wire`]), not another copy.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{
    decoded_entries, form_encode, members_of, opt_element, outputs_of, pairs_of, physical_ids_of,
};

/// The Region every request here is signed for, and so the Region the stacks and their
/// resources live in.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// How long a case waits for a stack operation to settle, and how often it looks again. What
/// is waited FOR is the condition (a status that is not an operation in flight); the deadline
/// is far longer than any span this emulator schedules, so a loaded host cannot decide a case.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The two embedding models the revisions declare. The model the knowledge base's vectors
/// were produced by is the create-only value inside `KnowledgeBaseConfiguration`, so a
/// revision that states the other one asks for a different knowledge base.
const TITAN_G1: &str = "amazon.titan-embed-text-v1";
const TITAN_V2: &str = "amazon.titan-embed-text-v2:0";

/// How many characters a minted `knowledgeBaseId` carries, and which they may be: the
/// modelled `Id` shape of `GetKnowledgeBase`'s `knowledgeBaseId` (`^[0-9a-zA-Z]{10}$`).
const KNOWLEDGE_BASE_ID_CHARACTERS: usize = 10;

/// The logical ids of the two resources the exporting stack declares.
const KNOWLEDGE_BASE: &str = "Escalation";
const TOPIC: &str = "Notifications";

/// The export the exporting stack publishes the knowledge base handle under, the output key
/// it publishes it on, and the name of the topic it creates in Amazon SNS.
const EXPORT: &str = "canceled-update-kb-handle";
const EXPORTED_OUTPUT: &str = "Exported";
const TOPIC_NAME: &str = "canceled-update-topic";

/// The two stacks every row deploys.
const EXPORTER: &str = "canceled-update-exporter";
const IMPORTER: &str = "canceled-update-importer";

/// The name the create gives the knowledge base, and the one a revision renames it to. A
/// replacement is created BESIDE the knowledge base it supersedes, which still holds the name
/// it was created under, so a replacing revision states another name.
const FIRST_NAME: &str = "canceled-update-first";
const SECOND_NAME: &str = "canceled-update-second";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `REGION`, which is
/// what routes the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000000000000000000000000000000000000000\
         000000000000000000000000"
    )
}

/// Call one awsQuery operation of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one CloudFormation operation (API version 2010-05-15).
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// One CloudFormation call that must be answered.
async fn cloudformation_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = cloudformation(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudFormation {action}, got {status} with {body}"
    );
    body
}

/// Call one Amazon SNS operation (API version 2010-03-31).
async fn sns(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "sns", "2010-03-31", action, members).await
}

/// `GetKnowledgeBase` for one knowledge base, over the `restJson1` wire Amazon Bedrock Agent's
/// model declares, on the method and URI that operation's own `@http` binding names, signed
/// for `bedrock` — the signing name every Bedrock-family model declares.
async fn get_knowledge_base(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("/knowledgebases/{id}"))
        .header("host", format!("bedrock.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("bedrock"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let document: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    let knowledge_base = document
        .get("knowledgeBase")
        .cloned()
        .unwrap_or(document.clone());
    (status, knowledge_base)
}

/// Whether `value` is an identifier of the form Amazon Bedrock Agent mints: exactly
/// [`KNOWLEDGE_BASE_ID_CHARACTERS`] alphanumeric characters, the whole of the modelled
/// pattern.
fn is_knowledge_base_id(value: &str) -> bool {
    value.len() == KNOWLEDGE_BASE_ID_CHARACTERS
        && value.chars().all(|character| character.is_ascii_alphanumeric())
}

/// The exporting stack's template: the knowledge base, the topic where the revision still
/// declares one, and the output that EXPORTS the handle the knowledge base publishes.
///
/// `suffix` is appended to the exported value, so a revision that leaves the knowledge base
/// alone can still ask to publish a different value under the export.
fn exporter_template(name: &str, model: &str, with_topic: bool, suffix: &str) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        KNOWLEDGE_BASE.to_string(),
        json!({
            "Type": "AWS::Bedrock::KnowledgeBase",
            "Properties": {
                "Name": name,
                "Description": "the escalation knowledge base",
                "RoleArn": format!("arn:aws:iam::{ACCOUNT}:role/canceled-update-role"),
                "KnowledgeBaseConfiguration": {
                    "Type": "VECTOR",
                    "VectorKnowledgeBaseConfiguration": {
                        "EmbeddingModelArn":
                            format!("arn:aws:bedrock:{REGION}::foundation-model/{model}")
                    }
                },
                "StorageConfiguration": {
                    "Type": "OPENSEARCH_SERVERLESS",
                    "OpensearchServerlessConfiguration": {
                        "CollectionArn":
                            format!("arn:aws:aoss:{REGION}:{ACCOUNT}:collection/canceled-update"),
                        "VectorIndexName": "canceled-update-index",
                        "FieldMapping": {
                            "VectorField": "bedrock-knowledge-base-default-vector",
                            "TextField": "AMAZON_BEDROCK_TEXT_CHUNK",
                            "MetadataField": "AMAZON_BEDROCK_METADATA"
                        }
                    }
                }
            }
        }),
    );
    if with_topic {
        resources.insert(
            TOPIC.to_string(),
            json!({
                "Type": "AWS::SNS::Topic",
                "Properties": { "TopicName": TOPIC_NAME }
            }),
        );
    }
    json!({
        "Resources": Value::Object(resources),
        "Outputs": {
            EXPORTED_OUTPUT: {
                "Value": { "Fn::Join": ["", [{ "Ref": KNOWLEDGE_BASE }, suffix]] },
                "Export": { "Name": EXPORT }
            }
        }
    })
    .to_string()
}

/// The importing stack's template: it reads the export into an output of its own, which is
/// what makes it an importer of that export.
fn importer_template() -> String {
    json!({
        "Resources": { "Handle": { "Type": "AWS::CloudFormation::WaitConditionHandle" } },
        "Outputs": { "Echoed": { "Value": { "Fn::ImportValue": EXPORT } } }
    })
    .to_string()
}

/// The status a row's revision settles the stack in.
const CANCELLED: &str = "UPDATE_ROLLBACK_COMPLETE";
const CARRIED_THROUGH: &str = "UPDATE_COMPLETE";

/// One row: what the revision asks for, whether the export is pinned while it is deployed, and
/// what the rule owes the resources it would have superseded.
struct Row {
    case: &'static str,
    /// The `Name` the revision gives the knowledge base.
    name: &'static str,
    /// The embedding model the revision declares.
    model: &'static str,
    /// Whether the revision still declares the topic the create made.
    keeps_the_topic: bool,
    /// What the revision appends to the exported value — the only way a revision that leaves
    /// the knowledge base alone can ask to publish a different value.
    suffix: &'static str,
    /// Whether another live stack IMPORTS the export while the revision is deployed.
    pinned: bool,
    /// The status the stack settles in: a revision that would move a pinned value is
    /// CANCELLED, and every other one is carried through.
    settles_in: &'static str,
    /// Whether the handle the stack publishes for the knowledge base is still the one that was
    /// in force before the revision.
    keeps_its_handle: bool,
    /// The `name` Amazon Bedrock Agent must report for the knowledge base the stack publishes
    /// afterwards: the revision's own where the revision landed, the one in force where it was
    /// cancelled.
    reports_name: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a cancelled update that would have replaced the exporting resource",
        name: SECOND_NAME,
        model: TITAN_V2,
        keeps_the_topic: true,
        suffix: "",
        pinned: true,
        settles_in: CANCELLED,
        keeps_its_handle: true,
        reports_name: FIRST_NAME,
    },
    Row {
        case: "a cancelled update that would also have dropped another resource",
        name: SECOND_NAME,
        model: TITAN_V2,
        keeps_the_topic: false,
        suffix: "",
        pinned: true,
        settles_in: CANCELLED,
        keeps_its_handle: true,
        reports_name: FIRST_NAME,
    },
    Row {
        case: "a cancelled update that leaves the exporting resource alone and drops another",
        name: FIRST_NAME,
        model: TITAN_G1,
        keeps_the_topic: false,
        suffix: "-revised",
        pinned: true,
        settles_in: CANCELLED,
        keeps_its_handle: true,
        reports_name: FIRST_NAME,
    },
    Row {
        case: "an in-place revision of the exporting resource, which leaves the exported value \
               unchanged and so is carried through while the export is pinned",
        name: SECOND_NAME,
        model: TITAN_G1,
        keeps_the_topic: true,
        suffix: "",
        pinned: true,
        settles_in: CARRIED_THROUGH,
        keeps_its_handle: true,
        reports_name: SECOND_NAME,
    },
    Row {
        case: "the same replacing revision with nothing importing the export, which completes",
        name: SECOND_NAME,
        model: TITAN_V2,
        keeps_the_topic: true,
        suffix: "",
        pinned: false,
        settles_in: CARRIED_THROUGH,
        keeps_its_handle: false,
        reports_name: SECOND_NAME,
    },
];

/// The reason a cancelled update carries, as AWS words it: the fixed text around the export
/// and the stack that still imports it, ending with its period. Written out here rather than
/// read off the emulator's own formatter, so the assertion pins the wording.
fn update_canceled_reason(export_name: &str, importing_stack_name: &str) -> String {
    format!(
        "Update canceled. Cannot update export {export_name} as it is in use by \
         {importing_stack_name}."
    )
}

/// One `DescribeStacks` body.
async fn describe(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    cloudformation_ok(router, case, "DescribeStacks", &[("StackName", stack)]).await
}

/// Poll `DescribeStacks` the way a client's waiter does and answer the body that reported the
/// stack SETTLED — the first read whose `StackStatus` is not an operation in flight.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let body = describe(router, case, stack).await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with("_IN_PROGRESS") {
            return body;
        }
        if seen.last().map(String::as_str) != Some(reported.as_str()) {
            seen.push(reported);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {stack} to settle, saw {seen:?}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// Create a stack and assert the create completed.
async fn create_and_settle(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) {
    let created = cloudformation_ok(
        router,
        case,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", template)],
    )
    .await;
    assert!(
        opt_element(&created, "StackId").is_some(),
        "case {case:?}: expected a StackId for {stack}, got {created}"
    );
    let described = settled(router, case, stack).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected {stack} to be created, got {described}"
    );
}

/// The logical-id to physical-id map one stack reports.
async fn reported_resources(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> BTreeMap<String, String> {
    let body = cloudformation_ok(
        router,
        case,
        "DescribeStackResources",
        &[("StackName", stack)],
    )
    .await;
    physical_ids_of(&body)
}

/// The value `ListExports` publishes under `EXPORT`, or `None` where this Region publishes no
/// such export.
async fn published_value(router: &MultiServiceRouter, case: &str) -> Option<String> {
    let body = cloudformation_ok(router, case, "ListExports", &[]).await;
    pairs_of(&body, "Name", "Value").remove(EXPORT)
}

/// The stack names `ListImports` reports for `EXPORT` right now. A name nothing imports is
/// REFUSED rather than answered with an empty page, which this reads as the empty set.
async fn importing_stacks(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = cloudformation(router, "ListImports", &[("ExportName", EXPORT)]).await;
    if status != 200 {
        return Vec::new();
    }
    members_of(&body)
        .into_iter()
        .map(ToString::to_string)
        .collect()
}

/// Whether Amazon SNS still holds the topic `arn` names, and reports that very ARN for it.
async fn topic_is_held(router: &MultiServiceRouter, case: &str, arn: &str) -> bool {
    let (status, body) = sns(router, "GetTopicAttributes", &[("TopicArn", arn)]).await;
    if status != 200 {
        return false;
    }
    let attributes = decoded_entries(&body);
    assert_eq!(
        attributes.get("TopicArn").map(String::as_str),
        Some(arn),
        "case {case:?}: expected Amazon SNS to report {arn} for itself, got {attributes:?}"
    );
    true
}

/// Whether Amazon Bedrock Agent still holds the knowledge base `id` names, and the `name` it
/// reports for it.
async fn held_knowledge_base_name(router: &MultiServiceRouter, id: &str) -> Option<String> {
    let (status, knowledge_base) = get_knowledge_base(router, id).await;
    if status != 200 {
        return None;
    }
    Some(
        knowledge_base
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    )
}

/// The `ResourceStatusReason`s of the stack's OWN events: those whose `LogicalResourceId` is
/// the stack itself, which is the stack-level audit trail a client reads to decide what went
/// wrong.
async fn stack_level_reasons(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<String> {
    let body = cloudformation_ok(
        router,
        case,
        "DescribeStackEvents",
        &[("StackName", stack)],
    )
    .await;
    members_of(&body)
        .into_iter()
        .filter(|entry| opt_element(entry, "LogicalResourceId").as_deref() == Some(stack))
        .filter_map(|entry| opt_element(entry, "ResourceStatusReason"))
        .collect()
}

#[tokio::test]
async fn an_update_that_does_not_go_through_takes_nothing_down() {
    for row in ROWS {
        let case = row.case;
        let router = build_router();
        create_and_settle(
            &router,
            case,
            EXPORTER,
            &exporter_template(FIRST_NAME, TITAN_G1, true, ""),
        )
        .await;
        if row.pinned {
            create_and_settle(&router, case, IMPORTER, &importer_template()).await;
            let importers = importing_stacks(&router).await;
            assert_eq!(
                importers,
                vec![IMPORTER.to_string()],
                "case {case:?}: expected the import index to name {IMPORTER}, got {importers:?}"
            );
        }

        // What the stack published before the update, and what the owning services hold for
        // it: the minted knowledge base id, the topic's ARN, and the export resolved from the
        // knowledge base's handle.
        let created = reported_resources(&router, case, EXPORTER).await;
        let knowledge_base_id = created.get(KNOWLEDGE_BASE).cloned().unwrap_or_default();
        assert!(
            is_knowledge_base_id(&knowledge_base_id),
            "case {case:?}: expected the create to publish a minted knowledge base id, got \
             {created:?}"
        );
        let topic_arn = created.get(TOPIC).cloned().unwrap_or_default();
        assert!(
            topic_is_held(&router, case, &topic_arn).await,
            "case {case:?}: expected Amazon SNS to hold the topic {topic_arn:?} the create made"
        );
        let held = held_knowledge_base_name(&router, &knowledge_base_id).await;
        assert_eq!(
            held.as_deref(),
            Some(FIRST_NAME),
            "case {case:?}: expected Amazon Bedrock Agent to hold {knowledge_base_id:?} as \
             {FIRST_NAME:?}, got {held:?}"
        );
        let exported = published_value(&router, case).await;
        assert_eq!(
            exported.as_deref(),
            Some(knowledge_base_id.as_str()),
            "case {case:?}: expected {EXPORT} to publish {knowledge_base_id:?}, got {exported:?}"
        );

        // The revision is ACCEPTED — the model declares no error for an export in use, so what
        // the pin cancels is the stack OPERATION and never the request.
        let revision = exporter_template(row.name, row.model, row.keeps_the_topic, row.suffix);
        cloudformation_ok(
            &router,
            case,
            "UpdateStack",
            &[("StackName", EXPORTER), ("TemplateBody", &revision)],
        )
        .await;
        let described = settled(&router, case, EXPORTER).await;
        let settled_status = opt_element(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            settled_status, row.settles_in,
            "case {case:?}: expected the revision to settle in {:?}, got {described}",
            row.settles_in
        );

        let now_reported = reported_resources(&router, case, EXPORTER).await;
        let published_id = now_reported.get(KNOWLEDGE_BASE).cloned().unwrap_or_default();
        assert!(
            is_knowledge_base_id(&published_id),
            "case {case:?}: expected the stack to publish a minted knowledge base id, got \
             {now_reported:?}"
        );
        assert_eq!(
            published_id == knowledge_base_id,
            row.keeps_its_handle,
            "case {case:?}: expected the published handle to {} across the revision, got \
             {knowledge_base_id:?} then {published_id:?}",
            if row.keeps_its_handle {
                "stay"
            } else {
                "move"
            }
        );
        if !row.keeps_its_handle {
            // The other side of the rule: the update went through, so the knowledge base it
            // replaced really was taken down.
            let superseded = held_knowledge_base_name(&router, &knowledge_base_id).await;
            assert_eq!(
                superseded, None,
                "case {case:?}: expected the knowledge base {knowledge_base_id:?} the completed \
                 update replaced to be taken down, Amazon Bedrock Agent still reports \
                 {superseded:?}"
            );
        }

        // NOTHING a cancelled update would have taken down is gone, and the knowledge base the
        // stack publishes is the one the owning service holds — under the name the revision
        // asked for where the revision landed, and under the name in force where it did not.
        let held_now = held_knowledge_base_name(&router, &published_id).await;
        assert_eq!(
            held_now.as_deref(),
            Some(row.reports_name),
            "case {case:?}: expected Amazon Bedrock Agent to hold {published_id:?} as {:?}, got \
             {held_now:?}",
            row.reports_name
        );
        assert!(
            topic_is_held(&router, case, &topic_arn).await,
            "case {case:?}: expected Amazon SNS to still hold the topic {topic_arn:?}"
        );

        // ...and every value the stack publishes for the knowledge base is resolved from the
        // handle in force.
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get(EXPORTED_OUTPUT).map(String::as_str),
            Some(published_id.as_str()),
            "case {case:?}: expected the output to report {published_id:?}, got {outputs:?}"
        );
        let still_published = published_value(&router, case).await;
        assert_eq!(
            still_published.as_deref(),
            Some(published_id.as_str()),
            "case {case:?}: expected {EXPORT} to publish {published_id:?}, got \
             {still_published:?}"
        );
        if row.pinned {
            let importers = importing_stacks(&router).await;
            assert_eq!(
                importers,
                vec![IMPORTER.to_string()],
                "case {case:?}: expected {IMPORTER} to still import {EXPORT}, got {importers:?}"
            );
        }

        // ...and a cancellation is explained ONCE among the stack's own events: one failure of
        // one stack operation is one cancellation, however many transitions observed it —
        // while an update that was carried through reports none at all.
        let reason = update_canceled_reason(EXPORT, IMPORTER);
        let reasons = stack_level_reasons(&router, case, EXPORTER).await;
        let carrying = reasons
            .iter()
            .filter(|recorded| recorded.contains(&reason))
            .count();
        let expected_cancellations = usize::from(row.settles_in == CANCELLED);
        assert_eq!(
            carrying, expected_cancellations,
            "case {case:?}: expected exactly {expected_cancellations} stack-level event(s) to \
             carry {reason:?}, got {reasons:?}"
        );
    }
}
