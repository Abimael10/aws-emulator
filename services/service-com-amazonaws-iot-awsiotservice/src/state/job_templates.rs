// SPDX-License-Identifier: CC0-1.0

//! The account's JOB TEMPLATES: the record one holds, what every reader of one reports,
//! and how a create states that an id is taken.
//!
//! A job template is the reusable shape of a job — the document a job deploys together
//! with the rollout, abort, retry, timeout and pre-signed-URL configuration it is
//! deployed under — which `CreateJob$jobTemplateArn` ("The ARN of the job template used
//! to create the job") then names. The id is the template's identity: the model binds it
//! as the `@httpLabel` of `/job-templates/{jobTemplateId}` on every single-template
//! operation (`smithy_query(query="operation",operation="CreateJobTemplate")`) and
//! documents it as "A unique identifier for the job template", which is why the store is
//! a `BTreeMap` keyed by it — the map IS the store, and `ListJobTemplates` is
//! `@paginated`, so a page boundary is only meaningful over a stable order.
//!
//! One record carries everything a reader hands back, so `DescribeJobTemplate` after
//! `CreateJobTemplate` reports the created template member for member: the reader clones
//! what the writer stored ([`RegisteredJobTemplate::to_description`]) and the two cannot
//! drift.
//!
//! A template created FROM a job (`CreateJobTemplate$jobArn`, "The ARN of the job to use
//! as the basis for the job template") takes that job's document, so the create resolves
//! the ARN against the shared jobs store ([`super::job_store`]) and keeps the document
//! the job holds; the template is a record of its own from then on, and the job it was
//! based on may be cancelled or deleted without changing it.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::ops::types::{
    AbortConfig, DescribeJobTemplateOutput, JobExecutionsRetryConfig, JobExecutionsRolloutConfig,
    JobTemplateSummary, MaintenanceWindow, PresignedUrlConfig, TimeoutConfig,
};

/// How many job templates this emulator keeps for the account.
///
/// AWS publishes a service quota on an account's job templates; the exact figure is not
/// in the model, so this is the emulator's own ceiling on a store a caller can grow — the
/// same reason [`super::MAX_POLICIES`] and [`super::MAX_STREAMS`] exist — and
/// `CreateJobTemplate` past it refuses with the modelled `LimitExceededException` rather
/// than growing without bound.
pub const MAX_JOB_TEMPLATES: usize = 100;

/// How many tags one job template may carry: the same documented IoT tagging limit
/// [`super::MAX_POLICY_TAGS`] records, for the same reason.
pub const MAX_JOB_TEMPLATE_TAGS: usize = super::MAX_POLICY_TAGS;

/// The ARN of a job template: `arn:aws:iot:<region>:<account>:jobtemplate/<jobTemplateId>`.
///
/// `jobtemplate/` is the resource type AWS IoT's job-template ARNs use — it is what
/// `CreateJobTemplate$jobTemplateArn` reports and what an `iot:DescribeJobTemplate`
/// policy statement names a template by; the namespace and account segments are the ones
/// [`super::thing_arn`] documents.
pub fn job_template_arn(region: &str, job_template_id: &str) -> String {
    ::emulator_server::emulator_arn("iot", region, &format!("jobtemplate/{job_template_id}"))
}

/// How this service refuses a `jobArn` that is no IoT job ARN at all.
///
/// FIXED text about the ARN's SYNTAX, interpolating nothing, for the reason
/// [`super::TARGET_ARN_SYNTAX_MESSAGE`] states: the value is not an
/// `arn:<partition>:iot:<region>:<account>:job/<jobId>`, which is a statement about the
/// FORM the member takes rather than about the value one request happened to send.
pub const JOB_ARN_SYNTAX_MESSAGE: &str = "Job ARN not in proper syntax.";

/// The `jobId` a `jobArn` names, or `None` when the value is no IoT job ARN.
///
/// The ACCOUNT and REGION segments are deliberately not compared, for the reason
/// [`super::parse_job_target`] documents: a caller may legitimately name a job by an ARN
/// minted in another region, and the jobs store is keyed by id.
pub fn job_id_of_job_arn(job_arn: &str) -> Option<&str> {
    // 6 fields: arn:<partition>:<service>:<region>:<account-id>:<resource>. The resource
    // keeps any colon of its own, so the split is bounded at 6.
    let fields: Vec<&str> = job_arn.splitn(6, ':').collect();
    let ["arn", _partition, "iot", _region, _account, resource] = fields.as_slice() else {
        return None;
    };
    let Some(("job", job_id)) = resource.split_once('/') else {
        return None;
    };
    (!job_id.is_empty()).then_some(job_id)
}

