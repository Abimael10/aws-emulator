// SPDX-License-Identifier: CC0-1.0

//! The Neptune ML resources this engine holds, and what the ML endpoints answer from them.
//!
//! Neptune ML is a chain of resources: a data-processing job prepares the graph's data, a
//! model-training job trains a model from that, a model-transform job produces model
//! artifacts, and an inference endpoint SERVES the model one of those jobs produced. The
//! chain is what the endpoint operations are decided from: an id that names no resource
//! this engine holds cannot be served, so the operation that was handed it is refused with
//! the modelled `MLResourceNotFoundException` rather than answered.
//!
//! Each registry below is written by the operation that MINTS its resource and read by the
//! ones that report on it. [`MlRegistry::endpoints`] is written by `CreateMLEndpoint` and
//! read by `GetMLEndpoint`; the job registries are read here and written by the
//! `StartML*Job` operations, each of which has its own stub item — so an endpoint can only
//! be created for a job this engine really holds, which is exactly the lookup the 404 above
//! comes from.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::ops::types::{
    ConstraintViolationException, MLResourceNotFoundException, MlConfigDefinition,
    MlResourceDefinition, PreconditionsFailedException,
};

/// What Neptune answers a request naming a machine-learning resource it does not hold.
///
/// RECORDED BEHAVIOUR: the message is wholly fixed text with no variable part — the
/// refusal names no id, so the id a caller sent is not echoed back to it.
pub const ML_RESOURCE_NOT_FOUND_MESSAGE: &str =
    "Invalid ID provided. Please check whether a resource with the given ID exists.";

/// The `MLResourceNotFoundException` payload for an id that names no ML resource this
/// engine holds. `code` carries the HTTP status, which is what the member's own
/// documentation says it is.
pub fn ml_resource_not_found() -> MLResourceNotFoundException {
    MLResourceNotFoundException {
        detailed_message: ML_RESOURCE_NOT_FOUND_MESSAGE.to_string(),
        request_id: super::new_request_id(),
    }
}

/// How many inference endpoints this engine holds.
///
/// Every create is a caller-driven write to the registry, so the registry has a ceiling:
/// one that is reached is refused rather than letting a stream of creates grow this
/// engine's memory without end. The same ceiling as the graph's own bounds elsewhere in
/// this crate — a number an emulated cluster will not meet in use, and a bound either way.
pub const MAX_ML_ENDPOINTS: usize = 64;

/// The `ConstraintViolationException` payload for a create this engine has no room for:
/// the registry already holds [`MAX_ML_ENDPOINTS`] endpoints.
pub fn ml_endpoint_limit() -> ConstraintViolationException {
    ConstraintViolationException {
        detailed_message: format!("This engine holds at most {MAX_ML_ENDPOINTS} inference endpoints"),
        request_id: super::new_request_id(),
    }
}

/// The `PreconditionsFailedException` payload for an endpoint asked to serve the model of a
/// job that has not finished: there is no model to serve yet, so the message names the job
/// and the state it is in.
pub fn ml_job_not_completed(job_id: &str, status: &str) -> PreconditionsFailedException {
    PreconditionsFailedException {
        detailed_message: format!(
            "Supplied job {} is {}, and an inference endpoint serves the model of a {} job",
            super::echoed(job_id),
            super::echoed(status),
            ML_JOB_STATUS_COMPLETED
        ),
        request_id: super::new_request_id(),
    }
}

/// The `PreconditionsFailedException` payload for a create whose `id` an endpoint already
/// holds. The message names the id and the member that makes it an update instead.
pub fn ml_endpoint_exists(endpoint_id: &str) -> PreconditionsFailedException {
    PreconditionsFailedException {
        detailed_message: format!(
            "Supplied id {} already names an inference endpoint. \
             Set update to true to update it.",
            super::echoed(endpoint_id)
        ),
        request_id: super::new_request_id(),
    }
}

