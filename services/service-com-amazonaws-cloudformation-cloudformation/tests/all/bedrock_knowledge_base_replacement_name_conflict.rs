// SPDX-License-Identifier: CC0-1.0

//! A replacing update whose create asks the owning service for a NAME another resource of
//! that service already holds, end to end over the awsQuery wire.
//!
//! RULE: CloudFormation carries a change to a create-only value out by REPLACING the
//! resource, and the replacement is created BEFORE the resource it replaces is deleted — so
//! the create reaches the owning service while every resource that service already holds is
//! still there. A name that service requires to be unique in the account is therefore still
//! taken, whichever resource holds it: the create is refused, CloudFormation reports the
//! refusal on the LOGICAL resource as `UPDATE_FAILED` in the `AlreadyExists` class a client
//! branches on, the stack rolls back, and every resource keeps the identity and the
//! configuration it had.
//!
//! An `AWS::Bedrock::KnowledgeBase` is such a resource: `CreateKnowledgeBase` refuses a name
//! the account's knowledge bases already hold, while the embedding model inside
//! `KnowledgeBaseConfiguration` is the create-only value that forces the replacement (the
//! vectors already in a knowledge base were produced by that model), and the `Name` a
//! declaration states is only what the knowledge base is called.
//!
//! The rows differ along WHICH knowledge base holds the name the replacement asks for — one
//! created outside the stack, through the owning service's own API, and a sibling resource of
//! the same stack — so what fails the replacement is the name being taken in the ACCOUNT
//! rather than anything about the resource being replaced.
//! `bedrock_knowledge_base_drift_and_replacement.rs` covers the third holder, the knowledge
//! base the replacement replaces itself.
//!
//! Everything goes through the one router `stack_lifecycle.rs` builds, over each service's own
//! wire — CloudFormation's awsQuery, Amazon Bedrock Agent's `restJson1`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::{early_validation, status};

use crate::bedrock_knowledge_base_drift_and_replacement::reported_knowledge_base;
use crate::bedrock_knowledge_base_identity::{
    bedrock_agent, described_physical_id, is_knowledge_base_id, model_arn, opensearch_storage,
    TITAN_G1, TITAN_V2,
};
use crate::harness::{call, create_stack, member, router, settled_stack_status, REGION};
use crate::stack_lifecycle::{resource_event_reason, resource_event_statuses, ACCOUNT_ID};

/// The resource type whose replacement these rows drive.
const KNOWLEDGE_BASE_TYPE: &str = "AWS::Bedrock::KnowledgeBase";

/// The `HandlerErrorCode` class a resource handler puts a name already in use in — the one a
/// client branches on to rename or stage the change.
const ALREADY_EXISTS: &str = "AlreadyExists";

/// One row: which knowledge base holds the name the replacement asks for.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// Whether the holder of the name is created outside the stack, through the owning
    /// service's own API, rather than declared beside the replaced resource.
    holder_outside_the_stack: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the name is held by a knowledge base created outside the stack",
            stack: "kbheld-outside",
            holder_outside_the_stack: true,
        },
        Row {
            case: "the name is held by a sibling resource of the same stack",
            stack: "kbheld-sibling",
            holder_outside_the_stack: false,
        },
    ]
}

/// The name the holder of each row carries, and the name the replacement therefore asks for.
fn held_name(row: &Row) -> String {
    format!("{}-holder", row.stack)
}

/// The name the stack's own knowledge base is deployed under.
fn deployed_name(row: &Row) -> String {
    format!("{}-escalation", row.stack)
}

/// One `AWS::Bedrock::KnowledgeBase` declaration: the name it is called, and the embedding
/// model whose change is the create-only one.
fn declaration(stack: &str, logical_id: &str, name: &str, model: &str) -> Value {
    json!({
        "Type": KNOWLEDGE_BASE_TYPE,
        "Properties": {
            "Name": name,
            "RoleArn": format!("arn:aws:iam::{ACCOUNT_ID}:role/{stack}"),
            "KnowledgeBaseConfiguration": {
                "Type": "VECTOR",
                "VectorKnowledgeBaseConfiguration": { "EmbeddingModelArn": model_arn(model) }
            },
            "StorageConfiguration": opensearch_storage(
                stack,
                &format!("{}-index", logical_id.to_lowercase()),
            )
        }
    })
}

/// The template one revision of a row deploys: the knowledge base the update replaces, and —
/// where the holder is declared rather than created outside — the sibling that holds the name.
fn template(row: &Row, escalation_name: &str, escalation_model: &str) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Escalation".to_string(),
        declaration(row.stack, "Escalation", escalation_name, escalation_model),
    );
    if !row.holder_outside_the_stack {
        resources.insert(
            "Runbooks".to_string(),
            declaration(row.stack, "Runbooks", &held_name(row), TITAN_G1),
        );
    }
    json!({ "Resources": Value::Object(resources) }).to_string()
}