/// One job template in the store, as this service persists it.
///
/// Every configuration member is kept as the MODELLED shape the request carried, so a
/// reader hands back what the writer sent rather than a reassembled copy that could drift
/// from it.
#[derive(Debug, Clone)]
pub struct RegisteredJobTemplate {
    /// The id the template was created under; also its key in the store.
    pub job_template_id: String,
    /// `arn:aws:iot:<region>:<account>:jobtemplate/<jobTemplateId>`, minted on create.
    pub job_template_arn: String,
    /// `CreateJobTemplate$description` — "A description of the job document", which the
    /// model makes `@required`, so every record has one.
    pub description: String,
    /// The job document the template deploys: the request's own, or the one the job named
    /// by `jobArn` holds.
    pub document: Option<String>,
    /// `CreateJobTemplate$documentSource` — "An S3 link, or S3 object URL, to the job
    /// document", when the template names its document that way instead.
    pub document_source: Option<String>,
    /// `CreateJobTemplate$presignedUrlConfig`, as the request carried it.
    pub presigned_url_config: Option<PresignedUrlConfig>,
    /// `CreateJobTemplate$jobExecutionsRolloutConfig`, as the request carried it.
    pub job_executions_rollout_config: Option<JobExecutionsRolloutConfig>,
    /// `CreateJobTemplate$abortConfig`, as the request carried it.
    pub abort_config: Option<AbortConfig>,
    /// `CreateJobTemplate$timeoutConfig`, as the request carried it — or the in-progress
    /// timeout of the job this template was based on.
    pub timeout_config: Option<TimeoutConfig>,
    /// `CreateJobTemplate$jobExecutionsRetryConfig`, as the request carried it.
    pub job_executions_retry_config: Option<JobExecutionsRetryConfig>,
    /// `CreateJobTemplate$maintenanceWindows`, as the request carried it.
    pub maintenance_windows: Option<Vec<MaintenanceWindow>>,
    /// `CreateJobTemplate$destinationPackageVersions`, as the request carried it.
    pub destination_package_versions: Option<Vec<String>>,
    /// `Tag$Key` -> `Tag$Value` for the tags the create request carried. Tags are
    /// "metadata that can be used to manage the job template" rather than part of what a
    /// reader of the template describes, so they are not reported by the reader.
    pub tags: BTreeMap<String, Option<String>>,
    /// When the template was created — `DescribeJobTemplate$createdAt`, "The time, in
    /// seconds since the epoch, when the job template was created".
    pub created_at: SystemTime,
}

impl RegisteredJobTemplate {
    /// A newly created template: the minted identity, the required description and the
    /// instant of the create. Every configuration member is filled by the writer with
    /// struct-update syntax, so a member this service does not yet honour cannot be
    /// silently defaulted here.
    pub fn created(
        job_template_id: String,
        job_template_arn: String,
        description: String,
        created_at: SystemTime,
    ) -> Self {
        Self {
            job_template_id,
            job_template_arn,
            description,
            document: None,
            document_source: None,
            presigned_url_config: None,
            job_executions_rollout_config: None,
            abort_config: None,
            timeout_config: None,
            job_executions_retry_config: None,
            maintenance_windows: None,
            destination_package_versions: None,
            tags: BTreeMap::new(),
            created_at,
        }
    }

    /// The record as `DescribeJobTemplate` reports it — every member of the stored
    /// record, so the read and the create that wrote it cannot drift.
    ///
    /// The configuration structures whose own members are all optional
    /// (`presignedUrlConfig`, `jobExecutionsRolloutConfig`, `timeoutConfig`) are reported
    /// for every template, empty for one created without them, under the rule
    /// [`super::reported_configuration`] states; `abortConfig` and
    /// `jobExecutionsRetryConfig` have a `@required` criteria list and so have no empty
    /// form, and the LIST members are reported only from a record that holds entries.
    pub fn to_description(&self) -> DescribeJobTemplateOutput {
        DescribeJobTemplateOutput {
            job_template_arn: Some(self.job_template_arn.clone()),
            job_template_id: Some(self.job_template_id.clone()),
            description: Some(self.description.clone()),
            document_source: self.document_source.clone(),
            document: self.document.clone(),
            created_at: Some(::aws_smithy_types::DateTime::from(self.created_at)),
            presigned_url_config: super::reported_configuration(
                self.presigned_url_config.clone(),
            ),
            job_executions_rollout_config: super::reported_configuration(
                self.job_executions_rollout_config.clone(),
            ),
            abort_config: self.abort_config.clone(),
            timeout_config: super::reported_configuration(self.timeout_config.clone()),
            job_executions_retry_config: self.job_executions_retry_config.clone(),
            maintenance_windows: self.maintenance_windows.clone(),
            destination_package_versions: self.destination_package_versions.clone(),
        }
    }

    /// The record as a LISTING entry — `JobTemplateSummary`, "An object that contains
    /// information about the job template".
    ///
    /// Projected from the SAME stored record the reader describes, so a listing entry and
    /// the describe of the template it names cannot drift: every member the summary
    /// declares (the ARN, the id, the description and the instant of the create) is the one
    /// [`Self::to_description`] reports.
    pub fn summary(&self) -> JobTemplateSummary {
        JobTemplateSummary {
            job_template_arn: Some(self.job_template_arn.clone()),
            job_template_id: Some(self.job_template_id.clone()),
            description: Some(self.description.clone()),
            created_at: Some(::aws_smithy_types::DateTime::from(self.created_at)),
        }
    }
}
