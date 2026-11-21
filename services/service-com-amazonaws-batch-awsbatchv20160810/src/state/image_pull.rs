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
//! The fetch of an image held in an Amazon ECR registry is preceded by the retrieval of an
//! EXECUTION RESOURCE: one authorization token for that registry, obtained as the job's
//! execution role and granted on the registry rather than on any repository. A role refused
//! it has reached no image, so what such a job reports is that retrieval
//! ([`unable_to_retrieve_registry_auth_message`]) rather than the runtime's account of a
//! fetch that was never made — again the sentence the Amazon ECS task's own stop carries.
//!
//! # Who answers the fetch
//!
//! The registry the reference names does, and only where this emulator HOLDS that registry:
//!
//! * `<account>.dkr.ecr.<region>.amazonaws.com/<repository>[:<tag>|@<digest>]` addresses an
//!   Amazon ECR private registry, which decides the pull itself — whose registry the
//!   reference addresses, the authorization on the principal the pull is made as, the
//!   resolution of the reference, and the upstream import a pull through cache rule covers
//!   ([`::service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::state::registry_pull`]).
//!   AWS Batch pulls as the job's EXECUTION ROLE — `ContainerProperties$executionRoleArn` is
//!   "the Amazon Resource Name (ARN) of the execution role that Batch can assume ... for
//!   jobs running on Fargate resources, you must provide an execution role" — so that is the
//!   principal handed to it. A registry belongs to ONE account, whose contents are shared
//!   only through that registry's own resource-based policy, so a reference into ANOTHER
//!   account's registry is a cross-account pull nothing here serves and the job's container
//!   never runs.
//! * `public.ecr.aws/<registry alias>/<repository>[:<tag>|@<digest>]` addresses the Amazon ECR
//!   Public gallery, which answers it the same way against the repositories and tags it holds
//!   ([`::service_com_amazonaws_ecrpublic_spencerfrontendservice::state::gallery_pull`]) — for
//!   the ONE registry this emulator holds there ([`gallery_pull::serves_registry_alias`]). An
//!   alias it holds no registry for is another account's public registry, which this emulator
//!   has no store for, so nothing is decided about such a reference either — EXCEPT for the
//!   repositories that gallery publishes itself whose image this emulator holds, where what
//!   the gallery publishes decides the reference
//!   ([`::service_com_amazonaws_ecrpublic_spencerfrontendservice::state::published_images`]).
//!
//! A reference naming any OTHER registry is not one this emulator holds and so not one it
//! answers for: nothing is decided about such an image, and the job's container runs the
//! program it declares exactly as one whose registry served it does — as the Amazon ECS task
//! the capacity takes ([`super::ecs_task`]) or performed in this process
//! ([`container_run`]). Neither is anything decided when
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

/// The `statusReason` of a job whose image the registry REFUSED to serve to its execution
/// role.
///
/// The same `CannotPullContainerError` class, worded for what the runtime saw: the registry
/// answers a manifest request no authorization covers with `403 Forbidden`
/// ([`FORBIDDEN_STATUS`]), and the runtime reports the request it made and the status it got
/// back, having retried the fetch to the end of its budget ([`MANIFEST_FETCH_RETRIES`]). The
/// authorization decision itself is the registry's own and is not on this wire: a `docker
/// pull` is answered with an HTTP status, not with an IAM sentence.
///
/// `manifest_url` is the request the registry refused ([`manifest_url`]).
pub fn pull_access_denied_message(image: &str, manifest_url: &str) -> String {
    format!(
        "CannotPullContainerError: pull image manifest has been retried \
         {MANIFEST_FETCH_RETRIES} time(s): failed to resolve ref {image}: unexpected status \
         from HEAD request to {manifest_url}: {FORBIDDEN_STATUS}"
    )
}

/// The HTTP status a registry answers a request the credential presented does not cover
/// with: `403 Forbidden` (RFC 9110, "the server understood the request but refuses to
/// authorize it"), which is what the Docker Registry HTTP API V2 answers such a manifest
/// request and therefore what the runtime reports for it.
const FORBIDDEN_STATUS: &str = "403 Forbidden";

/// The `statusReason` of a job whose image a PUBLIC registry holds nothing for: the same
/// `CannotPullContainerError` class, in the words the container agent words a reference that
/// resolves to no content with.
///
/// The image's MANIFEST is what is fetched first, and the agent retries that fetch before it
/// gives up ([`MANIFEST_FETCH_RETRIES`]); it then names the reference it could not resolve
/// twice — once as the reference the fetch asked for, and once as the answer it got about it,
/// which for a reference that resolves to nothing is that same reference and `not found`. A
/// public registry answers a `docker pull` with a status and that sentence rather than with an
/// API shape of its own, so there is no second wording to carry; the same sentence the Amazon
/// ECS crate reports as the `stoppedReason` of such a task, because a Batch job's container IS
/// an Amazon ECS task's container.
pub fn image_not_found_message(image: &str) -> String {
    format!(
        "CannotPullContainerError: pull image manifest has been retried \
         {MANIFEST_FETCH_RETRIES} time(s): failed to resolve ref {image}: {image}: not found"
    )
}

/// How many times the runtime retried the image MANIFEST fetch before it reported the
/// failure: a registry that refuses a request refuses it however often it is asked, so the
/// fetch is retried until the whole retry budget is spent and that budget is what the failure
/// reports.
const MANIFEST_FETCH_RETRIES: u32 = 7;

