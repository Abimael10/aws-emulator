// SPDX-License-Identifier: CC0-1.0

//! The harness (`/harnesses/`): the record `CreateHarness` persists and every harness
//! operation reports from, the endpoints created on it, and the sentences this family
//! refuses a missing or duplicated one in.
//!
//! Lives beside the stores in [`super`] rather than in any one `ops/` file, so the create,
//! the update and every read of a harness cannot drift from each other.

use super::*;

/// How many harnesses this emulator will hold.
///
/// `CreateHarness` models `ServiceQuotaExceededException` (HTTP 402) for
/// exceeding the real service's per-account quota. This number is NOT a fidelity
/// claim about that quota: it is the emulator's own ceiling, set generously so no
/// realistic workflow meets it, so that a caller looping on create cannot grow
/// this process's memory without bound.
pub const MAX_HARNESSES: usize = 1_000;

/// How long a freshly created harness stays `CREATING` before the clock owes it
/// `READY`.
///
/// AWS documents no duration for this transition, so the span is chosen: short
/// enough to keep a client script quick, long enough that a caller acting
/// straight after the call still observes the transient status.
pub const HARNESS_SETTLE_DELAY: Duration = Duration::from_millis(500);

/// The version a created harness carries. `HarnessVersion` is
/// `^([1-9][0-9]{0,4})$`, so versions start at 1; only `UpdateHarness` publishes
/// another one, and it is not implemented, so a stored harness is at version 1.
pub const FIRST_HARNESS_VERSION: u32 = 1;

/// How many endpoints ONE harness will hold.
///
/// `CreateHarnessEndpoint` models `ServiceQuotaExceededException` (HTTP 402) for
/// exceeding the real service's quota. The number is NOT a fidelity claim about
/// that quota: it is the emulator's own ceiling, set generously so no realistic
/// workflow meets it, so that a caller looping on create cannot grow this
/// process's memory without bound.
pub const MAX_ENDPOINTS_PER_HARNESS: usize = 1_000;

/// How long a freshly created harness endpoint stays `CREATING` before the clock
/// owes it `READY`.
///
/// AWS documents no duration for this transition, so the span is chosen: long
/// enough that a caller acting straight after the call still observes the
/// transient status, short enough to keep a client script quick. An endpoint is
/// served BY its harness, so the span runs from the instant the HARNESS itself is
/// ready ([`HarnessRecord::settles_at`]), not from the create call.
pub const HARNESS_ENDPOINT_SETTLE_DELAY: Duration = Duration::from_secs(2);

/// `HarnessTruncationStrategy` for "do not truncate". The enum's full value list
/// is `sliding_window | summarization | none`
/// (`smithy_query shape=HarnessTruncationStrategy`), and `Harness.truncation` is
/// `@required` on every response while `CreateHarness`'s is optional, so a
/// harness created without one reports the strategy that truncates nothing.
pub const HARNESS_TRUNCATION_NONE: &str = "none";

/// `NetworkMode` for a runtime reachable over the public internet. The enum's
/// full value list is `PUBLIC | VPC` (`smithy_query shape=NetworkMode`), and
/// `VPC` cannot be defaulted into: `VpcConfig` makes `securityGroups` and
/// `subnets` `@required`, so only the caller can ask for it.
pub const NETWORK_MODE_PUBLIC: &str = "PUBLIC";

/// The modelled not-found error for a harness this account does not hold.
///
/// This control plane words a missing harness in terms of the agent it runs:
/// "Agent with name <harnessId> not found." — the sentence around the
/// identifier is fixed, and the identifier itself is the one the request named.
/// Shared by the handlers rather than written per operation, so one harness
/// cannot be reported missing in two different sentences by two operations.
pub fn harness_not_found(harness_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(format!("Agent with name {harness_id} not found.")),
    }
}

