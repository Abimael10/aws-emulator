// SPDX-License-Identifier: CC0-1.0

//! `StartAutomatedReasoningPolicyBuildWorkflow` weighs the `sourceContent` a start carries
//! against the `buildWorkflowType` it names, over the same `MultiServiceRouter` the emulator
//! boots.
//!
//! The rule under test, asserted over a SET of inputs rather than over one recorded request:
//! a build workflow is started from the content its own KIND of workflow is built from. Four
//! forms of `AutomatedReasoningPolicyWorkflowTypeContent` each name the kind they belong to —
//! `documents` is "the list of documents to be processed in a document ingestion workflow",
//! `policyRepairAssets` "the assets and instructions needed for a policy repair workflow",
//! `generateFidelityReportContent` "the content configuration for generating a fidelity
//! report workflow" and `iterativeRefinementContent` "content configuration to start an
//! iterative policy refinement workflow"
//! (`smithy_query(query="shape",shape="AutomatedReasoningPolicyWorkflowTypeContent")`) — so a
//! build of one of those kinds carries exactly its own form, and a start carrying another
//! kind's form, or none at all, is refused with the modelled `ValidationException`
//! (`smithy_query(query="operation",operation="StartAutomatedReasoningPolicyBuildWorkflow")`:
//! `ValidationException` httpError 400, success code 200) and starts NOTHING: the policy
//! lists no workflow and is still deletable without `force`.
//!
//! The two members `sourceContent` itself declares are weighed differently:
//! `policyDefinition` is "an existing policy definition that serves as the starting point for
//! the build workflow"
//! (`smithy_query(query="shape",shape="AutomatedReasoningPolicyBuildWorkflowSource")`), which
//! any kind of build may start from, so it neither satisfies nor violates the rule; and the
//! three workflow types the model names no content form for are built from the policy itself,
//! so no form is wrong for them.
//!
//! The requests go through `crate::support` — the ONE harness every suite in this crate
//! drives the emulator with.

use serde_json::{json, Value};

use crate::support::{emulator, label, Emulator, Reply};

/// The sentence Amazon Bedrock refuses a start whose source content is not the content its
/// workflow type is built from with — wholly fixed text.
const INVALID_CONTENT: &str = "Invalid content for workflow type.";

/// A source document's bytes on the wire: `AutomatedReasoningPolicyBuildDocumentBlob` is a
/// `blob`, which this protocol carries base64-encoded — this is `policy rules`.
const DOCUMENT_BLOB: &str = "cG9saWN5IHJ1bGVz";

/// Every `AutomatedReasoningPolicyBuildWorkflowType` the enum admits
/// (`smithy_query(query="shape",shape="AutomatedReasoningPolicyBuildWorkflowType")`).
const WORKFLOW_TYPES: [&str; 7] = [
    "INGEST_CONTENT",
    "REFINE_POLICY",
    "IMPORT_POLICY",
    "GENERATE_FIDELITY_REPORT",
    "GENERATE_POLICY_SCENARIOS",
    "RESOLVE_POLICY_AMBIGUITIES",
    "ITERATIVELY_REFINE_POLICY",
];

/// The `(workflow type, the `workflowContent` form that kind of workflow is built from)` pairs
/// the member documentation this suite's module doc quotes names, so each row's expectation
/// comes from the model rather than from one recorded request.
const CONTENT_OF_KIND: [(&str, &str); 4] = [
    ("INGEST_CONTENT", "documents"),
    ("REFINE_POLICY", "policyRepairAssets"),
    ("GENERATE_FIDELITY_REPORT", "generateFidelityReportContent"),
    ("ITERATIVELY_REFINE_POLICY", "iterativeRefinementContent"),
];

/// One source document, as `AutomatedReasoningPolicyBuildWorkflowDocument` spells it.
fn document(name: &str) -> Value {
    json!({
        "document": DOCUMENT_BLOB,
        "documentContentType": "txt",
        "documentName": name,
    })
}

