// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the identifiers this service GENERATES: the form each is spelled in,
//! and the fact that the service mints them per creation instead of deriving them from the
//! request.
//!
//! RULE — an identifier the caller never sends is minted ONCE, where the resource or the node
//! it names enters the service's state, persisted with it, and spelled in the form AWS issues
//! for that class of record: a DATA QUALITY record carries the kind of record it is followed by
//! a 32-character lowercase hexadecimal token (the grammar the service states for a data
//! quality run id in its own refusal, `dqrun-`, so a ruleset reads `dqruleset-<token>`), and a
//! WORKFLOW GRAPH NODE carries an opaque token of the form the model pins for the one generated
//! id it constrains (`smithy_query shape=SchemaVersionIdString`, length 36 and pattern
//! `^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`). Either way the value is
//! stable across reads, no two records or nodes share one, and nothing of the names the request
//! carried can be read out of it or collide through it: the same names registered again are a
//! new record with a new identifier.
//!
//! The surfaces under test are the two whose identifier the caller cannot send: the
//! `Identifier` a data quality ruleset is created with, and the `Node$UniqueId` of a
//! workflow's graph, which the graph's `Edge$SourceId` and `Edge$DestinationId` reference.
//!
//! The rows differ along what the rule generalises over: for a ruleset, the form and length of
//! the name and which optional members the create registers; for a graph, the type of the
//! trigger the workflow is made of, how many components it names, whether those are jobs or
//! crawlers and whether the trigger waits on them or initiates them.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{emulator, ok};

/// The role ARN every job and crawler below is registered with.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/sim2real-glue-job-role";

/// The widths of the groups a generated NODE identifier is spelled in, in order: the 8-4-4-4-12
/// of `SchemaVersionIdString`'s pattern (`smithy_query shape=SchemaVersionIdString`).
const GENERATED_ID_GROUPS: [usize; 5] = [8, 4, 4, 4, 12];

/// Where the UUID VERSION digit sits in the third group, and the version AWS mints: a
/// generated identifier is a randomly generated (version-4) UUID, as the schema registry's own
/// modelled id is.
const UUID_VERSION_GROUP: usize = 2;
const UUID_VERSION_DIGIT: char = '4';

/// Where the UUID VARIANT digit sits, and the digits the canonical variant admits.
const UUID_VARIANT_GROUP: usize = 3;
const UUID_VARIANT_DIGITS: [char; 4] = ['8', '9', 'a', 'b'];

/// The fixed text a data quality RULESET identifier begins with, written out here rather than
/// read from the crate's own constant so this suite pins the grammar.
const RULESET_ID_PREFIX: &str = "dqruleset-";

/// How many hexadecimal characters the token of a data quality identifier carries.
const DATA_QUALITY_TOKEN_WIDTH: usize = 32;

/// Whether `token` is `width` lowercase hexadecimal characters.
fn is_hex_token(token: &str, width: usize) -> bool {
    token.chars().count() == width
        && token
            .chars()
            .all(|digit| matches!(digit, '0'..='9' | 'a'..='f'))
}

/// Whether `id` is spelled the way AWS spells a generated NODE identifier of this service:
/// [`GENERATED_ID_GROUPS`] groups of lowercase hexadecimal digits, separated by `-`, carrying
/// the version and variant digits of a randomly generated UUID.
fn is_generated_id_form(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    if groups.len() != GENERATED_ID_GROUPS.len() {
        return false;
    }
    let spelled = groups
        .iter()
        .zip(GENERATED_ID_GROUPS)
        .all(|(group, width)| is_hex_token(group, width));
    let version = groups[UUID_VERSION_GROUP].starts_with(UUID_VERSION_DIGIT);
    let variant = groups[UUID_VARIANT_GROUP]
        .chars()
        .next()
        .is_some_and(|digit| UUID_VARIANT_DIGITS.contains(&digit));
    spelled && version && variant
}

/// Assert that `id`, reported for `member` by `operation`, is a data quality identifier: the
/// kind of record it names, then the token that identifies the one record.
fn assert_data_quality_id_form(case: &str, operation: &str, member: &str, id: &str) {
    let token = id.strip_prefix(RULESET_ID_PREFIX);
    assert!(
        token.is_some_and(|token| is_hex_token(token, DATA_QUALITY_TOKEN_WIDTH)),
        "case {case}: {operation} must report {member} as {RULESET_ID_PREFIX:?} followed by \
         {DATA_QUALITY_TOKEN_WIDTH} lowercase hexadecimal characters, got {id:?}"
    );
}

