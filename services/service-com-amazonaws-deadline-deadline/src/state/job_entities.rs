// SPDX-License-Identifier: CC0-1.0

//! The job entities a WORKER reads while it runs work: what `BatchGetJobEntity` answers one
//! identifier of its batch with.
//!
//! A worker is handed a session of ONE job (see `super::scheduling`) and then has to read the
//! details of that job to run it: how the job is to be run (`jobDetails`), what has to be
//! synchronised onto the host (`jobAttachmentDetails`), and the Open Job Description
//! declarations of the step and the environments its session actions name (`stepDetails`,
//! `environmentDetails`). Every one of them is read out of state this store already holds —
//! the job, its queue, and the template the create carried — so nothing here is a value
//! invented for the response.
//!
//! Two rules decide what a worker is answered with:
//!
//! * the worker reads entities only of a job it holds an ASSIGNMENT on, which is a session of
//!   that job that has not ended ("the worker must have an assignment on a job to fetch job
//!   details", `smithy_query(query="operation", operation="BatchGetJobEntity")`, member
//!   `farmId`). A job it holds no assignment on DOES NOT EXIST as far as this call is
//!   concerned, and is refused for the identifier that named it as the missing resource it
//!   is, whether this store holds such a job or not: what a worker may read says nothing
//!   about which jobs the account has;
//! * an identifier that names a PART of an assigned job the job does not hold — a step of
//!   another job, an environment no template of it declares — is refused the same way, as
//!   that missing part.
//!
//! Each refusal is per identifier: `BatchGetJobEntity` answers 200 with the entities it could
//! read and one `errors` entry per identifier it could not, so one bad identifier never hides
//! the rest of the batch.
//!
//! LOCK ORDER, and WHY this module is in two halves: a job's template is up to a megabyte of
//! caller-supplied text (`JobTemplate, len 1..1000000`), and parsing is the one step of this
//! call whose cost the caller controls. So [`ComAmazonawsDeadlineDeadlineState::assigned_job_entity_source`]
//! reads everything the four entity kinds need out of the store in one pass, and the builders
//! below — which parse the templates it hands back — are called with the store guard given
//! up, exactly as `super::templates` requires.

use std::collections::HashMap;

use ::aws_smithy_types::Document;
use ::serde_json::Value;

use super::{
    not_found, session_log_group_name, template_document, ComAmazonawsDeadlineDeadlineState,
    ENVIRONMENT_SCOPE_JOB, ENVIRONMENT_SCOPE_STEP, RESOURCE_TYPE_ENVIRONMENT, RESOURCE_TYPE_JOB,
    RESOURCE_TYPE_STEP,
};
use crate::ops::types::{
    Attachments, EnvironmentDetailsEntity, JobAttachmentDetailsEntity, JobAttachmentSettings,
    JobDetailsEntity, JobDetailsJobAttachmentSettings, JobParameter, JobRunAsUser,
    StepDetailsEntity,
};

/// The `JobEntityErrorCode` a worker is answered with for an entity it cannot be given.
///
/// From `smithy_query(query="shape", shape="JobEntityErrorCode")`: `AccessDeniedException`,
/// `InternalServerException`, `ValidationException`, `ResourceNotFoundException`,
/// `MaxPayloadSizeExceeded`, `ConflictException`. A worker's view of the account's jobs is
/// the jobs it was assigned, so a job it holds no assignment on — and any part of one — is
/// reported as a resource that is not there rather than with a code that would say whether it
/// exists for anybody else.
pub const JOB_ENTITY_ERROR_NOT_FOUND: &str = "ResourceNotFoundException";

/// The `JobEntityErrorCode` for an entity whose own declaration cannot be read.
pub const JOB_ENTITY_ERROR_VALIDATION: &str = "ValidationException";

/// Why one identifier of a batch could not be answered with its entity.
///
/// The code and the sentence only; the caller pairs them with the ids the identifier carried,
/// because each entity kind reports its own ids (`JobDetailsError` the job's,
/// `StepDetailsError` the job's and the step's, and so on).
#[derive(Debug, Clone)]
pub struct JobEntityFailure {
    pub code: String,
    pub message: String,
}

