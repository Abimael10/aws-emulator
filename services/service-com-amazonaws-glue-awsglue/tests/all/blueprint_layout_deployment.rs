// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a Glue BLUEPRINT RUN deploys: the layout published inside the
//! blueprint's archive is executed, the entities the workflow it returns carries are created,
//! and the run comes to rest in what that came to.
//!
//! RULE: "a blueprint … contains the business logic needed to create a workflow", and that
//! logic is the `layoutGenerator` the archive at `Blueprint$BlueprintLocation` names in its
//! `blueprint.cfg` — so a run of a blueprint executes that function with the run's
//! `Parameters` and creates the jobs, crawlers and triggers the `Workflow` it returns carries,
//! into the workflow the run produced ("the name of a workflow that is created as a result of
//! a successful blueprint run", `BlueprintRun$WorkflowName`). The run then reports what that
//! came to: `SUCCEEDED` for a run that created at least one entity, whose workflow's graph
//! carries those entities and whose jobs and triggers are readable through their own readers,
//! and `FAILED` carrying an `ErrorMessage` for a run whose layout created nothing, raised, or
//! could not be read at all — "if a blueprint run has an error, there will not be a workflow
//! created", so such a run reports no `WorkflowName` and leaves no workflow behind.
//!
//! Amazon S3 is mounted beside Glue on one engine, so the archive a row stages with
//! `PutObject` ([`crate::blueprint_archive_registration::stage_archive`], the one way a suite
//! of this target publishes a blueprint archive) is the archive the run executes: nothing here
//! is interpreted by the test, the layout really runs inside the emulator and its entities are
//! created through Glue's own API.
//!
//! The rows differ along what the rule generalises over: what the layout creates (one job, a
//! job and its own trigger, two jobs chained by a conditional trigger, a crawler, nothing at
//! all), how the layout fails (it raises, the archive names no generator, the archive is no
//! archive, the location holds no object), and what the workflow it names is called.
//!
//! The due instants a registration and a run settle at are moved THROUGH THE STORE rather
//! than slept for, so every transition here is deterministic and no outcome depends on how
//! fast the host runs.

use std::io::Write;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{BLUEPRINT_RUN_SETTLES_AFTER, BLUEPRINT_SETTLES_AFTER};
use smithy_server_runtime::MultiServiceRouter;

use crate::blueprint_archive_registration::stage_archive;
use crate::blueprint_runs::rewind;
use crate::job_script_execution::{create_bucket, emulator_with_s3, BUCKET};
use crate::wire::{call, error_code, ok, s3_client};

/// The role a blueprint run is started as: "this role will be assumed by the Glue service and
/// will be used to create the workflow and other entities of a workflow"
/// (`StartBlueprintRun$RoleArn`).
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/sim2real-glue-blueprint-role";

/// The role the layouts below give the jobs and crawlers they create.
const PASS_ROLE: &str = "arn:aws:iam::000000000000:role/sim2real-glue-pass-role";

/// The `BlueprintRunState` values a settled run reports: "Succeeded — The blueprint run
/// completed successfully" and "Failed — The blueprint run failed and rollback is complete"
/// (`smithy_query shape=BlueprintRun`, member `State`).
const STATE_SUCCEEDED: &str = "SUCCEEDED";
const STATE_FAILED: &str = "FAILED";

/// The `WorkflowGraph` node types a deployed entity shows up as: `JOB`, `CRAWLER` and
/// `TRIGGER` (`smithy_query shape=NodeType`).
const NODE_JOB: &str = "JOB";
const NODE_CRAWLER: &str = "CRAWLER";
const NODE_TRIGGER: &str = "TRIGGER";