/// The modelled not-found error for a harness endpoint the request addressed.
///
/// An operation whose route addresses ONE endpoint carries both identifiers the
/// endpoint is named by — the harness and the endpoint name, both `@required`
/// `@httpLabel` — so what it could not find is that endpoint, and the sentence is
/// at endpoint granularity: the endpoint name and the harness it was looked for
/// on, both taken from the request path. The parent harness missing is the same
/// answer: the lookup that failed is still the endpoint's, so an operation
/// addressing an endpoint never reports the harness's own sentence
/// ([`harness_not_found`], which belongs to the operations that address a
/// harness). Shared by the handlers rather than written per operation, so one
/// endpoint cannot be reported missing in two different sentences by two
/// operations, and the asking operation's verb never appears.
pub fn harness_endpoint_not_found(endpoint_name: &str, harness_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(format!("Endpoint {endpoint_name} not found for harness {harness_id}.")),
    }
}

/// How this service words the conflict a SECOND endpoint under one name on one
/// harness is: the name the request asked for and the harness that already holds
/// an endpoint under it.
///
/// The name is what an endpoint's route and its ARN address it by, so one harness
/// holds one endpoint per name. The sentence takes the form every create of this
/// service refuses a duplicate name in (`CreateHarness`, `CreateGateway`,
/// `CreateMemory` and the rest): the resource, the name the request carried, and
/// the resource that already holds it.
pub fn harness_endpoint_name_taken(endpoint_name: &str, harness_id: &str) -> String {
    format!("an endpoint named {endpoint_name} already exists on harness {harness_id}")
}

/// How this service words [`MAX_ENDPOINTS_PER_HARNESS`] being met: the harness
/// the request named and the ceiling it is at, in the form every ceiling of this
/// service is reported in (`CreateGatewayTarget`'s targets per gateway,
/// `UpdateAgentRuntime`'s versions per runtime).
pub fn harness_endpoint_quota_met(harness_id: &str) -> String {
    format!("harness {harness_id} already holds the maximum of {MAX_ENDPOINTS_PER_HARNESS} endpoints")
}

/// One harness version's key in the version-ordered map a listing pages over:
/// the version number zero-padded to the five digits `HarnessVersion` allows
/// (pattern `^([1-9][0-9]{0,4})$`), so ascending KEY order is ascending VERSION
/// order for every version the wire can carry — which is what makes the shared
/// page walk's "resume strictly after the key the last page ended on" cursor
/// sound for a listing of versions.
fn harness_version_key(version: u32) -> String {
    format!("{version:05}")
}

/// One harness: everything the caller configured, plus what the service minted
/// for it and the status the clock drives.
///
/// `CreateHarness`, `GetHarness` and `DeleteHarness` all report the SAME
/// `Harness` structure, so it is rendered from this one record
/// ([`Self::harness`]) rather than in any handler: a member the create reports
/// and the read does not would be a difference no stored state justifies.
#[derive(Debug, Clone)]
pub struct HarnessRecord {
    pub harness_id: String,
    pub harness_arn: String,
    pub harness_name: String,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// The version this harness reports, as a number; rendered by
    /// [`Self::version_string`].
    pub version: u32,
    /// The status until [`settles_at`](Self::settles_at) passes.
    pub transient_status: String,
    /// The status from [`settles_at`](Self::settles_at) onwards.
    pub settled_status: String,
    /// The instant the transient status is owed its transition. Stored, never
    /// slept on: a read materialises the transition the clock already made due.
    pub settles_at: SystemTime,
    pub execution_role_arn: String,
    /// The model the harness runs with, as the caller configured it. Optional on
    /// `CreateHarness` and on `UpdateHarness`, and AWS publishes no default model
    /// id, so a harness created without one holds none.
    pub model: Option<HarnessModelConfiguration>,
    pub system_prompt: Vec<HarnessSystemContentBlock>,
    pub tools: Vec<HarnessTool>,
    pub skills: Vec<HarnessSkill>,
    pub allowed_tools: Vec<String>,
    pub truncation: HarnessTruncationConfiguration,
    /// The AgentCore Runtime environment this harness runs in: the identity the
    /// service minted for it, with the lifecycle, network and filesystem
    /// configuration the request asked for (or the documented defaults).
    pub environment: HarnessAgentCoreRuntimeEnvironment,
    pub environment_artifact: Option<HarnessEnvironmentArtifact>,
    pub environment_variables: BTreeMap<String, String>,
    pub authorizer_configuration: Option<AuthorizerConfiguration>,
    pub memory: Option<HarnessMemoryConfiguration>,
    pub hooks: Vec<HarnessHook>,
    pub max_iterations: Option<i32>,
    pub max_tokens: Option<i32>,
    pub timeout_seconds: Option<i32>,
    pub tags: BTreeMap<String, String>,
    /// The endpoints created on this harness, keyed by `endpointName` — the one
    /// identifier every harness-endpoint route addresses an endpoint by, and what
    /// its ARN ends in. An endpoint is reached only THROUGH its harness, so it is
    /// held here rather than in a map of its own: a harness and its endpoints
    /// cannot drift apart, and deleting the harness takes them with it.
    pub endpoints: BTreeMap<String, HarnessEndpointRecord>,
}

