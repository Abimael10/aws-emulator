// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that reports `CREATE_COMPLETE` has really created the
//! `AWS::Glue::Trigger` its template declares — in AWS Glue, in the stack's own Region, under
//! the physical id the stack publishes — and that trigger really starts what it declares.
//!
//! The rule under test: **a stack reports a resource as created only where the service that
//! owns it created it, through that service's own public API, in the stack's Region and under
//! the physical id the stack publishes — so the handle a stack's output carries addresses a
//! resource that service really holds, and deleting the stack releases it there.** For a
//! trigger the handle is its NAME ("Ref returns the trigger name"), so right after the create
//! waiter returns, `glue GetTrigger` answers 200 for the name the stack exported, with the
//! `Type`, `State`, `Schedule` and `Actions` the declaration carries, rather than AWS Glue
//! answering `EntityNotFoundException` for a trigger its own stack just reported complete.
//!
//! Two crates have to agree for that to hold (CloudFormation makes the calls, AWS Glue serves
//! them), so the suite lives here: a crate-local test can only ever see one side of it. The
//! router, the two wires over it and the stack clock are the ones the sibling Glue-job suite
//! mounts ([`crate::cloudformation_provisions_glue_jobs`],
//! [`crate::cloudformation_stack_clock`]), not another copy.
//!
//! The rows differ along what the rule generalises over: the `TriggerType` declared
//! (`SCHEDULED` on a cron expression, `ON_DEMAND`, `CONDITIONAL` on a predicate), whether the
//! create starts the trigger, whether its actions
//! name the job through `Ref` or literally, which optional properties it carries (description,
//! tags, action arguments and timeout), the template's wire format (JSON or YAML) and the
//! Region the stack is deployed in.
//!
//! The update half, the deletion half and the FIRING half are rows of the same rule: a changed
//! declaration reaches AWS Glue through that service's own update, deleting the stack deletes
//! the trigger, and a CFN-declared `SCHEDULED` trigger is a real armed trigger of that service
//! — so when the instant its schedule names falls due, it begins a run of the job its actions
//! name, tagged with its own name. (The firing rule itself, over the forms a schedule can
//! take, is pinned in the Glue crate's own
//! `tests/all/scheduled_trigger_firing.rs`.)

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_provisions_glue_jobs::{cloudformation, error_code, glue};
use crate::cloudformation_stack_clock::settle_the_stacks;
use crate::query_wire::{element, opt_element, outputs_of, physical_ids_of};

/// The logical ids every row uses, so each read is of the one resource the rule is about.
const JOB_LOGICAL_ID: &str = "GlueJob";
const SECOND_JOB_LOGICAL_ID: &str = "GlueWatchedJob";
const TRIGGER_LOGICAL_ID: &str = "GlueTrigger";

/// The output key every row's template publishes the trigger's name through, which is how a
/// client reaches the deployed trigger at all.
const NAME_OUTPUT: &str = "GlueTriggerName";

/// The longest trigger name AWS Glue accepts (`CreateTrigger.Name` is `NameString`,
/// `@length(min: 1, max: 255)` in that service's own model).
const MAX_TRIGGER_NAME: usize = 255;

/// How long a case waits for a stack to reach a terminal status while its clock is driven
/// past every due instant on each look ([`settle_the_stacks`]).
///
/// Generous: the condition is looked at again until it holds, so this bounds only a stack that
/// never settles at all, and no case depends on how fast the host runs.
const SETTLED_DEADLINE: Duration = Duration::from_secs(60);

/// How long a case waits for a firing the clock owes to show up in the job's run history.
///
/// Generous for the same reason: the run is polled for until it is there.
const FIRING_DEADLINE: Duration = Duration::from_secs(60);

/// How long one look at a condition waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// An emulator with CloudFormation and AWS Glue mounted on ONE engine, as the assembled
/// emulator mounts them.
///
/// IAM is deliberately NOT mounted: none of the templates below declares a role, and the job
/// a trigger acts on is addressed by name alone, so the only services the rule needs are the
/// one that deploys and the one that owns.
fn build_router_and_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// The role every declared job runs as. A literal ARN: the suite is about the trigger, and no
/// IAM is mounted for a role resource to be created in.
const JOB_ROLE: &str = "arn:aws:iam::000000000000:role/glue-trigger-suite";