/// The URL a container runtime fetches the MANIFEST of `image` at, for a reference that
/// addresses `reference` of an Amazon ECR registry: the registry the reference was written
/// against and the Docker Registry HTTP API V2 manifest route of the repository it names
/// (`GET /v2/<name>/manifests/<reference>`, where the reference is the tag or the digest
/// asked for).
///
/// The repository and the tag or digest are the registry's OWN reading of the reference
/// ([`registry_pull::parse_registry_reference`]), so the request named here is the one that
/// pull makes rather than a second reading of its own.
fn manifest_url(
    image: &str,
    reference: &::service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::state::registry_pull::RegistryReference,
) -> String {
    let asked_for = reference
        .digest
        .as_deref()
        .or(reference.tag.as_deref())
        .unwrap_or_default();
    let host = image.split_once('/').map_or(image, |(host, _)| host);
    format!(
        "https://{host}/v2/{}/manifests/{asked_for}",
        reference.repository_name
    )
}

/// The `statusReason` of a job whose execution role may not obtain the AUTHORIZATION TOKEN
/// for the Amazon ECR registry its image comes from.
///
/// That token is not an image: it is one of the EXECUTION RESOURCES retrieved for the
/// container before it starts — as the execution role, before any manifest is fetched — so a
/// role refused it fails the job in that step, and what the job reports is the agent's
/// account of the resource it could not retrieve rather than the runtime's account of a fetch
/// that was never made. The class is `ResourceInitializationError`, the resource is the
/// registry authentication ([`ECR_REGISTRY_AUTH_RESOURCE`]) and `refusal` is Amazon ECR's own
/// answer under the class that service refuses an unauthorized call with
/// ([`ACCESS_DENIED_CLASS`]), so nothing here is authored about the authorization.
pub fn unable_to_retrieve_registry_auth_message(refusal: &str) -> String {
    format!(
        "ResourceInitializationError: unable to pull secrets or registry auth: execution \
         resource retrieval failed: unable to retrieve {ECR_REGISTRY_AUTH_RESOURCE}: service \
         call has been retried {RETRIEVAL_ATTEMPTS} time(s): {ACCESS_DENIED_CLASS}: {refusal}"
    )
}

/// What the agent calls the execution resource it retrieves from Amazon ECR: the
/// authorization token for the registry (`ecr`), obtained once before the container's image
/// is fetched.
const ECR_REGISTRY_AUTH_RESOURCE: &str = "ecr registry auth";

/// How many attempts at the retrieval the reason reports: the agent states how often it
/// retried the call it could not complete, and this emulator makes that call ONCE — the
/// answer it gets is the emulated service's own and does not change between attempts.
const RETRIEVAL_ATTEMPTS: usize = 1;

/// The error class Amazon ECR refuses an unauthorized call with, which is the AWS common
/// `AccessDeniedException` — the class the agent's own account of a failed retrieval names.
const ACCESS_DENIED_CLASS: &str = "AccessDeniedException";

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
    use ::service_com_amazonaws_ecrpublic_spencerfrontendservice::state::{
        gallery_pull, published_images,
    };

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
            // The registry AUTHENTICATION comes before the fetch: one authorization token is
            // obtained for the Amazon ECR registry as the job's execution role, granted on
            // the registry rather than on any repository, so a role refused it has reached no
            // image and what the job reports is that retrieval
            // ([`unable_to_retrieve_registry_auth_message`]). A reference into another
            // registry states nothing about this one, so nothing is asked for it.
            let pulled_as = fetch
                .execution_role_arn
                .as_deref()
                .map(registry_pull::PullPrincipal::of_role);
            let verdict = if let Some(refusal) = registry_pull::parse_reference(&fetch.image)
                .and_then(|_| {
                    registry_pull::withheld_registry_authorization(engine, pulled_as)
                })
            {
                ImageFetch::Refused(unable_to_retrieve_registry_auth_message(&refusal))
            } else if let Some(served) =
                registry_pull::serve_reference_pull(engine, &fetch.image, pulled_as, now)
            {
                match served {
                    Ok(_) => ImageFetch::Served,
                    // The registry refused the manifest request the fetch makes, which the
                    // runtime reports as the status it got back.
                    Err(registry_pull::PullRefusal::NotAuthorized { .. }) => {
                        let refused = registry_pull::parse_registry_reference(&fetch.image)
                            .map(|(_, reference)| manifest_url(&fetch.image, &reference))
                            .unwrap_or_default();
                        ImageFetch::Refused(pull_access_denied_message(&fetch.image, &refused))
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
            } else if let Some(published) = gallery_pull::parse_gallery_reference(&fetch.image)
                .as_ref()
                .and_then(published_images::published_image)
            {
                // A repository the gallery AWS serves publishes ITSELF, whose image this
                // emulator holds: what it publishes decides the reference, so a tag it
                // publishes no release under resolves to no image and the fetch fails in the
                // agent's own words ([`published_images`], [`image_not_found_message`]).
                match published {
                    published_images::PublishedImage::Published => ImageFetch::Served,
                    published_images::PublishedImage::NotPublished => {
                        ImageFetch::Refused(image_not_found_message(&fetch.image))
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