impl JobEntityFailure {
    /// The refusal for a part of a job this worker has no entity of, worded with the one
    /// missing-resource sentence every other read of this service answers with
    /// ([`not_found`]).
    fn missing(resource_type: &str, resource_id: &str) -> Self {
        Self {
            code: JOB_ENTITY_ERROR_NOT_FOUND.to_string(),
            message: not_found(resource_type, resource_id).message,
        }
    }

    /// The refusal for an entity whose `schemaVersion` cannot be read: the template that
    /// declares the entity declares no `specificationVersion`, and the member is `@required`,
    /// so there is no version to report it under.
    fn unversioned(template: &str) -> Self {
        Self {
            code: JOB_ENTITY_ERROR_VALIDATION.to_string(),
            message: format!(
                "The {template} template declares no specificationVersion, so this entity has \
                 no schema version."
            ),
        }
    }

    /// The refusal for an entity whose own declaration is not in the template at all.
    fn undeclared(entity: &str, template: &str) -> Self {
        Self {
            code: JOB_ENTITY_ERROR_VALIDATION.to_string(),
            message: format!("The {template} template declares no {entity}."),
        }
    }
}

/// One step of an assigned job, as the entity builders need it.
#[derive(Debug, Clone, Default)]
pub struct JobEntityStep {
    pub step_id: String,
    pub name: String,
    /// The names of the step's own environments, in declared order.
    pub environments: Vec<String>,
}

/// One environment the assigned job's QUEUE holds, with the template that declares it.
#[derive(Debug, Clone, Default)]
pub struct JobEntityQueueEnvironment {
    pub name: String,
    pub template: String,
    pub template_type: String,
}

/// Everything this store holds about one assigned job that its entities are built from.
///
/// Read in ONE pass under the store guard ([`ComAmazonawsDeadlineDeadlineState::assigned_job_entity_source`])
/// so the templates are parsed with that guard given up.
#[derive(Debug, Clone, Default)]
pub struct JobEntitySource {
    pub job_id: String,
    pub queue_id: String,
    pub farm_id: String,
    /// The job's template and the language it is written in, as the create carried them.
    pub template: Option<String>,
    pub template_type: Option<String>,
    pub parameters: Option<HashMap<String, JobParameter>>,
    pub attachments: Option<Attachments>,
    /// The IAM role the job's queue carries, which is the role a worker runs its jobs under.
    pub queue_role_arn: Option<String>,
    pub job_attachment_settings: Option<JobAttachmentSettings>,
    pub job_run_as_user: Option<JobRunAsUser>,
    /// This job's steps, in the order its template declared them — the order the template's
    /// own `steps` list holds, which is how a stored step is matched to its declaration.
    pub steps: Vec<JobEntityStep>,
    /// The names of the environments the job's own template declared, in declared order.
    pub job_environments: Vec<String>,
    /// The environments the job's queue holds, each with its own template.
    pub queue_environments: Vec<JobEntityQueueEnvironment>,
}

impl JobEntitySource {
    /// This job's template as one document tree, or `None` for a job created without one.
    ///
    /// Call it with the store guard given up: this is the parse this module's header is about.
    pub fn document(&self) -> Option<Value> {
        let template = self.template.as_deref()?;
        let template_type = self.template_type.as_deref()?;
        template_document(template, template_type).ok().flatten()
    }
}