/// Create one knowledge base through the owning service's own `CreateKnowledgeBase`
/// (`PUT /knowledgebases/`, the trailing slash its `@http` binding spells), and report the
/// `knowledgeBaseId` it minted.
async fn create_knowledge_base(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let request = json!({
        "name": name,
        "roleArn": format!("arn:aws:iam::{ACCOUNT_ID}:role/{name}"),
        "knowledgeBaseConfiguration": {
            "type": "VECTOR",
            "vectorKnowledgeBaseConfiguration": { "embeddingModelArn": model_arn(TITAN_G1) }
        },
        "storageConfiguration": {
            "type": "OPENSEARCH_SERVERLESS",
            "opensearchServerlessConfiguration": {
                "collectionArn":
                    format!("arn:aws:aoss:{REGION}:{ACCOUNT_ID}:collection/{name}"),
                "vectorIndexName": "holder-index",
                "fieldMapping": {
                    "vectorField": "bedrock-knowledge-base-default-vector",
                    "textField": "AMAZON_BEDROCK_TEXT_CHUNK",
                    "metadataField": "AMAZON_BEDROCK_METADATA"
                }
            }
        }
    });
    let (code, answer) =
        bedrock_agent(router, "PUT", "/knowledgebases/", Some(&request)).await;
    assert_eq!(
        code, 202,
        "case {case:?}: expected CreateKnowledgeBase({name}) to be accepted, got {code} with \
         {answer}"
    );
    answer
        .pointer("/knowledgeBase/knowledgeBaseId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a minted knowledgeBaseId, got {answer}")
        })
        .to_string()
}

/// Every row: the replacement's create is refused because the name is held, the logical
/// resource fails in the `AlreadyExists` class, the stack rolls back, and both knowledge bases
/// keep what they had.
#[tokio::test]
async fn a_replacement_asking_for_a_held_name_fails_and_the_stack_rolls_back() {
    for row in rows() {
        let case = row.case;
        let router = router();
        let held = held_name(&row);

        // The holder of the name: created outside the stack through the owning service's own
        // API, or declared beside the resource the update replaces.
        let outside_holder = if row.holder_outside_the_stack {
            Some(create_knowledge_base(&router, case, &held).await)
        } else {
            None
        };

        // The revision the update starts from: the knowledge base under its own name, and —
        // where the holder is declared — the sibling that holds the colliding one. The create
        // is waited out by POLLING for a terminal status, so a loaded machine that takes
        // longer over the owning service's side of the walk is not a failure.
        let deployed = deployed_name(&row);
        let (code, body) = create_stack(
            &router,
            row.stack,
            &template(&row, &deployed, TITAN_G1),
            &[],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack({}) answered {code} with {body}",
            row.stack
        );
        let settled = settled_stack_status(case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::CREATE_COMPLETE,
            "case {case:?}: expected the create to complete, got {settled:?}"
        );
        let created = described_physical_id(&router, case, row.stack, "Escalation").await;
        assert!(
            is_knowledge_base_id(&created),
            "case {case:?}: expected Escalation to report a minted id, got {created:?}"
        );
        let sibling = match outside_holder {
            Some(ref id) => id.clone(),
            None => described_physical_id(&router, case, row.stack, "Runbooks").await,
        };

        // The update re-points the knowledge base at another embedding model — the create-only
        // value — and asks for the name the holder still has, so the replacement's create
        // collides.
        let after = template(&row, &held, TITAN_V2);
        let (code, body) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", row.stack),
                member("TemplateBody", &after),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: UpdateStack({}) answered {code}: {body}",
            row.stack
        );

        let settled = settled_stack_status(case, &router, REGION, row.stack).await;
        assert_eq!(
            settled,
            status::UPDATE_ROLLBACK_COMPLETE,
            "case {case:?}: expected the update to roll back, got {settled:?}"
        );

        // ONE failure event on the logical resource, reported as an UPDATE of it, carrying the
        // class a client branches on and the sentence naming the identifier it asked for.
        let statuses = resource_event_statuses(&router, case, row.stack, "Escalation").await;
        let failures = statuses
            .iter()
            .filter(|reported| *reported == "UPDATE_FAILED")
            .count();
        assert_eq!(
            failures, 1,
            "case {case:?}: expected one UPDATE_FAILED event for Escalation, got {statuses:?}"
        );
        let reason = resource_event_reason(&router, row.stack, "Escalation", "UPDATE_FAILED").await;
        assert!(
            reason.contains(&format!("HandlerErrorCode: {ALREADY_EXISTS}")),
            "case {case:?}: expected the failure to carry HandlerErrorCode: {ALREADY_EXISTS}, \
             got {reason:?}"
        );
        let sentence = early_validation::already_exists(KNOWLEDGE_BASE_TYPE, &held);
        assert!(
            reason.contains(&sentence),
            "case {case:?}: expected the failure to report {sentence:?}, got {reason:?}"
        );

        // Nothing moved: the stack's knowledge base keeps the id its create minted, its name
        // and the model it was created with, and so does the holder of the name.
        let now = described_physical_id(&router, case, row.stack, "Escalation").await;
        assert_eq!(
            now, created,
            "case {case:?}: expected Escalation to keep the id its create minted, got {now:?}"
        );
        let (name, model) = reported_knowledge_base(&router, case, &created).await;
        assert_eq!(
            name, deployed,
            "case {case:?}: expected Escalation to keep the name it was deployed under, got \
             {name:?}"
        );
        assert_eq!(
            model,
            model_arn(TITAN_G1),
            "case {case:?}: expected Escalation to keep the embedding model it was created \
             with, got {model:?}"
        );
        let (name, model) = reported_knowledge_base(&router, case, &sibling).await;
        assert_eq!(
            name, held,
            "case {case:?}: expected the holder to keep the name it holds, got {name:?}"
        );
        assert_eq!(
            model,
            model_arn(TITAN_G1),
            "case {case:?}: expected the holder to keep its embedding model, got {model:?}"
        );
    }
}
