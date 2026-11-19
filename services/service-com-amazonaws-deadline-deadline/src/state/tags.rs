// SPDX-License-Identifier: CC0-1.0

//! The resource a `resourceArn` names, and the tags that resource carries.
//!
//! Every tagging call of this service addresses its resource the same way: by the full ARN
//! of a resource of this service's own namespace, whose resource field spells the SAME
//! nesting the model's paths spell (`farm/<farmId>/queue/<queueId>/job/<jobId>` for the
//! job at `/2023-10-12/farms/{farmId}/queues/{queueId}/jobs/{jobId}`). Resolving that ARN
//! lives here rather than in each handler, so a tag write, a tag read and a tag removal
//! cannot disagree about which resource an ARN names, or about how a value that names none
//! is refused.
//!
//! The ARN itself is read by the workspace's one ARN parse, [`::service_common::arn`], so
//! this service decides for itself only what is its own: which vendor its ARNs carry,
//! which of the resources it stores a tagging call can reach, and how each refusal is
//! worded.
//!
//! The six fields of an ARN do not all identify a resource here: nothing in this store is
//! scoped by partition, Region or account — a farm created through any endpoint of this
//! emulator is read back through every other — and this emulator presents one account
//! (`::engine::emulator_account_id`). So the RESOURCE field is what a tagging call is
//! resolved on, and the vendor field is what says the value addresses this API at all.

use std::collections::HashMap;

use super::{
    field_validation_failed, not_found, ComAmazonawsDeadlineDeadlineState, RESOURCE_TYPE_BUDGET,
    RESOURCE_TYPE_FARM, RESOURCE_TYPE_FLEET, RESOURCE_TYPE_JOB, RESOURCE_TYPE_QUEUE,
    RESOURCE_TYPE_WORKER,
};

/// The vendor field of this service's own resource ARNs: the `arnNamespace` the model
/// publishes for it (`smithy_query(query="service_info")` reports
/// `awsService.arnNamespace: deadline`).
pub const ARN_NAMESPACE: &str = "deadline";

/// The resource type an ARN of a monitor names, spelled the way the API path spells that
/// resource (`/2023-10-12/monitors`).
///
/// A monitor is taggable — `CreateMonitor` takes `tags` — but this emulator stores no
/// monitor yet (`CreateMonitor` is still a scaffold), so an ARN naming one reaches no
/// stored resource.
pub const RESOURCE_TYPE_MONITOR: &str = "monitor";

/// The resource type an ARN of a license endpoint names, spelled the way the API path
/// spells it (`/2023-10-12/license-endpoints`). Taggable and not stored yet, exactly as
/// [`RESOURCE_TYPE_MONITOR`].
pub const RESOURCE_TYPE_LICENSE_ENDPOINT: &str = "license-endpoint";

/// The member a refused `resourceArn` arrived on, as the boundary validator names a member
/// at fault.
const RESOURCE_ARN_MEMBER: &str = "resourceArn";

/// The member a refused tag map arrived on.
const TAGS_MEMBER: &str = "tags";

/// How many tags one resource holds at once.
///
/// AWS's tagging quota for a resource is 50 tags, the same figure for every service that
/// takes a `tags` map, and this model constrains `Tags` with no `@length` of its own
/// (`smithy_query(query="shape", shape="Tags")` reports a bare `map<String, String>`), so
/// the documented quota is the bound. It is also what keeps repeated tag writes from
/// growing one resource's state without limit: a write that would leave a resource holding
/// more than this is refused and stores nothing.
pub const MAX_TAGS_PER_RESOURCE: usize = 50;

/// The longest tag key this service stores, in characters.
///
/// AWS's tagging restrictions bound a tag key at 128 characters and a tag value at 256,
/// the same figures for every service that takes a `tags` map, and this model constrains
/// neither with a `@length` of its own. Together with [`MAX_TAGS_PER_RESOURCE`] they are
/// what makes the tags one resource holds bounded in BYTES as well as in count, however
/// many tag writes a caller makes.
pub const MAX_TAG_KEY_LENGTH: usize = 128;

/// The longest tag value this service stores, in characters. See [`MAX_TAG_KEY_LENGTH`].
pub const MAX_TAG_VALUE_LENGTH: usize = 256;