impl ComAmazonawsDeadlineDeadlineState {
    /// What the job `job_id` holds for its entities, when `worker_id` holds an assignment on
    /// it under `farm_id`.
    ///
    /// `None` is "this worker has no entities of that job to read": no job of this farm
    /// carries the id, or one does and this worker holds no session of it that has not ended.
    /// The two are one answer on purpose — see this module's header.
    ///
    /// The job is reached across the farm's queues because the path of this call carries no
    /// queue: a worker names the job it was assigned, and the assignment is what says which
    /// queue that job belongs to.
    pub fn assigned_job_entity_source(
        &self,
        farm_id: &str,
        worker_id: &str,
        job_id: &str,
    ) -> Option<JobEntitySource> {
        let farm = self.farm(farm_id)?;
        let (queue, job) = farm
            .queues
            .values()
            .find_map(|queue| queue.jobs.get(job_id).map(|job| (queue, job)))?;
        let assigned = job
            .sessions
            .values()
            .any(|session| session.worker_id == worker_id && session.open());
        if !assigned {
            return None;
        }
        let mut queue_environments: Vec<JobEntityQueueEnvironment> = queue
            .environments
            .values()
            .map(|environment| JobEntityQueueEnvironment {
                name: environment.name.clone(),
                template: environment.template.clone(),
                template_type: environment.template_type.clone(),
            })
            .collect();
        // The queue holds its environments in a map, so they are ordered here for an answer
        // that does not depend on the store's iteration order.
        queue_environments.sort_by(|left, right| left.name.cmp(&right.name));
        Some(JobEntitySource {
            job_id: job.job_id.clone(),
            queue_id: job.queue_id.clone(),
            farm_id: job.farm_id.clone(),
            template: job.template.clone(),
            template_type: job.template_type.clone(),
            parameters: job.parameters.clone(),
            attachments: job.attachments.clone(),
            queue_role_arn: queue.role_arn.clone(),
            job_attachment_settings: queue.job_attachment_settings.clone(),
            job_run_as_user: queue.job_run_as_user.clone(),
            steps: job
                .steps
                .iter()
                .map(|step| JobEntityStep {
                    step_id: step.step_id.clone(),
                    name: step.name.clone(),
                    environments: step.environments.clone(),
                })
                .collect(),
            job_environments: job.environments.clone(),
            queue_environments,
        })
    }
}

/// The refusal every identifier naming a job this worker holds no assignment on earns: the
/// job is not there to read, as it is not there for this worker at all.
pub fn job_entity_not_assigned(job_id: &str) -> JobEntityFailure {
    JobEntityFailure::missing(RESOURCE_TYPE_JOB, job_id)
}

/// The `jobDetails` entity of an assigned job: how the worker is to run it.
///
/// Every member comes from stored state — the job's own parameters, and the queue's
/// attachment settings, run-as user and role, which are what the jobs of that queue run
/// under — except `schemaVersion`, which is the Open Job Description version the job's
/// template declares.
pub fn job_details_entity(
    source: &JobEntitySource,
    document: Option<&Value>,
) -> Result<JobDetailsEntity, JobEntityFailure> {
    let schema_version =
        schema_version(document).ok_or_else(|| JobEntityFailure::unversioned("job"))?;
    Ok(JobDetailsEntity {
        job_id: source.job_id.clone(),
        job_attachment_settings: source.job_attachment_settings.as_ref().map(|settings| {
            JobDetailsJobAttachmentSettings {
                s3_bucket_name: settings.s3_bucket_name.clone(),
                root_prefix: settings.root_prefix.clone(),
            }
        }),
        job_run_as_user: source.job_run_as_user.clone(),
        // The same log destination a session of this job reports to the worker that runs it
        // (`super::Session::log_configuration`), so the group a worker is told to write to is
        // the group its session names.
        log_group_name: session_log_group_name(&source.farm_id, &source.queue_id),
        queue_role_arn: source.queue_role_arn.clone(),
        parameters: source.parameters.clone(),
        schema_version,
        // Both members are optional and this emulator holds nothing to fill them with: it
        // reads no Open Job Description extension declaration out of a template (see
        // `super::job_template`), and it maps no path for a worker — a path mapping rule
        // rewrites a submitter's path to the one the host sees, and nothing here runs on a
        // host.
        extensions: None,
        path_mapping_rules: None,
    })
}

/// The `jobAttachmentDetails` entity of an assigned job: what it has the worker synchronise.
///
/// `attachments` is `@required` on the entity, so a job created without any is a job there is
/// no such entity for.
pub fn job_attachment_details_entity(
    source: &JobEntitySource,
) -> Result<JobAttachmentDetailsEntity, JobEntityFailure> {
    let attachments = source.attachments.clone().ok_or_else(|| JobEntityFailure {
        code: JOB_ENTITY_ERROR_NOT_FOUND.to_string(),
        message: format!("Job {} has no job attachments.", source.job_id),
    })?;
    Ok(JobAttachmentDetailsEntity {
        job_id: source.job_id.clone(),
        attachments,
    })
}

