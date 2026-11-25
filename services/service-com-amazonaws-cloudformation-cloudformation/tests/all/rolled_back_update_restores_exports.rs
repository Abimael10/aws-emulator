// SPDX-License-Identifier: CC0-1.0

//! What a CANCELLED, rolled-back update leaves the stack PUBLISHING, end to end over the
//! awsQuery wire.
//!
//! RULE under test: **a stack update that is rolled back is absent in its effect — the stack
//! goes back to the previous configuration and to the previous physical resources, so every
//! value it reports afterwards (`Stack$Outputs`, the physical ids of its resources and the
//! exported output values `ListExports` reports) is the one it reported before the update was
//! submitted, even where the abandoned forward pass had already created the replacement
//! resources those values would have been resolved over.**
//!
//! Why it matters: a client treats a rolled-back update as a no-op and goes on consuming the
//! exporting stack through `Fn::ImportValue` and `ListExports`. An export left naming a
//! resource the rollback abandoned wires every later consumer to a dangling identifier while
//! the stack's surviving resources are unreachable through the export — the corruption
//! cross-stack export protection exists to prevent.
//!
//! The rows differ along what the rule generalises over: how the output that would move is
//! resolved (`Ref`, `Fn::GetAtt`), whether the replacement's identifier is the NAME
//! CloudFormation decides for the resource or one its owning SERVICE mints, how the revision
//! is submitted (a new `TemplateBody`, or the same template with other `Parameters`), and how
//! many pinned exports the stack publishes at once (one, and a pair of them).
//!
//! `canceled_export_update_events.rs` pins what the cancelled deployment DID (its statuses,
//! its events and the resource it put back) over exports whose values are literals; this
//! suite pins the values the stack is left publishing when those exports are resolved over
//! resources the forward pass replaced.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::bedrock_knowledge_base_identity::{
    get_knowledge_base, knowledge_base_resource, model_arn, opensearch_storage, output_value,
    ACTIVE, TITAN_G1, TITAN_V2,
};
use crate::harness::{physical_resource_id, settled_stack_status, stack_own_events};
use crate::stack_lifecycle::{
    call, create_stack, member, resource_event_statuses, router, xml_blocks, xml_text, REGION,
};

/// The stacks of the pair: the one that publishes the exports, and the one that imports them.
const EXPORTER: &str = "rollback-restored-exporter";
const IMPORTER: &str = "rollback-restored-importer";

/// The `StackStatusReason` CloudFormation records on a deployment it cancelled because the
/// deployment would have withdrawn an exported output value another stack still imports.
///
/// Spelled out here rather than read off the emulator's own formatter: fixed text around the
/// export name and the importing stack's name, ending with its period.
fn update_canceled_reason(export_name: &str, importing_stack_name: &str) -> String {
    format!(
        "Update canceled. Cannot update export {export_name} as it is in use by \
         {importing_stack_name}."
    )
}

/// One exported output of the row: the output key, the export name it publishes under, and
/// the expression the value is resolved from.
struct Exported {
    key: &'static str,
    export: &'static str,
    value: Value,
}

/// How the row submits the revision: both are deployments of the same stack, and a rollback
/// is absent in its effect either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Revision {
    /// `UpdateStack` with the revised `TemplateBody`.
    Template,
    /// `UpdateStack` with the SAME template and other `Parameters`, which is what makes the
    /// declarations the stack deploys change.
    Parameters,
}

/// One row: the resources the exported outputs are resolved over, before and after.
struct Row {
    case: &'static str,
    /// The `Generation` the created revision declares, and the one the update submits: the
    /// row's declarations are a function of it, so one generation's resources are a
    /// replacement of the other's.
    before: &'static str,
    after: &'static str,
    revision: Revision,
    /// The logical ids the revision replaces, whose restored physical ids are asserted.
    replaced: &'static [&'static str],
    exports: Vec<Exported>,
    /// Whether the restored export values are knowledge-base identifiers the owning service
    /// must still report ACTIVE, and the embedding model each must carry — the corroboration
    /// that the restored export names a SURVIVING resource and not a dangling id.
    knowledge_bases: bool,
}

/// The embedding model one generation declares. Source: the catalog the owning service
/// resolves an `embeddingModelArn` against, as `bedrock_knowledge_base_identity.rs` names it.
fn model_of(generation: &str) -> &'static str {
    if generation == "g1" {
        TITAN_G1
    } else {
        TITAN_V2
    }
}