/// A Region that is NOT `region`, for the half of the rule that says a trigger is reachable
/// only through the endpoint of the Region its stack was deployed in.
fn foreign_region(region: &str) -> &'static str {
    if region == "us-east-2" {
        "us-east-1"
    } else {
        "us-east-2"
    }
}

/// Drive the stack clock and read `stack_name` back until it rests in a terminal status, and
/// answer the `DescribeStacks` body it rested in.
async fn settled_stack(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    region: &str,
    stack_name: &str,
) -> String {
    let deadline = SystemTime::now() + SETTLED_DEADLINE;
    loop {
        settle_the_stacks(router, engine, case).await;
        let (status, described) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body \
             {described}"
        );
        let stack_status = opt_element(&described, "StackStatus").unwrap_or_default();
        if stack_status.ends_with("_COMPLETE") || stack_status.ends_with("_FAILED") {
            return described;
        }
        assert!(
            SystemTime::now() < deadline,
            "case {case:?}: the stack never left {stack_status:?}; last read {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The members `GetTrigger` must report for a row's trigger: a JSON pointer into the
/// `Trigger` document, and the value the declaration states for it.
type ExpectedMember = (&'static str, fn() -> Value);

/// One row of the table: a template, and what the rule says the trigger it declares must then
/// be in AWS Glue.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the trigger is created in.
    region: &'static str,
    /// The trigger name the template states, which is the physical id the stack publishes.
    trigger_name: &'static str,
    /// The template body, in the form the row writes it (JSON or YAML).
    template: fn() -> String,
    /// The members `GetTrigger` must report for the trigger.
    expected_members: &'static [ExpectedMember],
}

/// The JSON template of the recorded shape: a pythonshell job and a `SCHEDULED` trigger over
/// it, started on creation, with tags and a description.
fn scheduled_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a Glue job and the schedule that runs it",
        "Resources": {
            JOB_LOGICAL_ID: {
                "Type": "AWS::Glue::Job",
                "Properties": {
                    "Name": "gts-scheduled-job",
                    "Role": JOB_ROLE,
                    "Command": {
                        "Name": "pythonshell",
                        "PythonVersion": "3",
                        "ScriptLocation": "s3://gts-scripts/report.py"
                    },
                    "GlueVersion": "3.0",
                    "MaxCapacity": 0.0625
                }
            },
            TRIGGER_LOGICAL_ID: {
                "Type": "AWS::Glue::Trigger",
                "Properties": {
                    "Name": "gts-scheduled-trigger",
                    "Description": "every five minutes",
                    "Type": "SCHEDULED",
                    "Schedule": "cron(0/5 * * * ? *)",
                    "StartOnCreation": true,
                    "Actions": [{ "JobName": { "Ref": JOB_LOGICAL_ID } }],
                    "Tags": { "Owner": "analytics" }
                }
            }
        },
        "Outputs": {
            NAME_OUTPUT: { "Value": { "Ref": TRIGGER_LOGICAL_ID } }
        }
    })
    .to_string()
}

/// The YAML template: an `ON_DEMAND` trigger, which the create cannot start, whose action
/// names the job literally and carries that action's own arguments and timeout.
fn on_demand_template() -> String {
    [
        "AWSTemplateFormatVersion: '2010-09-09'",
        "Resources:",
        "  GlueJob:",
        "    Type: AWS::Glue::Job",
        "    Properties:",
        "      Name: gts-on-demand-job",
        "      Role: arn:aws:iam::000000000000:role/glue-trigger-suite",
        "      Command:",
        "        Name: pythonshell",
        "        ScriptLocation: s3://gts-scripts/on-demand.py",
        "  GlueTrigger:",
        "    Type: AWS::Glue::Trigger",
        "    Properties:",
        "      Name: gts-on-demand-trigger",
        "      Type: ON_DEMAND",
        "      Actions:",
        "        - JobName: gts-on-demand-job",
        "          Timeout: 30",
        "          Arguments:",
        "            '--mode': batch",
        "Outputs:",
        "  GlueTriggerName:",
        "    Value: !Ref GlueTrigger",
    ]
    .join("\n")
}