/// The `stepDetails` entity of one step of an assigned job: the step as its template declares
/// it, and the steps it depends on.
pub fn step_details_entity(
    source: &JobEntitySource,
    document: Option<&Value>,
    step_id: &str,
) -> Result<StepDetailsEntity, JobEntityFailure> {
    let index = source
        .steps
        .iter()
        .position(|step| step.step_id == step_id)
        .ok_or_else(|| JobEntityFailure::missing(RESOURCE_TYPE_STEP, step_id))?;
    let schema_version =
        schema_version(document).ok_or_else(|| JobEntityFailure::unversioned("job"))?;
    // One stored step per declared step, in declared order (`super::mint_steps`), so the
    // declaration of the step at this index is the entry at that index of the template's own
    // `steps` list.
    let declared = document
        .and_then(|document| document.get("steps"))
        .and_then(Value::as_array)
        .and_then(|steps| steps.get(index))
        .ok_or_else(|| JobEntityFailure::undeclared("step", "job"))?;
    Ok(StepDetailsEntity {
        job_id: source.job_id.clone(),
        step_id: step_id.to_string(),
        schema_version,
        template: document_of(declared),
        dependencies: declared_dependencies(declared, source),
        // As on `jobDetails`: no extension declaration is read out of a template, and this
        // service resolves no symbol table — the worker resolves the template's references
        // itself.
        extensions: None,
        resolved_symbol_table: None,
    })
}

/// The `environmentDetails` entity one `EnvironmentId` of an assigned job names: the
/// environment as the template that declares it declares it.
///
/// The id spells which template that is (`smithy_query(query="shape",
/// shape="EnvironmentId")`: `JOB:<jobId>:<name>` or `STEP:<stepId>:<name>`): a job-scoped
/// environment is one of the job's queue's, whose own template declares it, or one the job
/// template's `jobEnvironments` declares; a step-scoped one is declared by that step's
/// `stepEnvironments`.
pub fn environment_details_entity(
    source: &JobEntitySource,
    document: Option<&Value>,
    environment_id: &str,
) -> Result<EnvironmentDetailsEntity, JobEntityFailure> {
    let missing = || JobEntityFailure::missing(RESOURCE_TYPE_ENVIRONMENT, environment_id);
    let (scope, rest) = environment_id.split_once(':').ok_or_else(missing)?;
    let (owner_id, name) = rest.split_once(':').ok_or_else(missing)?;
    let held = match scope {
        ENVIRONMENT_SCOPE_JOB if owner_id == source.job_id => {
            queue_or_job_environment(source, document, name)
        }
        ENVIRONMENT_SCOPE_STEP => step_environment(source, document, owner_id, name),
        _ => None,
    };
    // An id naming an environment this job runs under no matter what, and the declaration of
    // it read out of the template that declares it: the first is the missing resource a
    // worker is told about, the second a template this emulator could not read the entity
    // out of.
    let (schema_version, declared) = held.ok_or_else(missing)??;
    Ok(EnvironmentDetailsEntity {
        job_id: source.job_id.clone(),
        environment_id: environment_id.to_string(),
        schema_version,
        template: declared,
        extensions: None,
        resolved_symbol_table: None,
    })
}

/// The declaration of a JOB-scoped environment `name`: the queue's environment of that name,
/// whose own template declares it, or the one the job template's `jobEnvironments` declares.
///
/// The outer `None` is "this job runs under no environment of that name", which is the
/// missing resource the caller reports; the inner `Err` is an environment whose declaration
/// cannot be read.
fn queue_or_job_environment(
    source: &JobEntitySource,
    document: Option<&Value>,
    name: &str,
) -> Option<Result<(String, Document), JobEntityFailure>> {
    if let Some(environment) = source
        .queue_environments
        .iter()
        .find(|environment| environment.name == name)
    {
        // A queue environment is a template of its own (`specificationVersion:
        // environment-2023-09`), which describes ONE environment under its `environment`
        // key — the node `super::environment_template_name` reads that environment's name
        // out of.
        let environment_document =
            template_document(&environment.template, &environment.template_type)
                .ok()
                .flatten();
        return Some(
            match (
                schema_version(environment_document.as_ref()),
                environment_document
                    .as_ref()
                    .and_then(|document| document.get("environment")),
            ) {
                (Some(version), Some(declared)) => Ok((version, document_of(declared))),
                (None, _) => Err(JobEntityFailure::unversioned("environment")),
                (_, None) => Err(JobEntityFailure::undeclared("environment", "environment")),
            },
        );
    }
    if !source.job_environments.iter().any(|held| held == name) {
        return None;
    }
    Some(declared_environment(document, document, "jobEnvironments", name))
}