impl HarnessRecord {
    /// The status this harness reports at `now`: the transient one until the
    /// transition the clock owes comes due, the settled one afterwards.
    pub fn status_at(&self, now: SystemTime) -> &str {
        if now < self.settles_at {
            &self.transient_status
        } else {
            &self.settled_status
        }
    }

    /// The version number as the wire renders it (`HarnessVersion`, pattern
    /// `^([1-9][0-9]{0,4})$`).
    pub fn version_string(&self) -> String {
        self.version.to_string()
    }

    /// Whether a `harnessVersion` query value names the version this harness
    /// holds. `None` asks for the latest, which it always holds; an unparseable
    /// or unknown version names no stored version, which the caller answers with
    /// `ResourceNotFoundException`.
    pub fn holds_version(&self, requested: Option<&str>) -> bool {
        match requested {
            None => true,
            Some(raw) => raw.parse::<u32>().is_ok_and(|version| version == self.version),
        }
    }

    /// This harness's `HarnessVersionSummary` for the version it holds, as a
    /// listing of versions reports it at `now`. Rendered from the same record
    /// [`Self::harness`] reads, so a summary and a full read cannot disagree on
    /// the identity, the version or the status of one harness.
    pub fn version_summary(&self, now: SystemTime) -> HarnessVersionSummary {
        HarnessVersionSummary {
            harness_id: self.harness_id.clone(),
            harness_name: self.harness_name.clone(),
            arn: self.harness_arn.clone(),
            harness_version: self.version_string(),
            status: self.status_at(now).to_string(),
            created_at: timestamp(self.created_at),
            updated_at: timestamp(self.updated_at),
            // Reported only for a version in a failed status; one this emulator
            // drives reaches READY, which has no failure.
            failure_reason: None,
        }
    }

    /// Every version this harness holds, keyed by [`harness_version_key`] so the
    /// map's own order IS ascending version order.
    ///
    /// A harness gains one version per publish: a create publishes
    /// [`FIRST_HARNESS_VERSION`] and only an update publishes another, so a
    /// stored harness holds exactly the version in [`Self::version`] — the
    /// listing reports the versions this record persists rather than a range of
    /// numbers nothing was ever stored for.
    pub fn version_summaries(&self, now: SystemTime) -> BTreeMap<String, HarnessVersionSummary> {
        [(harness_version_key(self.version), self.version_summary(now))]
            .into_iter()
            .collect()
    }

    /// This harness as every harness operation reports it, at `now`.
    pub fn harness(&self, now: SystemTime) -> Harness {
        self.harness_with_status(self.status_at(now).to_string())
    }

    /// This harness reported with `status` instead of the one the clock resolves
    /// — the delete path, which reports the harness it just removed as
    /// `DELETING`.
    pub fn harness_with_status(&self, status: String) -> Harness {
        Harness {
            harness_id: self.harness_id.clone(),
            harness_name: self.harness_name.clone(),
            arn: self.harness_arn.clone(),
            status,
            harness_version: Some(self.version_string()),
            execution_role_arn: self.execution_role_arn.clone(),
            created_at: timestamp(self.created_at),
            updated_at: timestamp(self.updated_at),
            model: self.model.clone(),
            system_prompt: self.system_prompt.clone(),
            tools: self.tools.clone(),
            skills: self.skills.clone(),
            allowed_tools: self.allowed_tools.clone(),
            truncation: self.truncation.clone(),
            environment: HarnessEnvironmentProvider::agentCoreRuntimeEnvironment(
                self.environment.clone(),
            ),
            environment_artifact: self.environment_artifact.clone(),
            environment_variables: (!self.environment_variables.is_empty()).then(|| {
                self.environment_variables
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            }),
            authorizer_configuration: self.authorizer_configuration.clone(),
            memory: self.memory.clone(),
            hooks: (!self.hooks.is_empty()).then(|| self.hooks.clone()),
            max_iterations: self.max_iterations,
            max_tokens: self.max_tokens,
            timeout_seconds: self.timeout_seconds,
            // Reported only for a harness in a failed status; one this emulator
            // drives reaches READY, which has no failure.
            failure_reason: None,
        }
    }
}