/// The resources one generation of the knowledge-base estate declares: `count` knowledge
/// bases, each named and each bound to a vector index of that generation, so a change of
/// generation replaces every one of them.
fn knowledge_bases(generation: &str, count: usize) -> Value {
    let mut resources = serde_json::Map::new();
    for index in 0..count {
        let logical_id = format!("Knowledge{index}");
        resources.insert(
            logical_id.clone(),
            knowledge_base_resource(
                Some(&format!("{EXPORTER}-{generation}-{index}")),
                None,
                EXPORTER,
                model_of(generation),
                opensearch_storage(EXPORTER, &format!("{generation}-{index}-index")),
            ),
        );
    }
    Value::Object(resources)
}

/// The template of the row that submits its revision as PARAMETERS: the generation and the
/// embedding model are parameters, and each knowledge base's name, vector index and embedding
/// model are composed from them — so stating other values makes every knowledge base a
/// replacement of the one in force, with the document itself unchanged.
fn parameterised_template(count: usize, exports: &[Exported]) -> String {
    let mut resources = serde_json::Map::new();
    for index in 0..count {
        let mut declaration = knowledge_base_resource(
            Some(&format!("{EXPORTER}-{index}")),
            None,
            EXPORTER,
            TITAN_G1,
            opensearch_storage(EXPORTER, &format!("{index}-index")),
        );
        let properties = &mut declaration["Properties"];
        properties["Name"] = json!({"Fn::Sub": format!("{EXPORTER}-${{Generation}}-{index}")});
        properties["StorageConfiguration"]["OpensearchServerlessConfiguration"]
            ["VectorIndexName"] = json!({"Fn::Sub": format!("${{Generation}}-{index}-index")});
        properties["KnowledgeBaseConfiguration"]["VectorKnowledgeBaseConfiguration"]
            ["EmbeddingModelArn"] = json!({
            "Fn::Sub": "arn:aws:bedrock:${AWS::Region}::foundation-model/${EmbeddingModelId}"
        });
        resources.insert(format!("Knowledge{index}"), declaration);
    }
    json!({
        "Parameters": {
            "Generation": {"Type": "String", "Default": "g1"},
            "EmbeddingModelId": {"Type": "String", "Default": TITAN_G1},
        },
        "Resources": Value::Object(resources),
        "Outputs": outputs(exports),
    })
    .to_string()
}

/// The `Outputs` section of a row: every exported output it publishes.
fn outputs(exports: &[Exported]) -> Value {
    let mut outputs = serde_json::Map::new();
    for exported in exports {
        outputs.insert(
            exported.key.to_string(),
            json!({"Value": exported.value, "Export": {"Name": exported.export}}),
        );
    }
    Value::Object(outputs)
}

/// The template of the row whose replaced resource's identifier is the NAME CloudFormation
/// decides for it: renaming an `AWS::SSM::Parameter` replaces it, and `Ref` resolves to that
/// name.
fn parameter_template(generation: &str, exports: &[Exported]) -> String {
    json!({
        "Resources": {"Held": {
            "Type": "AWS::SSM::Parameter",
            "Properties": {
                "Name": format!("/rollback-restored/{generation}"),
                "Type": "String",
                "Value": "held",
            },
        }},
        "Outputs": outputs(exports),
    })
    .to_string()
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the export is a Ref to a resource whose NAME CloudFormation decides",
            before: "g1",
            after: "g2",
            revision: Revision::Template,
            replaced: &["Held"],
            exports: vec![Exported {
                key: "HeldName",
                export: "rollback-restored-parameter-name",
                value: json!({"Ref": "Held"}),
            }],
            knowledge_bases: false,
        },
        Row {
            case: "the export is a Ref to a resource whose id its owning service MINTS",
            before: "g1",
            after: "g2",
            revision: Revision::Template,
            replaced: &["Knowledge0"],
            exports: vec![Exported {
                key: "Knowledge0Id",
                export: "rollback-restored-knowledge-base-id",
                value: json!({"Ref": "Knowledge0"}),
            }],
            knowledge_bases: true,
        },
        Row {
            case: "a PAIR of pinned exports over two replaced resources",
            before: "g1",
            after: "g2",
            revision: Revision::Template,
            replaced: &["Knowledge0", "Knowledge1"],
            exports: vec![
                Exported {
                    key: "Knowledge0Id",
                    export: "rollback-restored-first-knowledge-base",
                    value: json!({"Ref": "Knowledge0"}),
                },
                Exported {
                    key: "Knowledge1Id",
                    export: "rollback-restored-second-knowledge-base",
                    value: json!({"Ref": "Knowledge1"}),
                },
            ],
            knowledge_bases: true,
        },
        Row {
            case: "the revision is submitted as other Parameters of the same template",
            before: "g1",
            after: "g2",
            revision: Revision::Parameters,
            replaced: &["Knowledge0"],
            exports: vec![Exported {
                key: "Knowledge0Id",
                export: "rollback-restored-parameterised-knowledge-base",
                value: json!({"Ref": "Knowledge0"}),
            }],
            knowledge_bases: true,
        },
        Row {
            case: "the export is an Fn::GetAtt of a resource whose id its owning service MINTS",
            before: "g1",
            after: "g2",
            revision: Revision::Template,
            replaced: &["Knowledge0"],
            exports: vec![Exported {
                key: "Knowledge0Arn",
                export: "rollback-restored-knowledge-base-arn",
                value: json!({"Fn::GetAtt": ["Knowledge0", "KnowledgeBaseArn"]}),
            }],
            knowledge_bases: true,
        },
    ]
}