/// The declaration of a STEP-scoped environment: the `stepEnvironments` entry of the step
/// `step_id` of this job that carries `name`.
fn step_environment(
    source: &JobEntitySource,
    document: Option<&Value>,
    step_id: &str,
    name: &str,
) -> Option<Result<(String, Document), JobEntityFailure>> {
    let index = source.steps.iter().position(|step| step.step_id == step_id)?;
    if !source.steps[index]
        .environments
        .iter()
        .any(|held| held == name)
    {
        return None;
    }
    let declared_step = document
        .and_then(|document| document.get("steps"))
        .and_then(Value::as_array)
        .and_then(|steps| steps.get(index));
    // The step's environments are declared inside the JOB template, so the version the
    // entity is reported under is that template's.
    Some(declared_environment(
        document,
        declared_step,
        "stepEnvironments",
        name,
    ))
}

/// The entry of `list` under `parent` that carries `name`, reported under the schema version
/// `versioned` declares.
fn declared_environment(
    versioned: Option<&Value>,
    parent: Option<&Value>,
    list: &str,
    name: &str,
) -> Result<(String, Document), JobEntityFailure> {
    let version =
        schema_version(versioned).ok_or_else(|| JobEntityFailure::unversioned("job"))?;
    let declared = parent
        .and_then(|parent| parent.get(list))
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
        })
        .ok_or_else(|| JobEntityFailure::undeclared("environment with that name", "job"))?;
    Ok((version, document_of(declared)))
}

/// The step ids one declared step depends on, in declared order.
///
/// An Open Job Description step declares its dependencies by step NAME
/// (`dependencies: [{dependsOn: <name>}]`), and the entity reports them as `StepId`s
/// (`DependenciesList`), so each name is resolved to the id of the step of THIS job that
/// carries it. A name no step of the job carries names no dependency the job holds, and is
/// left out rather than reported as an id that resolves to nothing.
fn declared_dependencies(declared: &Value, source: &JobEntitySource) -> Vec<String> {
    let Some(dependencies) = declared.get("dependencies").and_then(Value::as_array) else {
        return Vec::new();
    };
    dependencies
        .iter()
        .filter_map(|dependency| dependency.get("dependsOn").and_then(Value::as_str))
        .filter_map(|name| {
            source
                .steps
                .iter()
                .find(|step| step.name == name)
                .map(|step| step.step_id.clone())
        })
        .collect()
}

/// The Open Job Description version a template declares, as the entity's `schemaVersion`.
fn schema_version(document: Option<&Value>) -> Option<String> {
    document?
        .get("specificationVersion")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// A node of a parsed template as the `Document` the entity carries it in.
///
/// A template's `template` member is modeled as a document, so the declaration is answered
/// as the tree it was written as rather than as text. The recursion is bounded before this
/// runs: the document was built by `super::template_document`, which refuses a YAML template
/// nesting past `super::MAX_TEMPLATE_DEPTH` and leaves a JSON one to `serde_json`'s own
/// recursion limit.
fn document_of(value: &Value) -> Document {
    match value {
        Value::Null => Document::Null,
        Value::Bool(flag) => Document::Bool(*flag),
        Value::Number(number) => Document::Number(json_number(number)),
        Value::String(text) => Document::String(text.clone()),
        Value::Array(items) => Document::Array(items.iter().map(document_of).collect()),
        Value::Object(members) => Document::Object(
            members
                .iter()
                .map(|(name, member)| (name.clone(), document_of(member)))
                .collect(),
        ),
    }
}

/// A JSON number as the `Document` number of the same value.
fn json_number(number: &::serde_json::Number) -> ::aws_smithy_types::Number {
    use ::aws_smithy_types::Number;
    if let Some(unsigned) = number.as_u64() {
        Number::PosInt(unsigned)
    } else if let Some(signed) = number.as_i64() {
        Number::NegInt(signed)
    } else {
        Number::Float(number.as_f64().unwrap_or_default())
    }
}