/// A `sourceContent` carrying the `workflowContent` form `form` names, each filled as its own
/// shape requires.
fn source_with(form: &str) -> Value {
    let content = match form {
        "documents" => json!({ "documents": [document("returns.txt")] }),
        "policyRepairAssets" => json!({
            "policyRepairAssets": {
                "annotations": [{ "addRule": { "expression": "returnWindow == Open" } }],
            },
        }),
        "generateFidelityReportContent" => json!({
            "generateFidelityReportContent": { "documents": [document("fidelity.txt")] },
        }),
        "iterativeRefinementContent" => json!({
            "iterativeRefinementContent": {
                "documents": [document("refine.txt")],
                "feedback": "tighten the rules",
            },
        }),
        other => panic!("no such workflowContent form: {other:?}"),
    };
    json!({ "workflowContent": content })
}

/// A `sourceContent` carrying only the policy definition a build may start from.
fn definition_source() -> Value {
    json!({
        "policyDefinition": {
            "version": "1",
            "types": [],
            "variables": [],
            "rules": [{ "id": "R00000000001", "expression": "returnWindow == Open" }],
        },
    })
}

/// Create a policy named `name` and hand back the `policyArn` it minted.
async fn policy(emu: &Emulator, name: &str) -> String {
    let created = emu
        .call(
            "POST",
            "/automated-reasoning-policies",
            Some(json!({ "name": name })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {name:?}: the create must answer 200, got {} {:?}",
        created.status, created.body
    );
    created.text("policyArn")
}

/// One `POST /automated-reasoning-policies/{policyArn}/build-workflows/{type}/start`.
async fn start(emu: &Emulator, policy_arn: &str, workflow_type: &str, source: Value) -> Reply {
    emu.call(
        "POST",
        &format!(
            "/automated-reasoning-policies/{}/build-workflows/{workflow_type}/start",
            label(policy_arn)
        ),
        Some(source),
    )
    .await
}

/// Every `buildWorkflowId` `ListAutomatedReasoningPolicyBuildWorkflows` reports for a policy.
async fn listed_workflow_ids(emu: &Emulator, case: &str, policy_arn: &str) -> Vec<String> {
    let listed = emu
        .call(
            "GET",
            &format!(
                "/automated-reasoning-policies/{}/build-workflows",
                label(policy_arn)
            ),
            None,
        )
        .await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: the listing must answer 200, got {} {:?}",
        listed.status, listed.body
    );
    listed
        .get("automatedReasoningPolicyBuildWorkflowSummaries")
        .as_array()
        .map(|summaries| {
            summaries
                .iter()
                .map(|summary| {
                    summary
                        .get("buildWorkflowId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Assert `reply` is the refusal a start carrying the wrong content answers with.
fn assert_invalid_content(case: &str, reply: &Reply) {
    assert_eq!(
        (reply.status, reply.error.as_deref()),
        (400, Some("ValidationException")),
        "case {case:?}: expected 400 ValidationException, got {} {:?}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.get("message"),
        json!(INVALID_CONTENT),
        "case {case:?}: expected the message {INVALID_CONTENT:?}, got {:?}",
        reply.body
    );
}

#[tokio::test]
async fn a_build_workflow_is_started_only_from_the_content_its_kind_is_built_from() {
    let emu = emulator();

    // One row per (kind of workflow, form of content) pair: the form that kind is built from
    // is accepted, and every OTHER form the union admits is refused — so the rule is
    // asserted over the whole cross product rather than over one pairing.
    for (index, (workflow_type, own_form)) in CONTENT_OF_KIND.iter().enumerate() {
        for (_, other_form) in &CONTENT_OF_KIND {
            if other_form == own_form {
                continue;
            }
            let case = format!("{workflow_type} started from {other_form}");
            let policy_arn = policy(&emu, &format!("mismatched source {index} {other_form}")).await;
            assert_invalid_content(
                &case,
                &start(&emu, &policy_arn, workflow_type, source_with(other_form)).await,
            );
            // The refusal started nothing, so the policy holds no workflow at all and the
            // unforced delete of it is not refused for holding one.
            assert_eq!(
                listed_workflow_ids(&emu, &case, &policy_arn).await,
                Vec::<String>::new(),
                "case {case:?}: a refused start must leave the policy holding no workflow"
            );
            let deleted = emu
                .call(
                    "DELETE",
                    &format!("/automated-reasoning-policies/{}", label(&policy_arn)),
                    None,
                )
                .await;
            assert_eq!(
                deleted.status, 202,
                "case {case:?}: a policy a refused start left empty must delete unforced, got \
                 {} {:?}",
                deleted.status, deleted.body
            );
        }

        // Its own form is accepted, and the workflow it started is the one the listing
        // reports.
        let case = format!("{workflow_type} started from {own_form}");
        let policy_arn = policy(&emu, &format!("matched source {index}")).await;
        let started = start(&emu, &policy_arn, workflow_type, source_with(own_form)).await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: the start must answer 200, got {} {:?}",
            started.status, started.body
        );
        let id = started.text("buildWorkflowId");
        assert_eq!(
            listed_workflow_ids(&emu, &case, &policy_arn).await,
            vec![id],
            "case {case:?}: the listing must report the workflow the start minted"
        );
    }
}

#[tokio::test]
async fn a_start_carrying_no_content_of_its_kind_is_refused() {
    let emu = emulator();

    // A kind of workflow that is built from one form of content has nothing to run on when
    // the source carries none: neither an empty source nor one carrying only the policy
    // definition a build may start from satisfies it.
    for (index, (workflow_type, own_form)) in CONTENT_OF_KIND.iter().enumerate() {
        for (form_case, source) in [
            ("no sourceContent members at all", json!({})),
            ("only the policy definition", definition_source()),
        ] {
            let case = format!("{workflow_type} with {form_case}");
            let policy_arn = policy(&emu, &format!("absent source {index} {form_case}")).await;
            assert_invalid_content(
                &case,
                &start(&emu, &policy_arn, workflow_type, source).await,
            );
            assert_eq!(
                listed_workflow_ids(&emu, &case, &policy_arn).await,
                Vec::<String>::new(),
                "case {case:?}: a refused start must leave the policy holding no workflow"
            );
        }

        // The same type, carrying its own form beside that policy definition, is accepted:
        // the definition is not what the rule weighs.
        let case = format!("{workflow_type} with its own content and a policy definition");
        let policy_arn = policy(&emu, &format!("definition beside source {index}")).await;
        let mut source = source_with(own_form);
        source["policyDefinition"] = definition_source()["policyDefinition"].clone();
        let started = start(&emu, &policy_arn, workflow_type, source).await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: the start must answer 200, got {} {:?}",
            started.status, started.body
        );
    }
}

#[tokio::test]
async fn a_workflow_type_the_model_names_no_content_for_is_started_from_any_source() {
    let emu = emulator();

    // The three types no member of the union names: the model documents no content form for
    // them, so a start of one is accepted whatever source it carries — including the forms
    // the four kinds above are built from, an empty source, and a policy definition.
    let unnamed: Vec<&str> = WORKFLOW_TYPES
        .iter()
        .copied()
        .filter(|workflow_type| {
            !CONTENT_OF_KIND
                .iter()
                .any(|(named, _)| named == workflow_type)
        })
        .collect();
    assert_eq!(
        unnamed,
        vec![
            "IMPORT_POLICY",
            "GENERATE_POLICY_SCENARIOS",
            "RESOLVE_POLICY_AMBIGUITIES"
        ],
        "the enum must still carry exactly the three types the union names no content for, \
         got {unnamed:?}"
    );

    let mut index = 0;
    for workflow_type in unnamed {
        let sources: Vec<(String, Value)> = CONTENT_OF_KIND
            .iter()
            .map(|(_, form)| ((*form).to_string(), source_with(form)))
            .chain([
                ("an empty source".to_string(), json!({})),
                ("a policy definition".to_string(), definition_source()),
            ])
            .collect();
        for (form_case, source) in sources {
            let case = format!("{workflow_type} started from {form_case}");
            // One policy per row: a policy a workflow is still building admits no second
            // start, so each row's workflow is a workflow of its own policy.
            let policy_arn = policy(&emu, &format!("unnamed source {index}")).await;
            index += 1;
            let started = start(&emu, &policy_arn, workflow_type, source).await;
            assert_eq!(
                started.status, 200,
                "case {case:?}: the start must answer 200, got {} {:?}",
                started.status, started.body
            );
            assert_eq!(
                listed_workflow_ids(&emu, &case, &policy_arn).await,
                vec![started.text("buildWorkflowId")],
                "case {case:?}: the listing must report the workflow the start minted"
            );
        }
    }
}