impl Row {
    /// The template body this row deploys for `generation`.
    fn template(&self, generation: &str) -> String {
        match (self.revision, self.knowledge_bases) {
            (Revision::Parameters, _) => {
                parameterised_template(self.replaced.len(), &self.exports)
            }
            (_, true) => json!({
                "Resources": knowledge_bases(generation, self.replaced.len()),
                "Outputs": outputs(&self.exports),
            })
            .to_string(),
            (_, false) => parameter_template(generation, &self.exports),
        }
    }
}

/// The importing stack's template: it resolves every export of the row into an output of its
/// own, which is what pins those exports and so cancels the update that would move them.
fn importer_template(row: &Row) -> String {
    let mut outputs = serde_json::Map::new();
    for (index, exported) in row.exports.iter().enumerate() {
        outputs.insert(
            format!("Echoed{index}"),
            json!({"Value": {"Fn::ImportValue": exported.export}}),
        );
    }
    json!({
        "Resources": {"Handle": {"Type": "AWS::CloudFormation::WaitConditionHandle"}},
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// The value the stack reports for one of its `Outputs`, which it must report at all: the
/// shared reader of this target ([`crate::bedrock_knowledge_base_identity::output_value`]).
async fn reported_output(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
    key: &str,
) -> String {
    output_value(router, case, stack, key)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected {stack} to report the output {key}"))
}

/// What `ListExports` publishes under `export`, paging the whole index the way a client does.
async fn published_value(
    case: &str,
    router: &MultiServiceRouter,
    export: &str,
) -> Option<String> {
    let mut token: Option<String> = None;
    loop {
        let mut members = Vec::new();
        if let Some(cursor) = &token {
            members.push(member("NextToken", cursor));
        }
        let (code, body) = call(router, "ListExports", &members).await;
        assert_eq!(
            code, 200,
            "case {case:?}: ListExports must answer 200, got {code} {body}"
        );
        for entry in xml_blocks(&body, "member") {
            if xml_text(&entry, "Name").as_deref() == Some(export) {
                return xml_text(&entry, "Value");
            }
        }
        // No cursor: the whole index has been read and it carries no such export.
        let next = xml_text(&body, "NextToken")?;
        token = Some(next);
    }
}

/// The knowledge base the restored export names, read in the service that owns it: its
/// status and its embedding model.
///
/// `value` is the exported value, which is either the identifier itself or the ARN it is the
/// last segment of.
async fn reported_knowledge_base(
    case: &str,
    router: &MultiServiceRouter,
    value: &str,
) -> (String, String) {
    let id = value.rsplit('/').next().unwrap_or(value);
    let (code, knowledge_base) = get_knowledge_base(router, id).await;
    assert_eq!(
        code, 200,
        "case {case:?}: GetKnowledgeBase({id}) must answer 200, got {code} {knowledge_base}"
    );
    (
        knowledge_base["status"].as_str().unwrap_or_default().to_string(),
        knowledge_base["knowledgeBaseConfiguration"]["vectorKnowledgeBaseConfiguration"]
            ["embeddingModelArn"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

#[tokio::test]
async fn a_cancelled_update_leaves_the_stacks_exports_as_they_were() {
    for row in rows() {
        let case = row.case;
        let router = router();

        // The exporting stack publishes the values, and the importer resolves them: that is
        // the pin the update below runs into.
        let created = row.template(row.before);
        let (code, body) = create_stack(&router, EXPORTER, &created, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack({EXPORTER}) must be accepted, got {code} {body}"
        );
        let settled = settled_stack_status(case, &router, REGION, EXPORTER).await;
        assert_eq!(
            settled, "CREATE_COMPLETE",
            "case {case:?}: expected the exporter to be created, got {settled:?}"
        );

        // What the stack publishes, and the physical resources those values are resolved
        // over, BEFORE the update is submitted.
        let mut before: Vec<(String, String)> = Vec::new();
        for exported in &row.exports {
            let value = reported_output(case, &router, EXPORTER, exported.key).await;
            let publishing = published_value(case, &router, exported.export).await;
            assert_eq!(
                publishing.as_deref(),
                Some(value.as_str()),
                "case {case:?}: expected {} to publish the created value {value:?}, got \
                 {publishing:?}",
                exported.export
            );
            before.push((exported.key.to_string(), value));
        }
        let mut deployed: Vec<(&str, String)> = Vec::new();
        for logical_id in row.replaced {
            let id = physical_resource_id(&router, case, EXPORTER, logical_id).await;
            deployed.push((logical_id, id));
        }

        let (code, body) = create_stack(&router, IMPORTER, &importer_template(&row), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack({IMPORTER}) must be accepted, got {code} {body}"
        );
        let settled = settled_stack_status(case, &router, REGION, IMPORTER).await;
        assert_eq!(
            settled, "CREATE_COMPLETE",
            "case {case:?}: expected the importer to be created, got {settled:?}"
        );

        // The revision replaces the resources the exports are resolved over, so it would move
        // exported values another stack imports: the deployment is carried out and cancelled.
        let mut members = vec![member("StackName", EXPORTER)];
        match row.revision {
            Revision::Template => {
                members.push(member("TemplateBody", &row.template(row.after)));
            }
            Revision::Parameters => {
                members.push(member("UsePreviousTemplate", "true"));
                members.push(member("Parameters.member.1.ParameterKey", "Generation"));
                members.push(member("Parameters.member.1.ParameterValue", row.after));
                members.push(member("Parameters.member.2.ParameterKey", "EmbeddingModelId"));
                members.push(member(
                    "Parameters.member.2.ParameterValue",
                    model_of(row.after),
                ));
            }
        }
        let (code, body) = call(&router, "UpdateStack", &members).await;
        assert_eq!(
            code, 200,
            "case {case:?}: UpdateStack({EXPORTER}) must be accepted, got {code} {body}"
        );
        let settled = settled_stack_status(case, &router, REGION, EXPORTER).await;
        assert_eq!(
            settled, "UPDATE_ROLLBACK_COMPLETE",
            "case {case:?}: expected the update to be cancelled and rolled back, got \
             {settled:?}"
        );

        // The deployment really reached the stage its exports would have been published at —
        // so the forward pass had created the replacements before it was cancelled — and the
        // cancellation names one of the row's exports and the stack that imports it.
        let reasons: Vec<String> = stack_own_events(&router, case, EXPORTER)
            .await
            .into_iter()
            .map(|event| event.reason)
            .collect();
        let expected: Vec<String> = row
            .exports
            .iter()
            .map(|exported| update_canceled_reason(exported.export, IMPORTER))
            .collect();
        let cancellations = reasons
            .iter()
            .filter(|reason| expected.contains(reason))
            .count();
        assert_eq!(
            cancellations, 1,
            "case {case:?}: expected exactly one cancellation naming an export of this stack \
             and {IMPORTER}, got {reasons:?}"
        );
        // ...and the forward pass really acted on every resource it was to replace, so the
        // values the rollback restores below are ones a carried-out deployment had moved.
        for logical_id in row.replaced {
            let statuses = resource_event_statuses(&router, case, EXPORTER, logical_id).await;
            assert!(
                statuses
                    .iter()
                    .any(|status| status == "UPDATE_IN_PROGRESS"),
                "case {case:?}: expected the cancelled update to have acted on {logical_id}, \
                 got {statuses:?}"
            );
        }

        // The rollback is absent in its effect: the stack's own outputs, the exports the
        // importer holds, and the physical resources behind them are what was in force before
        // the update.
        for (logical_id, id) in &deployed {
            let now = physical_resource_id(&router, case, EXPORTER, logical_id).await;
            assert_eq!(
                &now, id,
                "case {case:?}: expected {logical_id} to report the physical resource in force \
                 before the update ({id:?}), got {now:?}"
            );
        }
        for (exported, (key, was)) in row.exports.iter().zip(&before) {
            let now = reported_output(case, &router, EXPORTER, key).await;
            assert_eq!(
                &now, was,
                "case {case:?}: expected the output {key} to be restored to {was:?}, got {now:?}"
            );
            let publishing = published_value(case, &router, exported.export).await;
            assert_eq!(
                publishing.as_deref(),
                Some(was.as_str()),
                "case {case:?}: expected {} to publish {was:?} again, got {publishing:?}",
                exported.export
            );
            // ...and the resource the restored export names SURVIVED the rollback: the owning
            // service still reports it, in the configuration the stack went back to.
            if row.knowledge_bases {
                let (status, model) = reported_knowledge_base(case, &router, was).await;
                assert_eq!(
                    status, ACTIVE,
                    "case {case:?}: expected the knowledge base {was:?} the restored export \
                     names to be reported {ACTIVE}, got {status:?}"
                );
                assert_eq!(
                    model,
                    model_arn(model_of(row.before)),
                    "case {case:?}: expected {was:?} to carry the embedding model in force \
                     before the update, got {model:?}"
                );
            }
        }
    }
}