/// The JSON template of a `CONDITIONAL` trigger: it watches one job's runs and starts
/// another, which is how a Glue deployment chains work.
fn conditional_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            SECOND_JOB_LOGICAL_ID: {
                "Type": "AWS::Glue::Job",
                "Properties": {
                    "Name": "gts-watched-job",
                    "Role": JOB_ROLE,
                    "Command": { "Name": "pythonshell", "ScriptLocation": "s3://gts/a.py" }
                }
            },
            JOB_LOGICAL_ID: {
                "Type": "AWS::Glue::Job",
                "Properties": {
                    "Name": "gts-started-job",
                    "Role": JOB_ROLE,
                    "Command": { "Name": "pythonshell", "ScriptLocation": "s3://gts/b.py" }
                }
            },
            TRIGGER_LOGICAL_ID: {
                "Type": "AWS::Glue::Trigger",
                "Properties": {
                    "Name": "gts-conditional-trigger",
                    "Type": "CONDITIONAL",
                    "StartOnCreation": true,
                    "Predicate": {
                        "Conditions": [{
                            "LogicalOperator": "EQUALS",
                            "JobName": { "Ref": SECOND_JOB_LOGICAL_ID },
                            "State": "SUCCEEDED"
                        }]
                    },
                    "Actions": [{ "JobName": { "Ref": JOB_LOGICAL_ID } }]
                }
            }
        },
        "Outputs": {
            NAME_OUTPUT: { "Value": { "Ref": TRIGGER_LOGICAL_ID } }
        }
    })
    .to_string()
}

/// The rows: three templates spanning the trigger types a declaration can state, whether the
/// create starts the trigger, the two wire formats and two Regions.
///
/// `State` is what the service decides from `StartOnCreation` and the type: a trigger the
/// create started is `ACTIVATED`, and one it could not start ("True is not supported for
/// ON_DEMAND triggers", `CreateTriggerRequest$StartOnCreation`) is `CREATED`.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a SCHEDULED trigger on a cron expression, started on creation, in JSON",
            region: "us-east-1",
            trigger_name: "gts-scheduled-trigger",
            template: scheduled_template,
            expected_members: &[
                ("/Type", || json!("SCHEDULED")),
                ("/State", || json!("ACTIVATED")),
                ("/Schedule", || json!("cron(0/5 * * * ? *)")),
                ("/Description", || json!("every five minutes")),
                ("/Actions/0/JobName", || json!("gts-scheduled-job")),
            ],
        },
        Row {
            case: "an ON_DEMAND trigger carrying its action's own arguments, in YAML",
            region: "us-east-2",
            trigger_name: "gts-on-demand-trigger",
            template: on_demand_template,
            expected_members: &[
                ("/Type", || json!("ON_DEMAND")),
                ("/State", || json!("CREATED")),
                ("/Actions/0/JobName", || json!("gts-on-demand-job")),
                ("/Actions/0/Timeout", || json!(30)),
                ("/Actions/0/Arguments", || json!({ "--mode": "batch" })),
            ],
        },
        Row {
            case: "a CONDITIONAL trigger watching one job and starting another, in JSON",
            region: "us-east-1",
            trigger_name: "gts-conditional-trigger",
            template: conditional_template,
            expected_members: &[
                ("/Type", || json!("CONDITIONAL")),
                ("/State", || json!("ACTIVATED")),
                ("/Predicate/Conditions/0/JobName", || json!("gts-watched-job")),
                ("/Predicate/Conditions/0/State", || json!("SUCCEEDED")),
                ("/Actions/0/JobName", || json!("gts-started-job")),
            ],
        },
    ]
}