/// One endpoint of a harness: the stable name callers invoke, the harness
/// version it serves, and the status the clock drives.
///
/// `CreateHarnessEndpoint` and every later read of the endpoint report the SAME
/// `HarnessEndpoint` structure, so it is rendered from this one record
/// ([`Self::endpoint`]) rather than in any handler: a member the create reports
/// and a read does not would be a difference no stored state justifies. The
/// harness's own id and name are NOT copied in — they are read from the harness
/// that holds the record, so a rename cannot leave two spellings behind.
#[derive(Debug, Clone)]
pub struct HarnessEndpointRecord {
    pub endpoint_name: String,
    pub endpoint_arn: String,
    pub description: Option<String>,
    /// The harness version this endpoint points to, as a number; rendered like
    /// [`HarnessRecord::version_string`] does.
    pub target_version: u32,
    /// The version the endpoint is SERVING until [`settles_at`](Self::settles_at)
    /// passes — `None` for an endpoint that has never finished a transition, which
    /// serves nothing yet.
    pub live_version: Option<u32>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// The status until [`settles_at`](Self::settles_at) passes.
    pub transient_status: String,
    /// The status from [`settles_at`](Self::settles_at) onwards.
    pub settled_status: String,
    /// The instant the transient status is owed its transition. Stored, never
    /// slept on: a read materialises the transition the clock already made due.
    pub settles_at: SystemTime,
    pub tags: BTreeMap<String, String>,
}

impl HarnessEndpointRecord {
    /// The status this endpoint reports at `now`: the transient one until the
    /// transition the clock owes comes due, the settled one afterwards.
    pub fn status_at(&self, now: SystemTime) -> &str {
        if now < self.settles_at {
            &self.transient_status
        } else {
            &self.settled_status
        }
    }

    /// The version this endpoint is SERVING at `now`.
    ///
    /// An endpoint serves the version it points to only once the transition onto it has
    /// come due; until then it serves whatever it served before, which is nothing at all
    /// for an endpoint that was only just created — exactly the difference the model
    /// documents between `liveVersion` and `targetVersion`.
    fn live_version(&self, now: SystemTime) -> Option<String> {
        if now < self.settles_at {
            self.live_version.map(|version| version.to_string())
        } else {
            Some(self.target_version.to_string())
        }
    }

    /// This endpoint as every harness-endpoint operation reports it at `now`, for
    /// the harness that holds it.
    pub fn endpoint(&self, harness_id: &str, harness_name: &str, now: SystemTime) -> HarnessEndpoint {
        self.endpoint_with_status(harness_id, harness_name, self.status_at(now).to_string(), now)
    }

    /// This endpoint reported with `status` instead of the one the clock resolves
    /// — the delete path, which reports the endpoint it just removed as
    /// `DELETING`.
    pub fn endpoint_with_status(
        &self,
        harness_id: &str,
        harness_name: &str,
        status: String,
        now: SystemTime,
    ) -> HarnessEndpoint {
        HarnessEndpoint {
            harness_id: harness_id.to_string(),
            harness_name: harness_name.to_string(),
            endpoint_name: self.endpoint_name.clone(),
            arn: self.endpoint_arn.clone(),
            status,
            created_at: timestamp(self.created_at),
            updated_at: timestamp(self.updated_at),
            live_version: self.live_version(now),
            target_version: Some(self.target_version.to_string()),
            description: self.description.clone(),
            // Reported only for an endpoint whose create or update failed; one
            // this emulator drives reaches READY, which has no failure.
            failure_reason: None,
        }
    }
}