/// Assert that `id`, reported for `member` by `operation`, is of the generated-identifier
/// form.
fn assert_generated_id_form(case: &str, operation: &str, member: &str, id: &str) {
    assert!(
        is_generated_id_form(id),
        "case {case}: {operation} must report {member} as a randomly generated UUID — \
         {GENERATED_ID_GROUPS:?} groups of lowercase hexadecimal digits carrying version \
         {UUID_VERSION_DIGIT} — got {id:?}"
    );
}

/// The `Identifier` an answer about a ruleset reports.
fn identifier_of(case: &str, operation: &str, body: &Value) -> String {
    body.get("Identifier")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case}: {operation} must report the ruleset's Identifier, got {body}")
        })
        .to_string()
}

/// Every identifier a data quality ruleset is assigned carries the grammar of this service's
/// data quality ids, is reported unchanged by the read of that ruleset, and is not a function
/// of what the create carried: a ruleset registered again under the same name, with the same
/// definition, is given another one.
#[tokio::test]
async fn a_ruleset_identifier_is_a_generated_data_quality_id() {
    let router = emulator();

    // (case, the create body)
    let rows: Vec<(&str, Value)> = vec![
        (
            "a ruleset targeting a table",
            json!({
                "Name": "id-form-orders",
                "Ruleset": "Rules = [ ColumnExists \"order_id\" ]",
                "Description": "the registered description",
                "TargetTable": { "DatabaseName": "sales", "TableName": "orders" },
            }),
        ),
        (
            "a ruleset targeting no table at all",
            json!({
                "Name": "id-form-untargeted",
                "Ruleset": "Rules = [ RowCount > 0 ]",
            }),
        ),
        (
            "a ruleset whose name is one character long",
            json!({
                "Name": "r",
                "Ruleset": "Rules = [ IsComplete \"id\" ]",
                "Tags": { "owner": "data-governance" },
            }),
        ),
        (
            "a ruleset whose name is long and hyphenated",
            json!({
                "Name": "id-form-a-rather-long-and-hyphenated-ruleset-name-0123456789",
                "Ruleset": "Rules = [ ColumnExists \"a\", ColumnExists \"b\" ]",
                "ClientToken": "id-form-token",
            }),
        ),
    ];

    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (case, create) in &rows {
        let created = ok(&router, "CreateDataQualityRuleset", create).await;
        let identifier = identifier_of(case, "CreateDataQualityRuleset", &created);
        assert_data_quality_id_form(case, "CreateDataQualityRuleset", "Identifier", &identifier);
        assert!(
            seen.insert(identifier.clone()),
            "case {case}: two rulesets must not share an Identifier, got {identifier:?} again"
        );

        // The identity is the RECORD's, so the read of the ruleset reports the same value.
        let name = create["Name"].as_str().unwrap_or_default();
        let read = ok(
            &router,
            "GetDataQualityRuleset",
            &json!({ "Name": name }),
        )
        .await;
        assert_eq!(
            identifier_of(case, "GetDataQualityRuleset", &read),
            identifier,
            "case {case}: the read must report the identifier the create assigned, got {read}"
        );

        // Nothing of the request decides the value: the same create, after a delete, is a new
        // record and is given a new identifier.
        ok(
            &router,
            "DeleteDataQualityRuleset",
            &json!({ "Name": name }),
        )
        .await;
        let again = ok(&router, "CreateDataQualityRuleset", create).await;
        let recreated = identifier_of(case, "the create after a delete", &again);
        assert_data_quality_id_form(case, "the create after a delete", "Identifier", &recreated);
        assert_ne!(
            recreated, identifier,
            "case {case}: a new record must be given a new identifier, got {recreated:?} twice"
        );
        assert!(
            seen.insert(recreated),
            "case {case}: an identifier must not be handed out twice, got it again in {again}"
        );
    }
}

/// Register the jobs and the crawler the trigger definitions below name, so their actions and
/// conditions resolve.
async fn register_components(router: &MultiServiceRouter, job: &str, other: &str, crawler: &str) {
    for name in [job, other] {
        ok(
            router,
            "CreateJob",
            &json!({
                "Name": name,
                "Role": ROLE_ARN,
                "Command": { "Name": "glueetl", "ScriptLocation": "s3://etl-scripts/etl.py" },
            }),
        )
        .await;
    }
    ok(
        router,
        "CreateCrawler",
        &json!({
            "Name": crawler,
            "Role": ROLE_ARN,
            "Targets": { "S3Targets": [{ "Path": "s3://sim2real-glue-crawler/data/" }] },
        }),
    )
    .await;
}

/// The `Graph` a `GetWorkflow` with `IncludeGraph` reports for `workflow`.
async fn graph_of(router: &MultiServiceRouter, case: &str, workflow: &str) -> Value {
    let body = ok(
        router,
        "GetWorkflow",
        &json!({ "Name": workflow, "IncludeGraph": true }),
    )
    .await;
    body["Workflow"]
        .get("Graph")
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: IncludeGraph must report a Graph, got {body}"))
}