/// How long an inference endpoint takes to come into service.
///
/// An endpoint is provisioned, so it is NOT in service the instant the create returns:
/// this is the span the clock owes that transition, and the record stores the instant it
/// falls due rather than anything sleeping for it. A caller reading the endpoint straight
/// after creating it therefore sees it being created, as it would against Neptune.
pub const ML_ENDPOINT_PROVISIONING: Duration = Duration::from_secs(3);

/// `GetMLEndpointOutput$status` while the endpoint is being provisioned, and once it is
/// serving.
///
/// Source: the status of the SageMaker inference endpoint a Neptune ML endpoint is, which
/// is what this operation reports.
pub const ML_ENDPOINT_STATUS_CREATING: &str = "Creating";
pub const ML_ENDPOINT_STATUS_IN_SERVICE: &str = "InService";

/// The status a model-training or model-transform job carries once it has finished, which
/// is the only state a job an endpoint can serve the model of is in:
/// `CreateMLEndpointInput$mlModelTrainingJobId` is documented as "the job Id of the
/// COMPLETED model-training job".
pub const ML_JOB_STATUS_COMPLETED: &str = "Completed";

/// `CreateMLEndpointInput$instanceType`'s documented default — the member's own
/// documentation states "The default is `ml.m5.xlarge`".
pub const DEFAULT_ML_INSTANCE_TYPE: &str = "ml.m5.xlarge";

/// `CreateMLEndpointInput$instanceCount`'s documented default — the member's own
/// documentation states "The default is 1".
pub const DEFAULT_ML_INSTANCE_COUNT: i32 = 1;

/// The AWS service whose resources a Neptune ML endpoint is made of: Neptune ML serves a
/// model through a SageMaker inference endpoint, so the ARNs it reports are SageMaker's.
const SAGEMAKER_SERVICE: &str = "sagemaker";

/// One Neptune ML job this engine holds: a data-processing, model-training or
/// model-transform job, as the registry it lives in says.
#[derive(Debug, Clone)]
pub struct MlJobRecord {
    /// The job id, which is the key this record is stored under and the id another
    /// operation names the job by.
    pub id: String,
    pub arn: String,
    pub created_at: SystemTime,
    /// How far the job has got. [`ML_JOB_STATUS_COMPLETED`] is the state a job has to be
    /// in for an endpoint to serve the model it produced.
    pub status: String,
    /// Where the job wrote what it produced, when it has.
    pub output_location: Option<String>,
}

impl MlJobRecord {
    /// Whether the job has finished, which is what an endpoint for it requires.
    pub fn is_completed(&self) -> bool {
        self.status.eq_ignore_ascii_case(ML_JOB_STATUS_COMPLETED)
    }

    /// This job as an [`MlResourceDefinition`], the shape the ML reads report a resource
    /// through.
    pub fn definition(&self) -> MlResourceDefinition {
        MlResourceDefinition {
            name: Some(self.id.clone()),
            arn: Some(self.arn.clone()),
            status: Some(self.status.clone()),
            output_location: self.output_location.clone(),
            failure_reason: None,
            cloudwatch_log_url: None,
        }
    }
}

/// One inference endpoint this engine holds, as `CreateMLEndpoint` minted it.
#[derive(Debug, Clone)]
pub struct MlEndpointRecord {
    /// The endpoint's unique id — `GetMLEndpoint`'s `id` label, and the key this record is
    /// stored under.
    pub id: String,
    /// The endpoint's own ARN, which is the SageMaker endpoint's.
    pub arn: String,
    /// The ARN of the endpoint configuration the endpoint was created with.
    pub config_arn: String,
    /// The id of the model-training or model-transform job whose model this endpoint
    /// serves: the resource the endpoint points at, which had to exist for it to be
    /// created.
    pub source_job_id: String,
    pub created_at: SystemTime,
    /// When the endpoint comes into service. Until then a read reports it as being
    /// created, which is the state Neptune reports a freshly created endpoint in.
    pub in_service_at: SystemTime,
    pub instance_type: String,
    pub instance_count: i32,
    /// The model type the endpoint serves, when the request named one.
    pub model_name: Option<String>,
}