/// Every row: the name the stack reports for its `AWS::Glue::Trigger` is a trigger AWS Glue
/// really holds in the stack's Region, defined as the template declared it — and deleting the
/// stack releases it there.
#[tokio::test]
async fn a_stack_provisions_the_glue_trigger_its_template_declares() {
    for (index, row) in rows().into_iter().enumerate() {
        let (router, engine) = build_router_and_engine();
        let case = row.case;
        let region = row.region;
        let stack_name = format!("glue-trigger-stack-{index}");
        let template = (row.template)();

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let described = settled_stack(&router, &engine, case, region, &stack_name).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
        );

        let (status, resources) = cloudformation(
            &router,
            region,
            "DescribeStackResources",
            &[("StackName", &stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             body {resources}"
        );
        let trigger_name = physical_ids_of(&resources)
            .get(TRIGGER_LOGICAL_ID)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: no {TRIGGER_LOGICAL_ID} resource in {resources}")
            });

        // "Ref returns the trigger name": the physical id IS the name the template asked for,
        // and the stack publishes it through its output.
        assert_eq!(
            trigger_name, row.trigger_name,
            "case {case:?}: expected the physical id to be the declared name {:?}, got \
             {trigger_name:?}",
            row.trigger_name
        );
        assert!(
            !trigger_name.is_empty() && trigger_name.chars().count() <= MAX_TRIGGER_NAME,
            "case {case:?}: expected a name inside AWS Glue's own NameString bound of \
             {MAX_TRIGGER_NAME}, got {trigger_name:?}"
        );
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get(NAME_OUTPUT),
            Some(&trigger_name),
            "case {case:?}: expected output {NAME_OUTPUT:?} to be the trigger name \
             {trigger_name:?}, got {outputs:?}"
        );

        // CloudFormation's own resource index resolves that name back to the stack.
        let (status, owned) = cloudformation(
            &router,
            region,
            "DescribeStackResources",
            &[("PhysicalResourceId", &trigger_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources on the physical id, \
             got {status} with body {owned}"
        );
        assert_eq!(
            element(&owned, "StackName"),
            stack_name,
            "case {case:?}: expected the physical id to resolve to {stack_name:?}, got {owned}"
        );

        // And AWS Glue itself resolves that name to the trigger the template declared.
        let (status, read) =
            glue(&router, region, "GetTrigger", &json!({ "Name": trigger_name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetTrigger on the name the stack reported, \
             got {status} with body {read}"
        );
        let trigger = read.get("Trigger").cloned().unwrap_or(Value::Null);
        assert_eq!(
            trigger.get("Name").and_then(Value::as_str),
            Some(trigger_name.as_str()),
            "case {case:?}: expected Trigger.Name to be {trigger_name:?}, got {trigger}"
        );
        for (pointer, expected) in row.expected_members {
            let expected = expected();
            let actual = trigger.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected Trigger{pointer} to be {expected}, got {actual} in \
                 {trigger}"
            );
        }

        // The trigger exists in the stack's Region ALONE: AWS Glue is regional, so another
        // Region's endpoint holds no trigger under that name.
        let elsewhere = foreign_region(region);
        let (status, absent) =
            glue(&router, elsewhere, "GetTrigger", &json!({ "Name": trigger_name })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from GetTrigger in {elsewhere}, got {status} \
             with body {absent}"
        );
        assert_eq!(
            error_code(&absent),
            "EntityNotFoundException",
            "case {case:?}: expected EntityNotFoundException in {elsewhere}, got {absent}"
        );

        // The Region's own listing names it, so a client enumerating that Region's triggers
        // sees the deployed one.
        let (status, listed) = glue(&router, region, "ListTriggers", &json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListTriggers, got {status} with {listed}"
        );
        assert!(
            listed["TriggerNames"]
                .as_array()
                .is_some_and(|names| names.iter().any(|name| name == &json!(trigger_name))),
            "case {case:?}: expected ListTriggers in {region} to name {trigger_name:?}, got \
             {listed}"
        );

        // Deleting the stack releases the trigger in the service that holds it.
        let (status, deleted) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body \
             {deleted}"
        );
        let deadline = SystemTime::now() + SETTLED_DEADLINE;
        loop {
            settle_the_stacks(&router, &engine, case).await;
            let (status, gone) =
                glue(&router, region, "GetTrigger", &json!({ "Name": trigger_name })).await;
            if status == 400 && error_code(&gone) == "EntityNotFoundException" {
                break;
            }
            assert!(
                SystemTime::now() < deadline,
                "case {case:?}: deleting the stack must release the trigger in AWS Glue; \
                 GetTrigger still answered {status} with {gone}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// A CHANGED declaration reaches AWS Glue through that service's own update: the trigger the
/// stack already created carries the new definition, under the name it was created with.
#[tokio::test]
async fn a_changed_trigger_declaration_is_applied_through_the_owning_service() {
    let case = "a redeclared schedule, description and action";
    let region = "us-east-1";
    let (router, engine) = build_router_and_engine();
    let stack_name = "glue-trigger-update-stack";

    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", &scheduled_template()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = settled_stack(&router, &engine, case, region, stack_name).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
    );

    // The same resource, with the three members `TriggerUpdate` carries restated.
    let changed = scheduled_template()
        .replace("cron(0/5 * * * ? *)", "cron(15 12 * * ? *)")
        .replace("every five minutes", "every day at 12:15 UTC");
    let (status, body) = cloudformation(
        &router,
        region,
        "UpdateStack",
        &[("StackName", stack_name), ("TemplateBody", &changed)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
    let described = settled_stack(&router, &engine, case, region, stack_name).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("UPDATE_COMPLETE"),
        "case {case:?}: expected the stack to reach UPDATE_COMPLETE, got {described}"
    );

    let (status, read) = glue(
        &router,
        region,
        "GetTrigger",
        &json!({ "Name": "gts-scheduled-trigger" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetTrigger, got {status} with body {read}"
    );
    let trigger = read.get("Trigger").cloned().unwrap_or(Value::Null);
    for (pointer, expected) in [
        ("/Schedule", json!("cron(15 12 * * ? *)")),
        ("/Description", json!("every day at 12:15 UTC")),
        // The members the update carries no value for are the create's own, so the trigger is
        // still the SCHEDULED one the stack created rather than a replacement.
        ("/Type", json!("SCHEDULED")),
        ("/State", json!("ACTIVATED")),
        ("/Actions/0/JobName", json!("gts-scheduled-job")),
    ] {
        let actual = trigger.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected Trigger{pointer} to be {expected}, got {actual} in \
             {trigger}"
        );
    }
}

/// The trigger a stack declares is a real ARMED trigger of AWS Glue: when the instant its
/// schedule names falls due, it begins a run of the job its actions name, and that run reports
/// the trigger as the `TriggerName` that began it.
///
/// The firing is owed by the CLOCK at an instant the caller's own cron expression names, which
/// for the recorded `cron(0/5 * * * ? *)` is minutes away — so the case brings the instant the
/// create armed forward, exactly as it drives the stack's own clock rather than waiting for
/// it, and then polls the job's run history for what the firing began. WHICH instant a
/// schedule names is the Glue crate's own suite's row to assert.
#[tokio::test]
async fn a_stack_declared_scheduled_trigger_starts_the_job_it_names() {
    let case = "a CFN-declared SCHEDULED trigger whose next firing has fallen due";
    let region = "us-east-1";
    let (router, engine) = build_router_and_engine();
    let stack_name = "glue-trigger-firing-stack";

    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", &scheduled_template()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = settled_stack(&router, &engine, case, region, stack_name).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
    );

    // Nothing is due yet: the next instant the expression names is minutes ahead, so the job
    // has no run at all.
    let (status, runs) = glue(
        &router,
        region,
        "GetJobRuns",
        &json!({ "JobName": "gts-scheduled-job" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetJobRuns, got {status} with body {runs}"
    );
    assert_eq!(
        runs["JobRuns"].as_array().map_or(0, Vec::len),
        0,
        "case {case:?}: a schedule whose next instant is ahead has begun no run, got {runs}"
    );

    // The instant the create armed, brought forward: what the trigger was armed WITH is the
    // Glue suite's row; what this one asserts is that the armed trigger really fires.
    {
        let state = service_com_amazonaws_glue_awsglue::state::service_state(&*engine)
            .expect("the Glue store is reachable");
        let mut held = state.lock();
        let trigger = held
            .trigger_mut(region, "gts-scheduled-trigger")
            .expect("the stack created the trigger");
        assert!(
            trigger.scheduled_firing_due_at.is_some(),
            "case {case:?}: the create must have armed the trigger's schedule, got {trigger:?}"
        );
        trigger.scheduled_firing_due_at = Some(SystemTime::now());
    }

    let deadline = SystemTime::now() + FIRING_DEADLINE;
    let begun = loop {
        let (status, runs) = glue(
            &router,
            region,
            "GetJobRuns",
            &json!({ "JobName": "gts-scheduled-job" }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetJobRuns, got {status} with body {runs}"
        );
        let begun = runs["JobRuns"].as_array().cloned().unwrap_or_default();
        if !begun.is_empty() {
            break begun;
        }
        assert!(
            SystemTime::now() < deadline,
            "case {case:?}: the firing the clock owed must begin a run of the job the trigger \
             names; GetJobRuns still answered {runs}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    assert_eq!(
        begun.len(),
        1,
        "case {case:?}: one instant of the schedule begins ONE run, got {begun:?}"
    );
    assert_eq!(
        begun[0].get("TriggerName").and_then(Value::as_str),
        Some("gts-scheduled-trigger"),
        "case {case:?}: the run must name the trigger that began it, got {:?}",
        begun[0]
    );
    assert_eq!(
        begun[0].get("JobName").and_then(Value::as_str),
        Some("gts-scheduled-job"),
        "case {case:?}: the run must be a run of the job the action names, got {:?}",
        begun[0]
    );
}