/// The list `graph` reports under `member`.
fn listed(case: &str, graph: &Value, member: &str) -> Vec<Value> {
    graph
        .get(member)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case}: Graph.{member} must be a list, got {graph}"))
        .clone()
}

/// The `UniqueId` of every node of `graph`, asserting the form of each.
fn node_ids(case: &str, graph: &Value) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for node in listed(case, graph, "Nodes") {
        let id = node
            .get("UniqueId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case}: every node reports a UniqueId, got {node}"))
            .to_string();
        assert_generated_id_form(case, "GetWorkflow", "Node.UniqueId", &id);
        assert!(
            ids.insert(id.clone()),
            "case {case}: two nodes must not share a UniqueId, got {id:?} again in {graph}"
        );
    }
    ids
}

/// The `UniqueId` of the node of `graph` that stands for the component `name`.
fn node_id_of(case: &str, graph: &Value, name: &str) -> String {
    listed(case, graph, "Nodes")
        .iter()
        .find(|node| node.get("Name").and_then(Value::as_str) == Some(name))
        .and_then(|node| node.get("UniqueId").and_then(Value::as_str))
        .unwrap_or_else(|| {
            panic!("case {case}: the graph must hold a node for {name:?}, got {graph}")
        })
        .to_string()
}

/// Every node of a workflow's graph is addressed by a generated identifier of AWS's form, the
/// graph's edges reference exactly those identifiers, and the identifiers belong to the
/// WORKFLOW rather than to the names it is made of: a workflow registered again under the same
/// name, with the same components attached, reports another set of them.
#[tokio::test]
async fn a_graph_node_is_addressed_by_a_generated_id_of_aws_form() {
    let job = "id-form-first-job";
    let other_job = "id-form-second-job";
    let crawler = "id-form-crawler";

    // (case, the trigger bodies the workflow is made of)
    let rows: Vec<(&str, Vec<Value>)> = vec![
        (
            "an on-demand trigger running one job",
            vec![json!({
                "Name": "id-form-on-demand",
                "Type": "ON_DEMAND",
                "Actions": [{ "JobName": job }],
            })],
        ),
        (
            "a scheduled trigger running a job and a crawler",
            vec![json!({
                "Name": "id-form-scheduled",
                "Type": "SCHEDULED",
                "Schedule": "cron(15 12 * * ? *)",
                "Actions": [{ "JobName": job }, { "CrawlerName": crawler }],
            })],
        ),
        (
            "a conditional trigger waiting on one job and running another",
            vec![json!({
                "Name": "id-form-conditional",
                "Type": "CONDITIONAL",
                "Predicate": {
                    "Conditions": [
                        { "LogicalOperator": "EQUALS", "JobName": job, "State": "SUCCEEDED" },
                    ],
                },
                "Actions": [{ "JobName": other_job }],
            })],
        ),
        (
            "two triggers naming the same job",
            vec![
                json!({
                    "Name": "id-form-pair-first",
                    "Type": "ON_DEMAND",
                    "Actions": [{ "JobName": job }],
                }),
                json!({
                    "Name": "id-form-pair-second",
                    "Type": "ON_DEMAND",
                    "Actions": [{ "JobName": job }, { "JobName": other_job }],
                }),
            ],
        ),
    ];

    for (index, (case, triggers)) in rows.iter().enumerate() {
        let router = emulator();
        register_components(&router, job, other_job, crawler).await;
        let workflow = format!("id-form-workflow-{index}");
        ok(&router, "CreateWorkflow", &json!({ "Name": workflow })).await;
        for trigger in triggers {
            let mut body = trigger.clone();
            body["WorkflowName"] = json!(workflow);
            ok(&router, "CreateTrigger", &body).await;
        }

        let graph = graph_of(&router, case, &workflow).await;
        let ids = node_ids(case, &graph);
        assert!(
            !ids.is_empty(),
            "case {case}: the workflow's components are nodes of its graph, got {graph}"
        );
        let edges = listed(case, &graph, "Edges");
        assert!(
            !edges.is_empty(),
            "case {case}: the trigger reaches the components it names, got {graph}"
        );
        for edge in &edges {
            for member in ["SourceId", "DestinationId"] {
                let end = edge
                    .get(member)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("case {case}: an edge reports its {member}, got {edge}")
                    });
                assert_generated_id_form(case, "GetWorkflow", member, end);
                assert!(
                    ids.contains(end),
                    "case {case}: {member} {end:?} must name a node of the graph, got {graph}"
                );
            }
        }

        // A read assigns nothing: the ids a later caller reads are the ones the write minted.
        let again = graph_of(&router, case, &workflow).await;
        assert_eq!(
            again, graph,
            "case {case}: two reads of an unchanged graph must agree, got {again}"
        );

        // The ids belong to this workflow's nodes, not to the names: the same components
        // attached to a workflow registered again under that name are new nodes.
        ok(&router, "DeleteWorkflow", &json!({ "Name": workflow })).await;
        ok(&router, "CreateWorkflow", &json!({ "Name": workflow })).await;
        let rebuilt = graph_of(&router, case, &workflow).await;
        let rebuilt_ids = node_ids(case, &rebuilt);
        assert_eq!(
            rebuilt_ids.len(),
            ids.len(),
            "case {case}: the components attached to the name are the graph of the workflow \
             registered under it, got {rebuilt}"
        );
        assert!(
            rebuilt_ids.is_disjoint(&ids),
            "case {case}: a new workflow's nodes are new nodes, so none of {ids:?} may be \
             reported again, got {rebuilt}"
        );
    }
}