/// A stored resource a tagging call reaches, as a `resourceArn` resolved to it.
///
/// The variants are the resources this emulator stores that the model gives a `tags`
/// member — `CreateFarm`, `CreateQueue`, `CreateFleet`, `CreateJob`, `CreateWorker` and
/// `CreateBudget` each take one — each holding the ids its own path names, so the tags are
/// read and written on the one stored record rather than on a copy taken while the ARN was
/// resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaggedResource {
    /// A farm, by its `farmId`.
    Farm { farm_id: String },
    /// A queue of a farm, by both ids its path names.
    Queue { farm_id: String, queue_id: String },
    /// A fleet of a farm, by both ids its path names.
    Fleet { farm_id: String, fleet_id: String },
    /// A job of a queue, by the three ids its path names.
    Job {
        farm_id: String,
        queue_id: String,
        job_id: String,
    },
    /// A worker of a fleet, by the three ids its path names.
    Worker {
        farm_id: String,
        fleet_id: String,
        worker_id: String,
    },
    /// A budget of a farm, by both ids its path names.
    Budget { farm_id: String, budget_id: String },
}

/// Why a `resourceArn` reaches no stored resource, as the refusal the operation naming it
/// answers with.
///
/// The distinction is the client-visible one: a value that cannot address a taggable
/// resource of this API at all is a bad request, which no later state can make good, while
/// an ARN that does address one this store holds no record for is a missing resource.
#[derive(Debug)]
pub enum UnreachableResource {
    /// The value is not the ARN of a taggable resource of this service: not a well-formed
    /// ARN, an ARN of another vendor, or one whose resource field names no taggable
    /// resource of this API. `ValidationException`, HTTP 400.
    Invalid(crate::ops::types::ValidationException),
    /// An ARN of a taggable resource of this service that names no stored record.
    /// `ResourceNotFoundException`, HTTP 404.
    NotFound(crate::ops::types::ResourceNotFoundException),
}

/// The one failure that is not about the request: an ARN resolved to a resource this store
/// then held no record of, which the lock the resolution and the access share makes
/// impossible. Reported rather than passed off as a resource carrying no tags.
pub const RESOLVED_RESOURCE_VANISHED: &str =
    "the resource this request named was resolved but holds no record";

/// The 400 for a value that cannot address a taggable resource of this service.
///
/// Both halves read the same way to a caller — the value names no taggable resource of
/// this namespace — so they are worded as one refusal, naming the member at fault the way
/// every other refusal of this service does and stating the form the value has to take.
fn invalid_resource_arn(resource_arn: &str) -> crate::ops::types::ValidationException {
    field_validation_failed(
        RESOURCE_ARN_MEMBER,
        &format!(
            "{resource_arn} isn't the Amazon Resource Name of a taggable {ARN_NAMESPACE} \
             resource. The value must be of the form \
             arn:<partition>:{ARN_NAMESPACE}:<region>:<account-id>:<resource>, naming the \
             resource as its API path nests it."
        ),
    )
}

/// The 400 a tag write earns when it carries a tag with no key.
///
/// `smithy_query(query="doc", shape="TagResourceRequest", member="tags")`: "Tag keys and
/// values are both required, but tag values can be empty strings." A key is therefore
/// required text, and the empty string is not a key; a value may be empty.
pub fn empty_tag_key() -> crate::ops::types::ValidationException {
    field_validation_failed(
        TAGS_MEMBER,
        "A tag key can't be empty. Tag keys and values are both required, but tag values \
         can be empty strings.",
    )
}

/// The 400 a tag write earns when a tag key is longer than [`MAX_TAG_KEY_LENGTH`].
///
/// The key itself is not interpolated — it is the value at fault and it is too long to
/// restate — so the sentence names the bound and the length the request sent.
pub fn tag_key_too_long(found: usize) -> crate::ops::types::ValidationException {
    field_validation_failed(
        TAGS_MEMBER,
        &format!(
            "A tag key can have at most {MAX_TAG_KEY_LENGTH} characters. This request sent \
             a key of {found} characters."
        ),
    )
}

/// The 400 a tag write earns when a tag value is longer than [`MAX_TAG_VALUE_LENGTH`].
///
/// The KEY is named — it is within its own bound by the time this is reached, and it is
/// what tells the caller which pair to fix — and the value is not.
pub fn tag_value_too_long(key: &str, found: usize) -> crate::ops::types::ValidationException {
    field_validation_failed(
        TAGS_MEMBER,
        &format!(
            "The value of tag key {key} can have at most {MAX_TAG_VALUE_LENGTH} characters. \
             This request sent a value of {found} characters."
        ),
    )
}

