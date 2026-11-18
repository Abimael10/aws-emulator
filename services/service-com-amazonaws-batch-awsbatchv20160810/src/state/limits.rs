// SPDX-License-Identifier: CC0-1.0

//! What this emulator will store for one caller, and the answer past it (the module's own
//! rule is stated where it is declared, in `super`).

/// AWS Batch service quota "Compute environments per Region": 50 (adjustable).
pub const MAX_COMPUTE_ENVIRONMENTS: usize = 50;

/// AWS Batch service quota "Job queues per Region": 50 (adjustable).
pub const MAX_JOB_QUEUES: usize = 50;

/// Service environments kept at once. An EMULATOR bound (AWS publishes no quota for
/// this resource): `CreateServiceEnvironment` fills the store and anyone can repeat
/// it, so a create past this answers the modelled `ClientException`. Sized like the
/// other per-Region, name-keyed Batch resources above.
pub const MAX_SERVICE_ENVIRONMENTS: usize = 50;

/// Dependencies one submit may name. AWS Batch documents it on `SubmitJob$dependsOn`:
/// "A job can depend upon a maximum of 20 jobs."
pub const MAX_JOB_DEPENDENCIES: usize = 20;

/// Jobs one bulk `CancelJobs`/`TerminateJobs` request may name, from both operations'
/// own docs: "An array of up to 50 Batch job IDs of the jobs to cancel" /
/// "...to terminate". Both members target a plain string list, so the bound is applied
/// in the handlers.
pub const MAX_BULK_JOBS: usize = 50;

/// Jobs kept at once. An emulator bound, as above: a submit that would exceed it
/// first evicts the jobs that have been terminal longest, and is refused only when
/// there is nothing evictable left.
pub const MAX_JOBS: usize = 500;

/// Consumable resources kept at once. An EMULATOR-CHOSEN ceiling, not an AWS quota:
/// `CreateConsumableResource` is a request anyone can repeat, so the store answers the
/// modelled `ClientException` past this point rather than growing without bound.
pub const MAX_CONSUMABLE_RESOURCES: usize = 500;

/// How many `UpdateConsumableResource` client tokens this store remembers at once, so
/// an identical repeat of one resolves to the update it already applied
/// ([`super::ComAmazonawsBatchAwsbatchv20160810State::consumable_resource_update_for_token`]).
/// An EMULATOR bound: the cache is a replay index anyone can fill, so the oldest entry
/// is dropped past this point rather than letting the store grow without bound.
pub const MAX_CONSUMABLE_RESOURCE_UPDATE_TOKENS: usize = 500;

/// Quota shares kept at once. An EMULATOR bound (AWS publishes no quota for this
/// resource): `CreateQuotaShare` fills the store and anyone can repeat it, so a create
/// past this answers the modelled `ClientException`. Sized like the other per-Region
/// Batch resources above.
pub const MAX_QUOTA_SHARES: usize = 50;

/// Service jobs kept at once, the same emulator bound as [`MAX_JOBS`] and with the
/// same eviction-before-refusal rule
/// ([`super::ComAmazonawsBatchAwsbatchv20160810State::insert_service_job`]).
pub const MAX_SERVICE_JOBS: usize = 500;

/// How many bytes of `SubmitServiceJob$serviceRequestPayload` this service stores for
/// one service job. An EMULATOR bound (AWS publishes none for the member): the payload
/// is a caller-supplied string kept on the record and read once to derive what capacity
/// the job asks for ([`super::requested_capacity`]), so a submit past this answers the
/// modelled `ClientException` rather than letting one request decide how much is stored
/// and parsed. 1 MiB, which is far more than a SageMaker training request needs.
pub const MAX_SERVICE_REQUEST_PAYLOAD_BYTES: usize = 1024 * 1024;

/// How many instance groups of one service job's request are weighed against a service
/// environment's capacity ([`super::requested_capacity`]). An EMULATOR bound: the
/// request chooses how many groups it names, so the derived demand is capped here
/// instead, which is what keeps the admission check constant-sized per record.
pub const MAX_REQUESTED_INSTANCE_GROUPS: usize = 16;

/// How many `capacityLimits` entries ONE service environment publishes. An EMULATOR
/// bound (AWS publishes no quota and the model declares no length): the list is filled
/// by `CreateServiceEnvironment`/`UpdateServiceEnvironment` and is walked whenever a
/// service job is weighed against the environment, so a request past this answers the
/// modelled `ClientException`.
pub const MAX_SERVICE_ENVIRONMENT_CAPACITY_LIMITS: usize = 20;

/// How many `computeEnvironmentOrder` entries ONE job queue carries. An EMULATOR bound
/// (the model declares no length for the list): the list is caller-filled and is walked
/// whenever a job of the queue is placed or the capacity behind the queue is scaled
/// ([`super::ec2_capacity`]), so one request cannot decide how much work a later one
/// costs. Sized at the compute environments this store holds at once
/// ([`MAX_COMPUTE_ENVIRONMENTS`]) — an entry has to name an environment that exists
/// before a queue can be attached to it, so no queue can usefully order more.
pub const MAX_COMPUTE_ENVIRONMENT_ORDER_ENTRIES: usize = MAX_COMPUTE_ENVIRONMENTS;

/// How many `serviceEnvironmentOrder` entries ONE job queue carries, bounded for the
/// same reason as the capacity limits above: the list is caller-filled and is walked
/// when a service job of the queue is placed.
pub const MAX_SERVICE_ENVIRONMENT_ORDER_ENTRIES: usize = 10;

/// Scheduling policies kept at once. An EMULATOR bound (AWS publishes no quota for
/// this resource): `CreateSchedulingPolicy` fills the store and anyone can repeat it,
/// so a create past this answers the modelled `ClientException`. Sized like the other
/// per-Region, name-keyed Batch resources above.
pub const MAX_SCHEDULING_POLICIES: usize = 50;

/// How many tags ONE resource carries at most.
///
/// The model's own bound: every tag map on this service — `TagResource$tags`,
/// `ListTagsForResource$tags`, `CreateJobQueue$tags`, … — targets
/// `com.amazonaws.batch#TagrisTagsMap`, declared `@length(min: 1, max: 50)`
/// (`smithy_query(query="shape", shape="TagrisTagsMap")`). A resource holding more
/// than this could not be reported through that shape at all, so a `TagResource`
/// whose merge would take a resource past it is refused instead.
pub const MAX_TAGS_PER_RESOURCE: usize = 50;

/// Job ids ONE `DescribeJobs` call may name. AWS documents it on the member:
/// `DescribeJobsRequest$jobs` is "A list of up to 100 job IDs"
/// (`smithy_query(query="doc", shape="DescribeJobsRequest", member="jobs")`); the
/// model types it as a plain `StringList`, so the request validator admits any length
/// and the handler applies the bound.
pub const MAX_DESCRIBE_JOBS: usize = 100;
