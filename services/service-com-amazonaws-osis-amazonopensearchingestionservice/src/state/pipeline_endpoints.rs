// SPDX-License-Identifier: CC0-1.0

//! The VPC endpoints a caller creates FOR a pipeline: the records `CreatePipelineEndpoint`
//! mints and `ListPipelineEndpoints` reports.
//!
//! A pipeline endpoint is a child of the pipeline its ARN names — it exists only while that
//! pipeline does, and it carries the VPC configuration the create asked for — so the record
//! below holds the parent's ARN, the endpoint's own id, its status and the ingestion address
//! a client reaches the pipeline at through it. Every member `PipelineEndpoint` models is
//! answered from this record, so the create's answer and every later listing cannot
//! disagree.

use std::time::SystemTime;

use super::{PendingTransition, StoredPipeline};
use crate::ops::types::{PipelineEndpoint, PipelineEndpointVpcOptions};

/// `com.amazonaws.osis#PipelineEndpointStatus` values this emulator moves an endpoint
/// through. The full enum (smithy_query: shape PipelineEndpointStatus) is CREATING, ACTIVE,
/// CREATE_FAILED, DELETING, REVOKING, REVOKED.
pub const ENDPOINT_STATUS_CREATING: &str = "CREATING";
/// The ready state: an endpoint a client can reach the pipeline through.
pub const ENDPOINT_STATUS_ACTIVE: &str = "ACTIVE";

/// How many pipeline endpoints this emulator will hold for one account.
///
/// `CreatePipelineEndpoint` models `LimitExceededException`, and the real service caps the
/// endpoints an account may create; the ceiling here is chosen generously so no realistic
/// workflow meets it, and it is what keeps a caller looping on create from growing this
/// process's memory without bound.
pub const MAX_PIPELINE_ENDPOINTS: usize = 1_000;

/// A VPC endpoint created for a pipeline: what the create was given, plus what the service
/// minted for it.
#[derive(Debug, Clone)]
pub struct StoredPipelineEndpoint {
    pub endpoint_id: String,
    /// The ARN of the pipeline this endpoint was created for — the parent that had to
    /// resolve before this record was minted.
    pub pipeline_arn: String,
    /// A `PipelineEndpointStatus` value.
    pub status: String,
    /// The transition the clock owes, or `None` in a settled status.
    pub pending: Option<PendingTransition>,
    /// The VPC configuration the create asked for, stored as given so a listing reports the
    /// subnets and security groups the caller named.
    pub vpc_options: PipelineEndpointVpcOptions,
    /// The address a client POSTs records to through this endpoint: the ingestion endpoint
    /// of the pipeline it was created for, which is the address this emulator's ingest plane
    /// answers on.
    pub ingest_endpoint_url: Option<String>,
}

impl StoredPipelineEndpoint {
    /// Materialise the transition the clock already owes this endpoint — the one whose due
    /// instant has passed, so it was owed at this instant whether or not anybody called.
    /// This is the one write a reporting operation makes, exactly as it is for a pipeline
    /// ([`StoredPipeline::settle`]).
    pub fn settle(&mut self, now: SystemTime) {
        let Some(pending) = self.pending.clone() else {
            return;
        };
        if now < pending.due_at {
            return;
        }
        self.status = pending.to;
        self.pending = None;
    }

    /// The modelled `PipelineEndpoint` for this record, as the create and
    /// `ListPipelineEndpoints` both report it.
    pub fn to_endpoint(&self) -> PipelineEndpoint {
        PipelineEndpoint {
            pipeline_arn: Some(self.pipeline_arn.clone()),
            endpoint_id: Some(self.endpoint_id.clone()),
            status: Some(self.status.clone()),
            // `VpcId` is absent on purpose: the VPC a subnet belongs to is EC2 state this
            // emulator does not hold, and inventing one would report a value nothing
            // derived. The same reading leaves `VpcEndpoint$VpcId` absent
            // (`crate::state::vpc_endpoints_for`).
            vpc_id: None,
            vpc_options: Some(self.vpc_options.clone()),
            ingest_endpoint_url: self.ingest_endpoint_url.clone(),
        }
    }
}

/// The id of a VPC endpoint this service mints, shaped `vpce-` plus 17 hex digits — the
/// form an Amazon Web Services PrivateLink interface endpoint id carries, and well inside
/// `PipelineEndpointId`'s `@length(3, 512)` and `^[a-zA-Z0-9][a-zA-Z0-9-_]+$`.
///
/// The suffix is a digest of `seed`, so an id is content-derived: stable across reads, the
/// same for the same endpoint however often it is reported, and never a clock reading or a
/// request id.
pub fn interface_endpoint_id(seed: &str) -> String {
    let digest = <::sha2::Sha256 as ::sha2::Digest>::digest(seed.as_bytes());
    let suffix = ::hex::encode(&digest[..9]);
    format!("vpce-{}", &suffix[..17])
}

/// The id of the endpoint a `CreatePipelineEndpoint` mints: derived from the pipeline it is
/// created for, the VPC configuration it was asked for and `ordinal`, the number of
/// endpoints the account already holds for that pipeline.
///
/// The ordinal is what keeps two creates distinct: an account may hold several endpoints for
/// one pipeline, and each one is its own resource with its own id.
pub fn pipeline_endpoint_id(
    pipeline_arn: &str,
    vpc_options: &PipelineEndpointVpcOptions,
    ordinal: usize,
) -> String {
    let subnets = vpc_options.subnet_ids.clone().unwrap_or_default().join(",");
    let groups = vpc_options
        .security_group_ids
        .clone()
        .unwrap_or_default()
        .join(",");
    interface_endpoint_id(&format!(
        "pipeline-endpoint/{pipeline_arn}/{subnets}/{groups}/{ordinal}"
    ))
}

/// The ingestion address an endpoint created for `pipeline` publishes: the pipeline's own
/// ingestion endpoint, which is the one address this emulator's ingest plane answers on, so
/// a client that reaches the pipeline through this endpoint reaches the same plane.
pub fn endpoint_ingest_url(pipeline: &StoredPipeline) -> Option<String> {
    pipeline.ingest_endpoint_urls.first().cloned()
}