/// The 400 a tag write earns when it would leave the resource holding more tags than
/// [`MAX_TAGS_PER_RESOURCE`].
///
/// `would_hold` is what the resource would carry had the write been applied, counted from
/// the tags it already holds and the keys the request adds, so the sentence states the
/// bound and what the request asked for.
pub fn too_many_tags(would_hold: usize) -> crate::ops::types::ValidationException {
    field_validation_failed(
        TAGS_MEMBER,
        &format!(
            "A resource can have at most {MAX_TAGS_PER_RESOURCE} tags. This request would \
             leave the resource with {would_hold} tags."
        ),
    )
}

/// The `(type, id)` pairs the resource field of an ARN spells, or `None` when it is not a
/// sequence of such pairs.
///
/// AWS writes a nested resource as `<type>/<id>` repeated — `farm/<farmId>/queue/<queueId>`
/// — so a resource field with an odd number of segments, or an empty one, names no
/// resource of this service.
fn type_id_pairs(resource: &str) -> Option<Vec<(&str, &str)>> {
    let segments: Vec<&str> = resource.split('/').collect();
    if !segments.len().is_multiple_of(2) || segments.iter().any(|segment| segment.is_empty()) {
        return None;
    }
    Some(
        segments
            .chunks(2)
            .map(|pair| (pair[0], pair[1]))
            .collect::<Vec<_>>(),
    )
}

/// The resource an ARN of this service's namespace addresses, before any lookup: which
/// resource type its path names, and the ids it names it by.
///
/// `None` for a resource field that names no taggable resource of this API — a type this
/// API cannot tag (a limit, a storage profile, a queue environment), a nesting the model's
/// paths do not have, or a path that is not `<type>/<id>` pairs at all.
fn addressed_resource(resource: &str) -> Option<TaggedResource> {
    let pairs = type_id_pairs(resource)?;
    let resource = match pairs.as_slice() {
        [(kind, farm_id)] if *kind == RESOURCE_TYPE_FARM => TaggedResource::Farm {
            farm_id: (*farm_id).to_string(),
        },
        [(farm, farm_id), (kind, queue_id)]
            if *farm == RESOURCE_TYPE_FARM && *kind == RESOURCE_TYPE_QUEUE =>
        {
            TaggedResource::Queue {
                farm_id: (*farm_id).to_string(),
                queue_id: (*queue_id).to_string(),
            }
        }
        [(farm, farm_id), (kind, fleet_id)]
            if *farm == RESOURCE_TYPE_FARM && *kind == RESOURCE_TYPE_FLEET =>
        {
            TaggedResource::Fleet {
                farm_id: (*farm_id).to_string(),
                fleet_id: (*fleet_id).to_string(),
            }
        }
        [(farm, farm_id), (kind, budget_id)]
            if *farm == RESOURCE_TYPE_FARM && *kind == RESOURCE_TYPE_BUDGET =>
        {
            TaggedResource::Budget {
                farm_id: (*farm_id).to_string(),
                budget_id: (*budget_id).to_string(),
            }
        }
        [(farm, farm_id), (queue, queue_id), (kind, job_id)]
            if *farm == RESOURCE_TYPE_FARM
                && *queue == RESOURCE_TYPE_QUEUE
                && *kind == RESOURCE_TYPE_JOB =>
        {
            TaggedResource::Job {
                farm_id: (*farm_id).to_string(),
                queue_id: (*queue_id).to_string(),
                job_id: (*job_id).to_string(),
            }
        }
        [(farm, farm_id), (fleet, fleet_id), (kind, worker_id)]
            if *farm == RESOURCE_TYPE_FARM
                && *fleet == RESOURCE_TYPE_FLEET
                && *kind == RESOURCE_TYPE_WORKER =>
        {
            TaggedResource::Worker {
                farm_id: (*farm_id).to_string(),
                fleet_id: (*fleet_id).to_string(),
                worker_id: (*worker_id).to_string(),
            }
        }
        _ => return None,
    };
    Some(resource)
}