/// A .zip archive holding `entries`, written in memory the way a caller publishes a blueprint.
fn archive(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        for (path, source) in entries {
            writer
                .start_file(*path, options)
                .expect("the archive entry starts");
            writer
                .write_all(source.as_bytes())
                .expect("the archive entry's bytes");
        }
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// The `blueprint.cfg` of an archive whose layout lives in `layout.py`.
const CONFIGURATION: &str = r#"{"layoutGenerator": "layout.generate_layout"}"#;

/// Register the blueprint `name` at `location` and move its registration past the span the
/// clock owed it, so a run may be started of it.
async fn register(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
    location: &str,
) {
    ok(
        router,
        "CreateBlueprint",
        &json!({ "Name": name, "BlueprintLocation": location }),
    )
    .await;
    rewind(engine, name, BLUEPRINT_SETTLES_AFTER);
}

/// Start a run of `blueprint` for the workflow `workflow`, move it past the span the clock
/// owed it, and answer the `BlueprintRun` a read of it reports.
async fn run_to_rest(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    blueprint: &str,
    workflow: &str,
) -> Value {
    let parameters = json!({
        "WorkflowName": workflow,
        "PassRole": PASS_ROLE,
        "ScriptLocation": format!("s3://{BUCKET}/scripts/etl.py"),
        "TargetPath": format!("s3://{BUCKET}/raw/"),
    })
    .to_string();
    let started = ok(
        router,
        "StartBlueprintRun",
        &json!({
            "BlueprintName": blueprint,
            "RoleArn": ROLE_ARN,
            "Parameters": parameters,
        }),
    )
    .await;
    let run_id = started
        .get("RunId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("StartBlueprintRun must report a RunId, got {started}"))
        .to_string();
    rewind(engine, blueprint, BLUEPRINT_RUN_SETTLES_AFTER);
    let read = ok(
        router,
        "GetBlueprintRun",
        &json!({ "BlueprintName": blueprint, "RunId": run_id }),
    )
    .await;
    read.get("BlueprintRun")
        .cloned()
        .unwrap_or_else(|| panic!("GetBlueprintRun must report a BlueprintRun, got {read}"))
}

/// The `(type, name)` pair of every node of the workflow `name`'s graph, sorted so one order
/// is asserted whatever order the graph is walked in.
async fn graph_nodes(router: &MultiServiceRouter, name: &str) -> Vec<(String, String)> {
    let read = ok(
        router,
        "GetWorkflow",
        &json!({ "Name": name, "IncludeGraph": true }),
    )
    .await;
    let mut nodes: Vec<(String, String)> = read
        .get("Workflow")
        .and_then(|workflow| workflow.get("Graph"))
        .and_then(|graph| graph.get("Nodes"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("GetWorkflow must report a graph with Nodes, got {read}"))
        .iter()
        .map(|node| {
            (
                node.get("Type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                node.get("Name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    nodes.sort();
    nodes
}

/// A layout that creates ONE job and names no trigger of its own: the shape the AWS blueprint
/// sample has, which leaves the workflow's starting trigger to the deployment.
const ONE_JOB_LAYOUT: &str = r#"
from awsglue.blueprint.workflow import *
from awsglue.blueprint.job import *


def generate_layout(user_params, system_params):
    etl_job = Job(
        Name="%s_etl_job" % user_params["WorkflowName"],
        Role=user_params["PassRole"],
        Command={
            "Name": "glueetl",
            "ScriptLocation": user_params["ScriptLocation"],
            "PythonVersion": "3",
        },
        DefaultArguments={"--region": system_params["region"]},
    )
    return Workflow(
        Name=user_params["WorkflowName"],
        Entities=Entities(Jobs=[etl_job]),
        Description="deployed from a blueprint",
    )
"#;

/// A layout that creates a job AND the on-demand trigger that runs it.
const JOB_AND_TRIGGER_LAYOUT: &str = r#"
from awsglue.blueprint.workflow import *
from awsglue.blueprint.job import *


def generate_layout(user_params, system_params):
    workflow_name = user_params["WorkflowName"]
    etl_job = Job(
        Name="%s_etl_job" % workflow_name,
        Role=user_params["PassRole"],
        Command={"Name": "glueetl", "ScriptLocation": user_params["ScriptLocation"]},
    )
    starting = OnDemandTrigger(
        Name="%s_entry" % workflow_name,
        Actions=[Action(etl_job)],
    )
    return Workflow(
        Name=workflow_name,
        Entities=Entities(Jobs=[etl_job], Triggers=[starting]),
    )
"#;

/// A layout that chains two jobs with a conditional trigger, the shape a staged workflow has.
const CHAINED_JOBS_LAYOUT: &str = r#"
from awsglue.blueprint.workflow import *
from awsglue.blueprint.job import *


def generate_layout(user_params, system_params):
    workflow_name = user_params["WorkflowName"]
    role = user_params["PassRole"]
    first = Job(Name="%s_first" % workflow_name, Role=role, Command={"Name": "glueetl"})
    second = Job(Name="%s_second" % workflow_name, Role=role, Command={"Name": "glueetl"})
    gate = ConditionalTrigger(
        Name="%s_gate" % workflow_name,
        Predicate=Predicate(
            Conditions=[Condition(LogicalOperator="EQUALS", Job=first, State="SUCCEEDED")]
        ),
        Actions=[Action(second)],
    )
    return Workflow(
        Name=workflow_name,
        Entities=Entities(Jobs=[first, second], Triggers=[gate]),
    )
"#;

/// A layout that creates a crawler, which a blueprint may deploy instead of a job.
const CRAWLER_LAYOUT: &str = r#"
from awsglue.blueprint.workflow import *
from awsglue.blueprint.crawler import *


def generate_layout(user_params, system_params):
    workflow_name = user_params["WorkflowName"]
    crawler = Crawler(
        Name="%s_crawler" % workflow_name,
        Role=user_params["PassRole"],
        DatabaseName="blueprint_target_db",
        Targets={"S3Targets": [{"Path": user_params["TargetPath"]}]},
    )
    return Workflow(Name=workflow_name, Entities=Entities(Crawlers=[crawler]))
"#;

/// A layout that returns a workflow with NO entity at all, which is unrunnable.
const NO_ENTITY_LAYOUT: &str = r#"
from awsglue.blueprint.workflow import *


def generate_layout(user_params, system_params):
    return Workflow(Name=user_params["WorkflowName"], Entities=Entities())
"#;

/// A layout that raises, so the run reports the layout's own failure.
const RAISING_LAYOUT: &str = r#"
def generate_layout(user_params, system_params):
    raise ValueError("the blueprint's parameters name no source table")
"#;

// ---------------------------------------------------------------------------
// What a run deploys
// ---------------------------------------------------------------------------

/// One row of the deployment table.
struct Deployment {
    /// What this row varies, named in every assertion it drives.
    case: &'static str,
    /// The layout published in the archive.
    layout: &'static str,
    /// The workflow the run's parameters ask for.
    workflow: &'static str,
    /// The `(type, name)` nodes the deployed workflow's graph must carry.
    nodes: Vec<(&'static str, String)>,
    /// The jobs a `GetJob` must read back.
    jobs: Vec<String>,
    /// The crawlers a `GetCrawler` must read back.
    crawlers: Vec<String>,
}

/// The rows: the layouts a run deploys, and what each leaves behind.
fn deployments() -> Vec<Deployment> {
    vec![
        Deployment {
            case: "a layout creating one job and no trigger of its own",
            layout: ONE_JOB_LAYOUT,
            workflow: "bp_one_job",
            nodes: vec![
                (NODE_JOB, "bp_one_job_etl_job".to_string()),
                (NODE_TRIGGER, "bp_one_job_starting_trigger".to_string()),
            ],
            jobs: vec!["bp_one_job_etl_job".to_string()],
            crawlers: Vec::new(),
        },
        Deployment {
            case: "a layout creating a job and the trigger that runs it",
            layout: JOB_AND_TRIGGER_LAYOUT,
            workflow: "bp_job_trigger",
            nodes: vec![
                (NODE_JOB, "bp_job_trigger_etl_job".to_string()),
                (NODE_TRIGGER, "bp_job_trigger_entry".to_string()),
            ],
            jobs: vec!["bp_job_trigger_etl_job".to_string()],
            crawlers: Vec::new(),
        },
        Deployment {
            case: "a layout chaining two jobs with a conditional trigger",
            layout: CHAINED_JOBS_LAYOUT,
            workflow: "bp_chained",
            nodes: vec![
                (NODE_JOB, "bp_chained_first".to_string()),
                (NODE_JOB, "bp_chained_second".to_string()),
                (NODE_TRIGGER, "bp_chained_gate".to_string()),
                (NODE_TRIGGER, "bp_chained_starting_trigger".to_string()),
            ],
            jobs: vec![
                "bp_chained_first".to_string(),
                "bp_chained_second".to_string(),
            ],
            crawlers: Vec::new(),
        },
        Deployment {
            case: "a layout creating a crawler",
            layout: CRAWLER_LAYOUT,
            workflow: "bp_crawler",
            nodes: vec![
                (NODE_CRAWLER, "bp_crawler_crawler".to_string()),
                (NODE_TRIGGER, "bp_crawler_starting_trigger".to_string()),
            ],
            jobs: Vec::new(),
            crawlers: vec!["bp_crawler_crawler".to_string()],
        },
    ]
}

/// A run whose layout creates entities SUCCEEDS carrying the workflow it deployed them into:
/// the workflow's graph reports those entities, and each one is readable through its own
/// reader — so what the run reports as deployed really exists.
#[tokio::test]
async fn a_run_deploys_the_entities_its_layout_creates() {
    let (router, engine) = emulator_with_s3();
    create_bucket(&router).await;
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "blueprint_target_db" } }),
    )
    .await;

    for (index, row) in deployments().into_iter().enumerate() {
        let case = row.case;
        let blueprint = format!("deploying-blueprint-{index}");
        let location = stage_archive(
            &router,
            &format!("blueprints/{index}.zip"),
            archive(&[("blueprint.cfg", CONFIGURATION), ("layout.py", row.layout)]),
        )
        .await;
        register(&router, &engine, &blueprint, &location).await;

        let run = run_to_rest(&router, &engine, &blueprint, row.workflow).await;
        assert_eq!(
            run.get("State").and_then(Value::as_str),
            Some(STATE_SUCCEEDED),
            "case {case}: expected the run to succeed, got {run}"
        );
        assert_eq!(
            run.get("WorkflowName").and_then(Value::as_str),
            Some(row.workflow),
            "case {case}: expected the run to report the workflow it deployed, got {run}"
        );
        assert_eq!(
            run.get("ErrorMessage"),
            None,
            "case {case}: a run that deployed its entities carries no error, got {run}"
        );

        let expected: Vec<(String, String)> = {
            let mut nodes: Vec<(String, String)> = row
                .nodes
                .iter()
                .map(|(node_type, name)| ((*node_type).to_string(), name.clone()))
                .collect();
            nodes.sort();
            nodes
        };
        assert_eq!(
            graph_nodes(&router, row.workflow).await,
            expected,
            "case {case}: expected the deployed workflow's graph to carry {expected:?}"
        );

        for job in &row.jobs {
            let read = ok(&router, "GetJob", &json!({ "JobName": job })).await;
            assert_eq!(
                read.get("Job")
                    .and_then(|held| held.get("Role"))
                    .and_then(Value::as_str),
                Some(PASS_ROLE),
                "case {case}: the job {job} must be readable with the role the layout gave \
                 it, got {read}"
            );
        }
        for crawler in &row.crawlers {
            let read = ok(&router, "GetCrawler", &json!({ "Name": crawler })).await;
            assert_eq!(
                read.get("Crawler")
                    .and_then(|held| held.get("Role"))
                    .and_then(Value::as_str),
                Some(PASS_ROLE),
                "case {case}: the crawler {crawler} must be readable with the role the layout \
                 gave it, got {read}"
            );
        }
    }
}

/// The workflow a run deploys carries the members its layout states about it, which the
/// deployment applies to the workflow the run created.
#[tokio::test]
async fn the_deployed_workflow_carries_what_the_layout_states() {
    let (router, engine) = emulator_with_s3();
    create_bucket(&router).await;
    let location = stage_archive(
        &router,
        "blueprints/described.zip",
        archive(&[("blueprint.cfg", CONFIGURATION), ("layout.py", ONE_JOB_LAYOUT)]),
    )
    .await;
    register(&router, &engine, "described-blueprint", &location).await;

    let run = run_to_rest(&router, &engine, "described-blueprint", "bp_described").await;
    assert_eq!(
        run.get("State").and_then(Value::as_str),
        Some(STATE_SUCCEEDED),
        "the run must succeed, got {run}"
    );
    let read = ok(&router, "GetWorkflow", &json!({ "Name": "bp_described" })).await;
    let workflow = read
        .get("Workflow")
        .unwrap_or_else(|| panic!("GetWorkflow must report a Workflow, got {read}"));
    assert_eq!(
        workflow.get("Description").and_then(Value::as_str),
        Some("deployed from a blueprint"),
        "the workflow must carry the description its layout stated, got {read}"
    );
    assert_eq!(
        workflow
            .get("BlueprintDetails")
            .and_then(|details| details.get("BlueprintName"))
            .and_then(Value::as_str),
        Some("described-blueprint"),
        "the workflow must name the blueprint it was created from, got {read}"
    );
}

// ---------------------------------------------------------------------------
// What fails a run
// ---------------------------------------------------------------------------

/// One row of the failure table: what is published, and what the run must report.
struct Failure {
    /// What this row varies.
    case: &'static str,
    /// The entries of the archive this row stages, and `None` for a row that stages none at
    /// all.
    entries: Option<Vec<(&'static str, &'static str)>>,
    /// Bytes to stage that are no .zip archive, when the row stages those instead.
    raw: Option<&'static str>,
    /// A fragment the run's `ErrorMessage` must carry.
    reason: &'static str,
}

/// The rows: a layout that creates nothing, one that raises, and the three ways the layout
/// cannot be reached at all.
fn failures() -> Vec<Failure> {
    vec![
        Failure {
            case: "a layout that creates no entity at all",
            entries: Some(vec![
                ("blueprint.cfg", CONFIGURATION),
                ("layout.py", NO_ENTITY_LAYOUT),
            ]),
            raw: None,
            // A workflow with no entity has nothing for its starting trigger to initiate, so
            // the trigger create refuses it and that refusal is what fails the run — the
            // sentence a trigger write words an empty `Actions` with.
            reason: "Actions cannot be null or empty",
        },
        Failure {
            case: "a layout that raises",
            entries: Some(vec![
                ("blueprint.cfg", CONFIGURATION),
                ("layout.py", RAISING_LAYOUT),
            ]),
            raw: None,
            reason: "the blueprint's parameters name no source table",
        },
        Failure {
            case: "an archive naming no layout generator",
            entries: Some(vec![("layout.py", ONE_JOB_LAYOUT)]),
            raw: None,
            reason: "blueprint.cfg",
        },
        Failure {
            case: "an archive whose configuration names a module it does not hold",
            entries: Some(vec![(
                "blueprint.cfg",
                r#"{"layoutGenerator": "absent_module.generate_layout"}"#,
            )]),
            raw: None,
            reason: "absent_module",
        },
        Failure {
            case: "bytes that are no .zip archive",
            entries: None,
            raw: Some("not an archive at all"),
            reason: "could not be read",
        },
    ]
}

/// A run whose layout created nothing, raised, or could not be read FAILS carrying the reason
/// — and leaves no workflow behind, because "if a blueprint run has an error, there will not
/// be a workflow created".
#[tokio::test]
async fn a_run_whose_layout_deploys_nothing_fails() {
    let (router, engine) = emulator_with_s3();
    create_bucket(&router).await;

    for (index, row) in failures().into_iter().enumerate() {
        let case = row.case;
        let blueprint = format!("failing-blueprint-{index}");
        let workflow = format!("bp_failing_{index}");
        let bytes = match (&row.entries, row.raw) {
            (Some(entries), _) => archive(entries),
            (None, Some(raw)) => raw.as_bytes().to_vec(),
            (None, None) => Vec::new(),
        };
        let location = stage_archive(&router, &format!("failing/{index}.zip"), bytes).await;
        register(&router, &engine, &blueprint, &location).await;

        let run = run_to_rest(&router, &engine, &blueprint, &workflow).await;
        assert_eq!(
            run.get("State").and_then(Value::as_str),
            Some(STATE_FAILED),
            "case {case}: expected the run to fail, got {run}"
        );
        assert_eq!(
            run.get("WorkflowName"),
            None,
            "case {case}: a failed run reports no workflow, got {run}"
        );
        let reported = run
            .get("ErrorMessage")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            reported.contains(row.reason),
            "case {case}: expected an error message carrying {:?}, got {run}",
            row.reason
        );

        let (status, body) = call(&router, "GetWorkflow", &json!({ "Name": &workflow })).await;
        assert_eq!(
            status, 400,
            "case {case}: a failed run must leave no workflow behind, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains("EntityNotFoundException"),
            "case {case}: expected EntityNotFoundException for the rolled-back workflow, got \
             {body}"
        );
    }
}

/// A location the object store holds nothing at fails the run the same way, carrying what the
/// store answered about it rather than a success nothing deployed.
#[tokio::test]
async fn a_run_whose_archive_is_absent_fails() {
    let (router, engine) = emulator_with_s3();
    create_bucket(&router).await;
    // The registration of a location the store answers for is what pins the FAILED
    // registration; here the object is published and then the BLUEPRINT is registered at a
    // key the bucket does not hold, so the registration is the one a client would get for a
    // live bucket and the RUN is what the absent object fails.
    stage_archive(&router, "present.zip", archive(&[("layout.py", "")])).await;
    register(
        &router,
        &engine,
        "absent-archive-blueprint",
        &format!("s3://{BUCKET}/present.zip"),
    )
    .await;
    // The object is deleted after the registration settled, so the run is what finds it gone.
    s3_client(&router)
        .delete_object()
        .bucket(BUCKET)
        .key("present.zip")
        .send()
        .await
        .expect("the staged archive is deleted");

    let run = run_to_rest(&router, &engine, "absent-archive-blueprint", "bp_absent").await;
    assert_eq!(
        run.get("State").and_then(Value::as_str),
        Some(STATE_FAILED),
        "the run must fail when the archive is gone, got {run}"
    );
    let reported = run
        .get("ErrorMessage")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        reported.contains(&format!("s3://{BUCKET}/present.zip")),
        "the failure must name the location it could not read, got {run}"
    );
}