/// The nodes a graph holds ids for are the nodes its triggers place in it: a component a
/// trigger no longer names leaves the graph, the components it still names keep the ids they
/// were assigned, and a component it newly names is assigned one of its own.
#[tokio::test]
async fn the_nodes_a_graph_holds_follow_the_triggers_attached_to_it() {
    let case = "a trigger whose action is repointed, then deleted and created again";
    let job = "id-form-lifecycle-first-job";
    let other_job = "id-form-lifecycle-second-job";
    let crawler = "id-form-lifecycle-crawler";
    let workflow = "id-form-lifecycle-workflow";
    let trigger = "id-form-lifecycle-trigger";

    let router = emulator();
    register_components(&router, job, other_job, crawler).await;
    ok(&router, "CreateWorkflow", &json!({ "Name": workflow })).await;
    ok(
        &router,
        "CreateTrigger",
        &json!({
            "Name": trigger,
            "Type": "ON_DEMAND",
            "WorkflowName": workflow,
            "Actions": [{ "JobName": job }],
        }),
    )
    .await;

    let placed = graph_of(&router, case, workflow).await;
    let first = node_ids(case, &placed);
    let trigger_id = node_id_of(case, &placed, trigger);
    let job_id = node_id_of(case, &placed, job);

    // The action is repointed at the other job: the trigger is the same node and keeps its
    // id, the job it no longer initiates is no node of the graph, and the one it now
    // initiates is a node with an id of its own.
    ok(
        &router,
        "UpdateTrigger",
        &json!({
            "Name": trigger,
            "TriggerUpdate": { "Actions": [{ "JobName": other_job }] },
        }),
    )
    .await;
    let repointed = graph_of(&router, case, workflow).await;
    let after = node_ids(case, &repointed);
    assert_eq!(
        node_id_of(case, &repointed, trigger),
        trigger_id,
        "case {case}: the trigger is the same node, so it keeps its id, got {repointed}"
    );
    assert!(
        !after.contains(&job_id),
        "case {case}: the job the trigger no longer names is no node of the graph, but \
         {job_id:?} is still reported in {repointed}"
    );
    let repointed_job_id = node_id_of(case, &repointed, other_job);
    assert_generated_id_form(case, "GetWorkflow", "Node.UniqueId", &repointed_job_id);
    assert!(
        !first.contains(&repointed_job_id),
        "case {case}: a component newly named is a new node, got {repointed_job_id:?} twice"
    );

    // The trigger deleted takes the graph with it: nothing places a node any more.
    ok(&router, "DeleteTrigger", &json!({ "Name": trigger })).await;
    let emptied = graph_of(&router, case, workflow).await;
    assert!(
        node_ids(case, &emptied).is_empty() && listed(case, &emptied, "Edges").is_empty(),
        "case {case}: a workflow nothing is attached to has a graph with no nodes and no \
         edges, got {emptied}"
    );

    // And a trigger created again under that name is a NEW node of the graph.
    ok(
        &router,
        "CreateTrigger",
        &json!({
            "Name": trigger,
            "Type": "ON_DEMAND",
            "WorkflowName": workflow,
            "Actions": [{ "JobName": job }],
        }),
    )
    .await;
    let recreated = graph_of(&router, case, workflow).await;
    let recreated_trigger_id = node_id_of(case, &recreated, trigger);
    assert_generated_id_form(case, "GetWorkflow", "Node.UniqueId", &recreated_trigger_id);
    assert_ne!(
        recreated_trigger_id, trigger_id,
        "case {case}: a trigger created again is a new node, so its id is a new one, got \
         {recreated}"
    );
}