/// Whether the resource field names a taggable resource type this emulator stores nothing
/// of yet, which is a resource that is MISSING rather than a value that addresses nothing.
///
/// `CreateMonitor` and `CreateLicenseEndpoint` both take `tags`, so both resources are
/// addressable by a tagging call; neither create is implemented, so this store holds no
/// record either one could name. The type and id are reported the way every other missing
/// resource of this service is.
fn unstored_taggable_resource(resource: &str) -> Option<(&str, &str)> {
    let [(kind, id)] = type_id_pairs(resource)?[..] else {
        return None;
    };
    (kind == RESOURCE_TYPE_MONITOR || kind == RESOURCE_TYPE_LICENSE_ENDPOINT).then_some((kind, id))
}

impl ComAmazonawsDeadlineDeadlineState {
    /// The stored resource whose tags `resource_arn` names.
    ///
    /// A tagging call addresses a resource of this service's own namespace: a value that is
    /// not such an ARN, or one whose resource field names no taggable resource of this API,
    /// can name no resource of this request and is refused as invalid input before any
    /// lookup; an ARN that does name one this store holds no record for — an unknown id, a
    /// resource since deleted, a resource type nothing has created yet — is refused as not
    /// found, naming the LEVEL of its path that is missing, exactly as every read of this
    /// service names it.
    pub fn resolve_tagged_resource(
        &self,
        resource_arn: &str,
    ) -> Result<TaggedResource, UnreachableResource> {
        let Some(arn) = ::service_common::arn::parse_of(resource_arn, ARN_NAMESPACE) else {
            return Err(UnreachableResource::Invalid(invalid_resource_arn(
                resource_arn,
            )));
        };
        let Some(resource) = addressed_resource(arn.resource) else {
            if let Some((resource_type, resource_id)) = unstored_taggable_resource(arn.resource) {
                return Err(UnreachableResource::NotFound(not_found(
                    resource_type,
                    resource_id,
                )));
            }
            return Err(UnreachableResource::Invalid(invalid_resource_arn(
                resource_arn,
            )));
        };
        match self.missing_level(&resource) {
            Some((resource_type, resource_id)) => Err(UnreachableResource::NotFound(not_found(
                resource_type,
                &resource_id,
            ))),
            None => Ok(resource),
        }
    }

    /// The level of `resource`'s path this store holds no record of, as the type and id a
    /// not-found reports, or `None` when every level of it is here.
    ///
    /// Each level is checked in path order, so a call naming a queue of a farm that does
    /// not exist is told about the farm — the same level every other read of this service
    /// reports.
    fn missing_level(&self, resource: &TaggedResource) -> Option<(&'static str, String)> {
        let farm_missing = |farm_id: &String| {
            self.farm(farm_id)
                .is_none()
                .then(|| (RESOURCE_TYPE_FARM, farm_id.clone()))
        };
        match resource {
            TaggedResource::Farm { farm_id } => farm_missing(farm_id),
            TaggedResource::Queue { farm_id, queue_id } => farm_missing(farm_id).or_else(|| {
                self.queue(farm_id, queue_id)
                    .is_none()
                    .then(|| (RESOURCE_TYPE_QUEUE, queue_id.clone()))
            }),
            TaggedResource::Fleet { farm_id, fleet_id } => farm_missing(farm_id).or_else(|| {
                self.fleet(farm_id, fleet_id)
                    .is_none()
                    .then(|| (RESOURCE_TYPE_FLEET, fleet_id.clone()))
            }),
            TaggedResource::Budget { farm_id, budget_id } => farm_missing(farm_id).or_else(|| {
                self.budget(farm_id, budget_id)
                    .is_none()
                    .then(|| (RESOURCE_TYPE_BUDGET, budget_id.clone()))
            }),
            TaggedResource::Job {
                farm_id,
                queue_id,
                job_id,
            } => farm_missing(farm_id)
                .or_else(|| {
                    self.queue(farm_id, queue_id)
                        .is_none()
                        .then(|| (RESOURCE_TYPE_QUEUE, queue_id.clone()))
                })
                .or_else(|| {
                    self.job(farm_id, queue_id, job_id)
                        .is_none()
                        .then(|| (RESOURCE_TYPE_JOB, job_id.clone()))
                }),
            TaggedResource::Worker {
                farm_id,
                fleet_id,
                worker_id,
            } => farm_missing(farm_id)
                .or_else(|| {
                    self.fleet(farm_id, fleet_id)
                        .is_none()
                        .then(|| (RESOURCE_TYPE_FLEET, fleet_id.clone()))
                })
                .or_else(|| {
                    self.worker(farm_id, fleet_id, worker_id)
                        .is_none()
                        .then(|| (RESOURCE_TYPE_WORKER, worker_id.clone()))
                }),
        }
    }