impl MlEndpointRecord {
    /// The endpoint's status as of `now`: being created until the provisioning span the
    /// create started has passed, in service afterwards.
    pub fn status(&self, now: SystemTime) -> &'static str {
        if now < self.in_service_at {
            ML_ENDPOINT_STATUS_CREATING
        } else {
            ML_ENDPOINT_STATUS_IN_SERVICE
        }
    }

    /// `CreateMLEndpointOutput$creationTimeInMillis`: when the endpoint was created, in
    /// milliseconds since the epoch.
    pub fn created_at_millis(&self) -> i64 {
        let millis = self
            .created_at
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since_epoch| since_epoch.as_millis());
        i64::try_from(millis.min(i64::MAX as u128)).unwrap_or(i64::MAX)
    }

    /// `GetMLEndpointOutput$endpoint`: the endpoint resource itself.
    pub fn definition(&self, now: SystemTime) -> MlResourceDefinition {
        MlResourceDefinition {
            name: Some(self.id.clone()),
            arn: Some(self.arn.clone()),
            status: Some(self.status(now).to_string()),
            // An inference endpoint SERVES a model rather than writing anything, so it has
            // no output location; it has not failed, so it has no failure reason; and this
            // emulator collects no logs, so there is no log URL to report.
            output_location: None,
            failure_reason: None,
            cloudwatch_log_url: None,
        }
    }

    /// `GetMLEndpointOutput$endpointConfig`: the configuration the endpoint runs with.
    pub fn config(&self) -> MlConfigDefinition {
        MlConfigDefinition {
            name: Some(self.id.clone()),
            arn: Some(self.config_arn.clone()),
        }
    }
}

/// The Neptune ML resources this engine holds, each kind keyed by its own id.
///
/// A `BTreeMap` per kind is what gives the ML list operations a stable order over the ids.
#[derive(Debug, Default)]
pub struct MlRegistry {
    pub data_processing_jobs: BTreeMap<String, MlJobRecord>,
    pub model_training_jobs: BTreeMap<String, MlJobRecord>,
    pub model_transform_jobs: BTreeMap<String, MlJobRecord>,
    pub endpoints: BTreeMap<String, MlEndpointRecord>,
}

impl MlRegistry {
    /// The model-training or model-transform job `id` names, whichever registry holds it.
    ///
    /// `CreateMLEndpoint` takes either kind of job id for the one model the endpoint is to
    /// serve, so one lookup answers for both.
    pub fn model_job(&self, id: &str) -> Option<&MlJobRecord> {
        self.model_training_jobs
            .get(id)
            .or_else(|| self.model_transform_jobs.get(id))
    }
}

/// The ARN of the inference endpoint named `name`:
/// `arn:aws:sagemaker:<region>:<account>:endpoint/<name>`.
pub fn ml_endpoint_arn(region: &str, name: &str) -> String {
    ::engine::emulator_arn(SAGEMAKER_SERVICE, region, &format!("endpoint/{name}"))
}

/// The ARN of the endpoint configuration named `name`:
/// `arn:aws:sagemaker:<region>:<account>:endpoint-config/<name>`.
pub fn ml_endpoint_config_arn(region: &str, name: &str) -> String {
    ::engine::emulator_arn(
        SAGEMAKER_SERVICE,
        region,
        &format!("endpoint-config/{name}"),
    )
}

/// The id a `CreateMLEndpoint` that named none gets: `CreateMLEndpointInput$id`'s own
/// documentation states "The default is an autogenerated timestamped name", so the name is
/// built from the instant the endpoint was created.
pub fn default_ml_endpoint_id(created_at: SystemTime) -> String {
    let millis = created_at
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_millis());
    format!("neptune-ml-endpoint-{millis}")
}
