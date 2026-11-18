// SPDX-License-Identifier: CC0-1.0

//! The IMAGE FETCH a placed job's container needs, and the job a failed fetch ends.
//!
//! # The rule
//!
//! A container runs an image, and the image is fetched from its registry BEFORE the container
//! starts — so a job whose image cannot be fetched never runs at all, and what it reports is
//! the fetch failure rather than anything about a run. `JobDetail$statusReason` is "a short,
//! human-readable string to provide more details for the current status of the job", and for a
//! fetch that failed the container runtime words it as a `CannotPullContainerError`
//! ([`cannot_pull_image_message`]) — the same sentence Amazon ECS reports as the
//! `stoppedReason` of the task, because a Batch job's container IS an Amazon ECS task's
//! container. Nothing of the run is reported with it: no `startedAt`, no `stoppedAt`, no
//! `exitCode`, and the CloudWatch Logs stream the attempt names — opened for the task, so it
//! exists either way (see [`super::job_logs`]) — holds no event, because nothing wrote one.
//!
//! # Who answers the fetch
//!
//! The registry the reference names does, and only where this emulator HOLDS that registry:
//!
//! * `<account>.dkr.ecr.<region>.amazonaws.com/<repository>[:<tag>|@<digest>]` addresses an
//!   Amazon ECR private registry, which decides the pull itself — the authorization on the
//!   principal the pull is made as, the resolution of the reference, and the upstream import a
//!   pull through cache rule covers
//!   ([`::service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::state::registry_pull`]).
//!   AWS Batch pulls as the job's EXECUTION ROLE — `ContainerProperties$executionRoleArn` is
//!   "the Amazon Resource Name (ARN) of the execution role that Batch can assume ... for
//!   jobs running on Fargate resources, you must provide an execution role" — so that is the
//!   principal handed to it.
//! * `public.ecr.aws/<registry alias>/<repository>[:<tag>|@<digest>]` addresses the Amazon ECR
//!   Public gallery, which answers it the same way against the repositories and tags it holds
//!   ([`::service_com_amazonaws_ecrpublic_spencerfrontendservice::state::gallery_pull`]) — for
//!   the ONE registry this emulator holds there ([`gallery_pull::serves_registry_alias`]). An
//!   alias it holds no registry for is another account's public registry, which this emulator
//!   has no store for, so nothing is decided about such a reference either.
//!
//! A reference naming any OTHER registry is not one this emulator holds and so not one it
//! answers for: nothing is decided about such an image, and the job's container runs the
//! program it declares exactly as one whose registry served it does ([`container_run`]).
//! Neither is anything decided when
//! the engine carries no store for the registry the reference names at all — an absent store
//! is not evidence that an image is missing.
//!
//! # When it is attempted
//!
//! `RUNNABLE` is where a job waits for the capacity its queue offers, so the fetch of its
//! image is attempted while it waits there: once for each job whose queue can place it
//! ([`ComAmazonawsBatchAwsbatchv20160810State::jobs_awaiting_image_fetch`]), by the call that
//! finds it waiting and with this store's lock RELEASED — the verdict is a question for
//! another service's store, which is read under that store's lock and no other
//! ([`settle_job_image_fetches`], the shape [`super::job_logs`] and
//! [`super::service_job_dispatch`] follow). The verdict is then recorded on the job, and the
//! PLACEMENT that falls due one span later is what materialises it
//! ([`ComAmazonawsBatchAwsbatchv20160810State::place_job`]): a fetch the registry refused
//! ends the job's initiation one span later, before any container exists, so no run is ever
//! reported for it ([`super::container_run`]).
//!
//! A job whose fetch has not been answered yet is not placed — its placement stays due, and
//! the next drain answers the fetch — so the reason a job ends with never depends on how
//! quickly anybody called.

use super::*;

/// The `statusReason` of a job whose container image could not be fetched.
///
/// The error CLASS is the container runtime's `CannotPullContainerError`, which is what a
/// compute service surfaces for a failed fetch, with the condition the runtime words for a
/// reference that resolves to no content; the variable parts are the reference the job asked
/// for and the registry's OWN account of what it does not hold, so nothing here is authored
/// about an image.
pub fn cannot_pull_image_message(image: &str, refusal: &str) -> String {
    format!("CannotPullContainerError: failed to resolve ref {image}: not found: {refusal}")
}

/// The `statusReason` of a job whose image the registry would not serve to its execution
/// role: the same class, with the condition the runtime words for a registry that refuses the
/// pull and the registry's own refusal as the authorization it was refused by.
pub fn pull_access_denied_message(image: &str, refusal: &str) -> String {
    format!(
        "CannotPullContainerError: failed to resolve ref {image}: pull access denied, \
         repository does not exist or may require authorization: {refusal}"
    )
}

/// What the fetch of ONE job's container image answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageFetch {
    /// The image is there to be fetched — or names a registry this emulator does not hold, so
    /// nothing is decided against the fetch.
    Served,
    /// The registry did not serve it: the `statusReason` the job ends with.
    Refused(String),
}