    /// The job `job_id` names under `farm_id`/`queue_id`, exactly as stored.
    ///
    /// The plain lookup, beside [`settled_job`](Self::settled_job): a call about a job's
    /// TAGS is about neither its lifecycle nor its tasks, so it asks whether the job is
    /// here without settling what the clock owes it.
    pub fn job(&self, farm_id: &str, queue_id: &str, job_id: &str) -> Option<&super::Job> {
        self.queue(farm_id, queue_id)?.jobs.get(job_id)
    }

    /// The tags stored on `resource`, or `None` for a resource this store no longer holds —
    /// which a caller that resolved it under the same lock never sees.
    pub fn tags_of(&self, resource: &TaggedResource) -> Option<&HashMap<String, String>> {
        match resource {
            TaggedResource::Farm { farm_id } => self.farm(farm_id).map(|farm| &farm.tags),
            TaggedResource::Queue { farm_id, queue_id } => {
                self.queue(farm_id, queue_id).map(|queue| &queue.tags)
            }
            TaggedResource::Fleet { farm_id, fleet_id } => {
                self.fleet(farm_id, fleet_id).map(|fleet| &fleet.tags)
            }
            TaggedResource::Budget { farm_id, budget_id } => {
                self.budget(farm_id, budget_id).map(|budget| &budget.tags)
            }
            TaggedResource::Job {
                farm_id,
                queue_id,
                job_id,
            } => self.job(farm_id, queue_id, job_id).map(|job| &job.tags),
            TaggedResource::Worker {
                farm_id,
                fleet_id,
                worker_id,
            } => self
                .worker(farm_id, fleet_id, worker_id)
                .map(|worker| &worker.tags),
        }
    }

    /// Mutable form of [`tags_of`](Self::tags_of): the one map a tag write and a tag
    /// removal both work on, so neither writes a second copy of a resource's tags.
    pub fn tags_mut(&mut self, resource: &TaggedResource) -> Option<&mut HashMap<String, String>> {
        match resource {
            TaggedResource::Farm { farm_id } => {
                self.farms.get_mut(farm_id).map(|farm| &mut farm.tags)
            }
            TaggedResource::Queue { farm_id, queue_id } => self
                .queue_mut(farm_id, queue_id)
                .map(|queue| &mut queue.tags),
            TaggedResource::Fleet { farm_id, fleet_id } => self
                .fleet_mut(farm_id, fleet_id)
                .map(|fleet| &mut fleet.tags),
            TaggedResource::Budget { farm_id, budget_id } => self
                .budget_mut(farm_id, budget_id)
                .map(|budget| &mut budget.tags),
            TaggedResource::Job {
                farm_id,
                queue_id,
                job_id,
            } => self
                .queue_mut(farm_id, queue_id)?
                .jobs
                .get_mut(job_id)
                .map(|job| &mut job.tags),
            TaggedResource::Worker {
                farm_id,
                fleet_id,
                worker_id,
            } => self
                .fleet_mut(farm_id, fleet_id)?
                .workers
                .get_mut(worker_id)
                .map(|worker| &mut worker.tags),
        }
    }
}

/// Apply `requested` to the tags `stored` holds, or refuse the write whole.
///
/// A tag write ADDS the pairs it carries and replaces the value of a key the resource
/// already holds; it never removes a key the request does not name. A refusal applies
/// nothing: every pair is checked, and the per-resource bound decided, before the first
/// insert, so a refused write leaves the resource exactly as it was.
pub fn apply_tags(
    stored: &mut HashMap<String, String>,
    requested: HashMap<String, String>,
) -> Result<(), crate::ops::types::ValidationException> {
    for (key, value) in &requested {
        if key.is_empty() {
            return Err(empty_tag_key());
        }
        let key_length = key.chars().count();
        if key_length > MAX_TAG_KEY_LENGTH {
            return Err(tag_key_too_long(key_length));
        }
        let value_length = value.chars().count();
        if value_length > MAX_TAG_VALUE_LENGTH {
            return Err(tag_value_too_long(key, value_length));
        }
    }
    let added = requested
        .keys()
        .filter(|key| !stored.contains_key(*key))
        .count();
    let would_hold = stored.len() + added;
    if would_hold > MAX_TAGS_PER_RESOURCE {
        return Err(too_many_tags(would_hold));
    }
    stored.extend(requested);
    Ok(())
}