/// What ONE job's image fetch depends on: the job it belongs to, the image its container runs
/// and the principal the pull is made as.
///
/// Collected from the stored jobs under this store's lock
/// ([`ComAmazonawsBatchAwsbatchv20160810State::jobs_awaiting_image_fetch`]) and answered
/// against the registries' own stores under those stores' locks, so no two locks are ever held
/// at once.
#[derive(Debug, Clone)]
pub struct JobImageFetch {
    /// The job the verdict belongs to.
    pub job_id: String,
    /// The image its container runs, as `ContainerDetail$image` holds it.
    pub image: String,
    /// The job's execution role, which Batch makes the pull as, and `None` for a job whose
    /// definition and overrides name none.
    pub execution_role_arn: Option<String>,
}

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// The jobs waiting in `RUNNABLE` on capacity that can hold them whose image fetch has not
    /// been answered yet.
    ///
    /// An ARRAY parent is never placed and runs no container of its own, so nothing is fetched
    /// for it; a job whose queue offers no capacity is not being placed, so its image is not
    /// being fetched either. Costs one walk of the jobs map, which [`limits::MAX_JOBS`] bounds
    /// — the same walk [`ComAmazonawsBatchAwsbatchv20160810State::rearm_resting_jobs`] makes.
    pub fn jobs_awaiting_image_fetch(&self) -> Vec<JobImageFetch> {
        self.jobs
            .values()
            .filter(|job| {
                job.status == JOB_RUNNABLE && job.array_size.is_none() && job.image_fetch.is_none()
            })
            .filter_map(|job| {
                let container = job.container.as_ref()?;
                let image = container.image.clone()?;
                Some(JobImageFetch {
                    job_id: job.job_id.clone(),
                    image,
                    execution_role_arn: container.execution_role_arn.clone(),
                })
            })
            .filter(|fetch| self.job_is_placeable(&fetch.job_id))
            .collect()
    }

    /// Record what each job's image fetch answered.
    ///
    /// Written once per job: a job that already carries a verdict keeps it, so a later call
    /// re-decides nothing and the reason a job ends with cannot change under a reader.
    pub fn record_image_fetches(&mut self, verdicts: Vec<(String, ImageFetch)>) {
        for (id, verdict) in verdicts {
            if let Some(job) = self.jobs.get_mut(&id) {
                if job.image_fetch.is_none() {
                    job.image_fetch = Some(verdict);
                }
            }
        }
    }

    /// Whether a job is still waiting for its image fetch to be answered, which is what keeps
    /// the clock pump awake until every placement that is due can be made.
    pub(super) fn has_pending_image_fetches(&self) -> bool {
        !self.jobs_awaiting_image_fetch().is_empty()
    }

    /// What the fetch of `id`'s image answered, or `None` while it has not been answered.
    pub(super) fn image_fetch_of(&self, id: &str) -> Option<ImageFetch> {
        let job = self.jobs.get(id)?;
        // A job whose container names no image at all has nothing to fetch, so there is
        // nothing to wait for: the placement proceeds.
        if job
            .container
            .as_ref()
            .and_then(|container| container.image.as_ref())
            .is_none()
        {
            return Some(ImageFetch::Served);
        }
        job.image_fetch.clone()
    }
}

/// Attempt the image fetch of every job waiting to be placed, and record what each one
/// answered.
///
/// Called by the drain every operation of this service's job plane makes, and by the clock
/// pump, with this store's guard dropped: each verdict is a question about what a REGISTRY
/// holds, read out of that service's own store through the one shared engine, under that
/// store's lock and no other. Nothing is attempted twice — a job that already carries a
/// verdict is not collected — and nothing is recorded for a fact this emulator does not hold.
pub fn settle_job_image_fetches<Eng: Send + Sync + 'static>(engine: &Eng) {
    use ::service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::state::registry_pull;
    use ::service_com_amazonaws_ecrpublic_spencerfrontendservice::state::gallery_pull;

    let Some(state) = service_state(engine) else {
        return;
    };
    // The guard is taken and dropped inside this statement: no lock of this store is held
    // while another service's is.
    let waiting = state.lock().jobs_awaiting_image_fetch();
    if waiting.is_empty() {
        return;
    }
    let now = SystemTime::now();
    let verdicts: Vec<(String, ImageFetch)> = waiting
        .into_iter()
        .map(|fetch| {
            let verdict = if let Some(reference) = registry_pull::parse_reference(&fetch.image) {
                match registry_pull::serve_pull(
                    engine,
                    &reference,
                    fetch.execution_role_arn.as_deref(),
                    now,
                ) {
                    Ok(_) => ImageFetch::Served,
                    Err(registry_pull::PullRefusal::NotAuthorized { message, .. }) => {
                        ImageFetch::Refused(pull_access_denied_message(&fetch.image, &message))
                    }
                    Err(registry_pull::PullRefusal::NotFound { message }) => {
                        ImageFetch::Refused(cannot_pull_image_message(&fetch.image, &message))
                    }
                }
            } else if let Some(reference) = gallery_pull::parse_gallery_reference(&fetch.image)
                .filter(|reference| {
                    // The gallery answers for the registry alias it was ASSIGNED; an alias it
                    // holds no registry for names another account's public registry, which
                    // this emulator has no store for, so such a reference falls to the branch
                    // below and nothing is decided against it — the same reading the Amazon
                    // ECS crate's own image fetch takes.
                    gallery_pull::serves_registry_alias(engine, &reference.registry_alias)
                })
            {
                match gallery_pull::serve_pull(engine, &reference) {
                    Ok(_) => ImageFetch::Served,
                    Err(gallery_pull::GalleryPullRefusal::NotFound { message }) => {
                        ImageFetch::Refused(cannot_pull_image_message(&fetch.image, &message))
                    }
                }
            } else {
                // A reference naming a registry this emulator does not hold is not its to
                // answer, so nothing is decided against the fetch.
                ImageFetch::Served
            };
            (fetch.job_id, verdict)
        })
        .collect();
    state.lock().record_image_fetches(verdicts);
}
