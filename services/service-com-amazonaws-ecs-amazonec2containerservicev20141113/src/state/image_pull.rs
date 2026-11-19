// SPDX-License-Identifier: CC0-1.0

//! Whether a launched task can FETCH the images its containers run, and the stop a task
//! that cannot earns.
//!
//! A container runs an image, and the image is fetched from its registry before the
//! container starts — so a task whose image cannot be fetched never runs at all.
//! `Task$stopCode` is "the stop code indicating why a task was stopped", and the
//! `TaskStopCode` member for a task that failed before its containers started is
//! [`STOP_CODE_TASK_FAILED_TO_START`]; `Task$stoppedReason` ("The reason that the task was
//! stopped") carries the fetch failure itself, which the container runtime words as a
//! `CannotPullContainerError` ([`cannot_pull_container_message`] and its siblings).
//!
//! Two things have to hold for the fetch, and each is answered by the service that owns the
//! facts:
//!
//! 1. **a path to the registry**, which is Amazon VPC's and is read off the placement's own
//!    subnet:
//!
//!    * a subnet whose route table sends `0.0.0.0/0` to a PUBLIC NAT gateway reaches the
//!      internet — "with a public NAT gateway, internet-bound traffic from a private subnet
//!      can be routed to the NAT gateway" (`CreateNatGateway`), which a PRIVATE one does
//!      not do;
//!    * a subnet whose route table sends `0.0.0.0/0` to an internet gateway reaches it only
//!      for a task that was given a public address — "to use an internet gateway, your
//!      subnet's route table must contain a route that directs internet-bound traffic to
//!      the internet gateway" (Amazon VPC User Guide) and the instance must have a public
//!      address, which for an `awsvpc` task is `AwsVpcConfiguration$assignPublicIp`
//!      ([`ASSIGN_PUBLIC_IP_ENABLED`]);
//!    * a subnet with no route off the VPC at all can still reach AMAZON ECR, through the
//!      endpoints that service publishes: an interface endpoint for `ecr.api`, one for
//!      `ecr.dkr` and a gateway endpoint for Amazon S3, which the image layers come from
//!      (Amazon ECR User Guide, "Amazon ECR interface VPC endpoints"). A public registry is
//!      not an AWS service and publishes no endpoint, so an image held in one needs the
//!      internet.
//!
//! 2. **an image the registry it names serves**, for an image reference that names a
//!    registry this emulator HOLDS, which is either of two:
//!
//!    * a repository of an AMAZON ECR private registry, which Amazon ECS pulls
//!      from as the task's EXECUTION ROLE — "the task execution role that grants the Amazon
//!      ECS container agent permission to make Amazon Web Services API calls on your behalf"
//!      (`TaskDefinition$executionRoleArn`) — so Amazon ECR decides the pull: whose registry
//!      the reference addresses, the policies in force on that role and the repository's own
//!      policy, the reference itself, and the upstream image it caches when a pull through
//!      cache rule covers the repository
//!      ([`::service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::state::registry_pull`]).
//!      A registry belongs to one account, which shares its contents only through its own
//!      resource-based policy, so a reference into ANOTHER account's Amazon ECR registry is a
//!      cross-account pull nothing here serves and the task does not start;
//!    * a repository under the registry ALIAS of the one public gallery this emulator
//!      serves, which resolves the reference against the repositories and tags it holds
//!      ([`::service_com_amazonaws_ecrpublic_spencerfrontendservice::state::gallery_pull`]).
//!
//!    Either way the registry that holds the image is the one that answers, and the manifest
//!    digest it resolved is what the container reports for the image it ran
//!    (`Container$imageDigest`). A reference naming any OTHER registry — an alias that
//!    gallery was not assigned, which names another public registry, another registry
//!    altogether, or a bare name a container runtime resolves against Docker Hub — is no
//!    registry's here to answer, so nothing beyond the path above is decided for it and no
//!    digest is known for it.
//!
//! The pull is attempted at LAUNCH, by the call that places the task: the instants it took
//! are recorded on the task (`Task$pullStartedAt`/`pullStoppedAt`) and a failure with them
//! ([`TaskRecord::launch_failure`]); the clock then stops the task at the instant it would
//! otherwise have started ([`TaskRecord::fail_launch`]), so a caller reading in between
//! still sees the task come up. It is attempted only for a task whose launch roles have
//! already been put to IAM ([`TaskRecord::launch_roles_assumed`]), because the pull is made
//! AS the execution role. The attempt is made once per task: a task that already
//! carries its pull instants is not pulled again, so a later placement by another call
//! re-decides nothing and imports nothing twice.
//!
//! The pass that attempts it settles the OTHER execution resource the agent retrieves in
//! that same step, as that same role: the `secrets` the revision references
//! ([`super::container_secrets`]). The agent's own account of a failure here —
//! "unable to pull secrets or registry auth: execution resource retrieval failed" — names
//! them together because it is one step, so one verdict is recorded for the task and either
//! half can be what fails its launch.
//!
//! LOCK ORDER: the cross-service reads here take the Amazon EC2 store's lock and then the
//! Amazon ECR store's (which takes the IAM store's), each holding no other while it does,
//! and hand back owned values — so they are called with NO guard of this service's store
//! held, which is what keeps a lock cycle between the stores unreachable. The same contract
//! [`unattached_target_groups`] has.

use super::*;

use ::service_com_amazonaws_ec2_amazonec2::ops::types::{Route, RouteTable};
use ::service_com_amazonaws_ec2_amazonec2::state::{
    CONNECTIVITY_TYPE_PUBLIC, NAT_GATEWAY_STATE_AVAILABLE, NAT_GATEWAY_STATE_PENDING,
    ROUTE_STATE_BLACKHOLE, VPC_ENDPOINT_STATE_AVAILABLE, VPC_ENDPOINT_STATE_PENDING,
    VPC_ENDPOINT_TYPE_GATEWAY, VPC_ENDPOINT_TYPE_INTERFACE,
};
use ::service_com_amazonaws_ec2_amazonec2::ComAmazonawsEc2Amazonec2State;

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of at all: the
/// `TaskStopCode` enum's `TaskFailedToStart` member
/// (`smithy_query(query="shape",shape="TaskStopCode")` -> EssentialContainerExited,
/// InfrastructureHealth, ServiceSchedulerInitiated, SpotInterruption, TaskFailedToStart,
/// TerminationNotice, UserInitiated).
pub const STOP_CODE_TASK_FAILED_TO_START: &str = "TaskFailedToStart";

/// The destination of the route that carries traffic OFF a VPC: "a route table contains a
/// set of rules, called routes" and the one for everything not inside the VPC is the
/// default route (Amazon VPC User Guide, "Route tables").
pub const DEFAULT_IPV4_DESTINATION: &str = "0.0.0.0/0";

/// The id prefix of an internet gateway, which EC2 spells `igw-<hex>` — what a route's
/// `GatewayId` carries when the route points at one.
pub const INTERNET_GATEWAY_ID_PREFIX: &str = "igw-";

/// The registry a reference that names none is pulled from: the Docker Hub registry a
/// container runtime resolves a bare name (`nginx:latest`) against.
pub const DOCKER_HUB_REGISTRY: &str = "registry-1.docker.io";

/// The endpoint service suffix of the Amazon ECR API, as an endpoint's `ServiceName`
/// spells it (`com.amazonaws.<region>.ecr.api`).
const ECR_API_ENDPOINT_SUFFIX: &str = ".ecr.api";

/// The endpoint service suffix of the Amazon ECR Docker registry
/// (`com.amazonaws.<region>.ecr.dkr`).
const ECR_DKR_ENDPOINT_SUFFIX: &str = ".ecr.dkr";

/// The endpoint service suffix of Amazon S3, which an ECR image's layers are fetched from
/// (`com.amazonaws.<region>.s3`).
const S3_ENDPOINT_SUFFIX: &str = ".s3";

/// The host part of an Amazon ECR registry URI (`<account>.dkr.ecr.<region>.<suffix>`),
/// which is what makes an image one the ECR endpoints can reach.
const ECR_REGISTRY_INFIX: &str = ".dkr.ecr.";

/// What ONE launched task's image fetch depends on: where the launch placed it, what its
/// containers run, and the principal Amazon ECS pulls as.
///
/// Collected from the stored tasks under this service's lock
/// ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::launches_awaiting_image_pull`])
/// and answered against the Amazon EC2 and Amazon ECR stores under those stores' own, so no
/// two locks are ever held at once.
#[derive(Debug, Clone)]
pub struct TaskLaunch {
    /// The task the verdict belongs to.
    pub task_arn: String,
    /// `AwsVpcConfiguration$subnets`: "The IDs of the subnets associated with the task or
    /// service", in the order the launch named them. Empty for a task that shares the
    /// networking of the host that runs it, which carries no placement of its own.
    pub subnets: Vec<String>,
    /// `AwsVpcConfiguration$assignPublicIp`: "Whether the task's elastic network interface
    /// receives a public IP address", and `None` when the launch named no value.
    pub assign_public_ip: Option<String>,
    /// The images the task's containers run, as `Container$image` holds them.
    pub images: Vec<String>,
    /// The task execution role Amazon ECS makes the pull as
    /// ([`TaskRecord::execution_role_arn`]), and `None` for a task whose revision and
    /// overrides name none.
    pub execution_role_arn: Option<String>,
    /// The values the agent fetches for the task's containers before any of them starts —
    /// the `secrets` its revision references ([`super::container_secrets`]) — which are
    /// retrieved as that same execution role, in the same step as the registry
    /// authentication this fetch takes.
    pub secrets: Vec<ContainerSecret>,
    /// Where the `awslogs` driver of each of the task's containers delivers that
    /// container's output ([`super::log_driver_init`]), which the agent initialises as that
    /// same execution role before the container it is for runs.
    pub log_targets: Vec<AwsLogsTarget>,
}

/// The `stoppedReason` of a task whose containers could not be launched because an image
/// could not be fetched from where the task is placed.
///
/// The error CLASS is the container runtime's `CannotPullContainerError`, which is what
/// Amazon ECS surfaces for a failed fetch; the variable parts are the image the request
/// asked for and the registry that reference resolves to ([`registry_host`]), with the
/// network condition a placement that cannot reach it produces.
pub fn cannot_pull_container_message(image: &str) -> String {
    let host = registry_host(image);
    format!(
        "CannotPullContainerError: failed to resolve ref {image}: failed to do request: \
         Get \"https://{host}/v2/\": dial tcp {host}:443: i/o timeout"
    )
}

/// The `stoppedReason` of a task whose image the registry would not serve to its execution
/// role: the same `CannotPullContainerError` class, with the condition a registry that
/// refuses the pull produces (the runtime's words for a registry answering `403 Forbidden`)
/// and the registry's own refusal as the authorization it was refused by.
pub fn pull_access_denied_message(image: &str, refusal: &str) -> String {
    format!(
        "CannotPullContainerError: failed to resolve ref {image}: pull access denied, \
         repository does not exist or may require authorization: {refusal}"
    )
}

/// How many times the container agent retried the image MANIFEST fetch before it reported
/// the failure, which is the count its account of a reference that resolves to nothing
/// carries ([`image_not_found_message`]).
///
/// A reference the registry ANSWERS for — it holds no such repository, and no such tag or
/// digest in one it does hold — resolves to nothing however often it is asked, so the fetch
/// is retried until the agent's whole retry budget is spent and that budget is what the
/// failure reports. The number is the agent's own, not a quota of the API: between two such
/// failures what differs is the reference, not the count.
const MANIFEST_FETCH_RETRIES: u32 = 7;

/// The `stoppedReason` of a task whose image the registry holds nothing for: the same
/// class, with the condition the container runtime words for a reference that resolves to
/// no content.
///
/// The image's MANIFEST is what is fetched first, and the agent retries that fetch before it
/// gives up ([`MANIFEST_FETCH_RETRIES`]); the runtime then names the reference it could not
/// resolve twice — once as the reference the fetch asked for, and once as the answer the
/// registry gave about it, which for a reference that resolves to nothing is that same
/// reference and `not found`.
pub fn image_not_found_message(image: &str) -> String {
    format!(
        "CannotPullContainerError: pull image manifest has been retried \
         {MANIFEST_FETCH_RETRIES} time(s): failed to resolve ref {image}: {image}: not found"
    )
}

/// The REGISTRY an image reference is fetched from: the part before the first `/` when
/// that part is a host — it carries a `.` or a `:`, or is `localhost` — and
/// [`DOCKER_HUB_REGISTRY`] otherwise, which is how a container runtime resolves a
/// reference that names no registry.
pub fn registry_host(image: &str) -> &str {
    match image.split_once('/') {
        Some((host, _))
            if host.contains('.') || host.contains(':') || host == "localhost" =>
        {
            host
        }
        _ => DOCKER_HUB_REGISTRY,
    }
}

/// Attempt the image fetch of every task that has just been launched, and record what it
/// took and why it failed.
///
/// Called by the operations that LAUNCH a task, and by the clock pump for a task the CLOCK
/// placed — the scheduler's replacement for a task a service lost
/// ([`super::service_steady_state`]), which no call is there to attempt the pull for. Either
/// way with this service's guard dropped: the verdict is a question about the Amazon VPC
/// networking the launch named and about what Amazon ECR serves the task's execution role, so
/// each half is read out of that service's own store through the one shared engine, under that
/// store's lock and no other.
///
/// Nothing is recorded for a fact this emulator does not hold — a subnet the Amazon EC2
/// store does not hold is not evidence of a blocked path, and an engine carrying no Amazon
/// ECR store says nothing about an image — and nothing is attempted twice: a task that
/// already carries its pull instants is not pulled again.
pub fn evaluate_image_pulls<Eng: Send + Sync + 'static>(engine: &Eng) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let launches = state.lock().launches_awaiting_image_pull();
    if launches.is_empty() {
        return;
    }
    // The network half, under the Amazon EC2 store's lock alone: every launch is answered
    // in one pass, so the lock is taken once however many tasks were placed.
    let mut failures: Vec<ImagePullVerdict> = match ::service_com_amazonaws_ec2_amazonec2::state::service_state(engine) {
        Some(ec2) => {
            let store = ec2.lock();
            launches
                .iter()
                .map(|launch| ImagePullVerdict::of(launch, unfetchable_image(&store, launch)))
                .collect()
        }
        None => launches
            .iter()
            .map(|launch| ImagePullVerdict::of(launch, None))
            .collect(),
    };
    // The registry half, with no lock of this service's or Amazon EC2's held: the registry
    // answers each pull itself, which is also what imports an upstream image through a pull
    // through cache rule, and states the digest of the manifest it resolved.
    let now = SystemTime::now();
    for (launch, verdict) in launches.iter().zip(failures.iter_mut()) {
        if verdict.failure.is_none() {
            let fetched = fetch_launch_images(engine, launch, now);
            if let Some((image, reason)) = fetched.refused {
                verdict.failed_image = Some(image);
                verdict.failure = Some(reason);
            }
            verdict.image_digests = fetched.digests;
        }
    }
    // The SECRETS the agent fetches for the task's containers, in the same step and as the
    // same role: "unable to pull secrets or registry auth" is one resource-retrieval step of
    // the agent's, so a task that cannot retrieve one of its secrets fails to start exactly
    // as one that cannot fetch an image does ([`super::container_secrets`]). Decided after
    // the images, with no lock held: the verdict is the owning service's.
    for (launch, failure) in launches.iter().zip(failures.iter_mut()) {
        if failure.failure.is_none() {
            failure.failure = unretrievable_secret(engine, launch);
        }
    }
    // The LOGGER each container's `awslogs` configuration describes, which the agent
    // initialises as that same role before it starts the container: a task whose logger
    // cannot be validated fails to start exactly as one whose execution resources could not
    // be retrieved does ([`super::log_driver_init`]). Decided after them, because the agent
    // retrieves the task's resources and then creates each container, and with no lock held:
    // the verdict is the owning service's.
    for (launch, failure) in launches.iter().zip(failures.iter_mut()) {
        if failure.failure.is_none() {
            failure.failure = uninitialisable_logger(engine, launch);
        }
    }
    state.lock().record_image_pulls(&failures, now);
}

/// What the image fetch of ONE launched task answered: why its containers could not be
/// fetched, and the manifest digest of each image that WAS fetched.
#[derive(Debug, Clone)]
pub struct ImagePullVerdict {
    /// The task the verdict belongs to.
    pub task_arn: String,
    /// The `stoppedReason` of the stop a task whose containers could not be fetched earns,
    /// and `None` when nothing was decided against the fetch.
    pub failure: Option<String>,
    /// The image reference whose fetch is what failed, when the failure is ONE image's: the
    /// container that runs that image is the container the failure is reported on
    /// ([`TaskRecord::launch_failure_container`]). `None` for a verdict that failed on
    /// something other than an image, and for one that failed on nothing.
    pub failed_image: Option<String>,
    /// `(image reference, manifest digest)` for each of the task's images a registry this
    /// emulator holds served: `Container$imageDigest` is "The container image manifest
    /// digest", and the registry that served the pull is what states it.
    pub image_digests: Vec<(String, String)>,
}

impl ImagePullVerdict {
    /// The verdict of `launch` with `failure` — the image whose fetch failed and the reason
    /// it did — recorded, and no image fetched yet.
    fn of(launch: &TaskLaunch, failure: Option<(String, String)>) -> Self {
        let (failed_image, failure) = match failure {
            Some((image, reason)) => (Some(image), Some(reason)),
            None => (None, None),
        };
        Self {
            task_arn: launch.task_arn.clone(),
            failure,
            failed_image,
            image_digests: Vec::new(),
        }
    }
}

/// Fetch each image of `launch` from the registry its reference names, and answer what that
/// took: the FIRST image a registry this emulator holds would not serve with why it could
/// not be fetched, and the manifest digest of each image that was served.
///
/// Two registries are this emulator's to answer for, and an image reference names one of
/// them by its host (the module docs stand this on what each registry documents):
///
/// * an AMAZON ECR private registry, which decides whose registry the reference addresses,
///   the authorization, the resolution of the reference and the upstream image it caches
///   where a pull through cache rule covers the repository — which is why the import happens
///   on the FIRST pull of such a reference and is visible to every later read of that
///   repository, and why a reference into ANOTHER account's registry is answered as that
///   registry's own cross-account refusal rather than resolved against the same-named
///   repository of this account's;
/// * the one public gallery this emulator serves, which resolves a reference into ITS OWN
///   registry alias against the repositories and tags it holds.
///
/// Nothing else is decided: a reference naming an alias that gallery was not assigned names
/// another public registry, and one naming any other registry — or a bare name a container
/// runtime resolves against Docker Hub — names a registry this emulator does not hold, so
/// neither is evidence about an image and the network path above is all that was decided for
/// it.
fn fetch_launch_images<Eng: Send + Sync + 'static>(
    engine: &Eng,
    launch: &TaskLaunch,
    now: SystemTime,
) -> LaunchFetch {
    let mut digests: Vec<(String, String)> = Vec::new();
    for image in &launch.images {
        match fetch_image(engine, launch, image, now) {
            ImageFetch::Undecided => {}
            ImageFetch::Served(digest) => {
                if let Some(digest) = digest {
                    digests.push((image.clone(), digest));
                }
            }
            // The first image that cannot be fetched is what stops the task: its containers
            // are not started, so the images after it are not fetched either.
            ImageFetch::Refused(reason) => {
                return LaunchFetch {
                    refused: Some((image.clone(), reason)),
                    digests,
                }
            }
        }
    }
    LaunchFetch {
        refused: None,
        digests,
    }
}

/// What the fetch of a whole launch's images answered ([`fetch_launch_images`]).
struct LaunchFetch {
    /// `(image reference, reason)` of the FIRST image a registry this emulator holds would
    /// not serve, and `None` where nothing was decided against the fetch.
    refused: Option<(String, String)>,
    /// `(image reference, manifest digest)` for each image such a registry served.
    digests: Vec<(String, String)>,
}

/// What the fetch of ONE image reference answered.
enum ImageFetch {
    /// A registry this emulator holds served the reference, under the manifest digest it
    /// resolved it to — `None` where that registry states no digest for it.
    Served(Option<String>),
    /// The reference names no registry this emulator holds, so nothing is decided about it.
    Undecided,
    /// A registry this emulator holds would not serve it: the `stoppedReason` of the stop
    /// the task earns.
    Refused(String),
}

/// What the fetch of ONE of `launch`'s image references answered ([`fetch_launch_images`]
/// for which registries are answered for).
fn fetch_image<Eng: Send + Sync + 'static>(
    engine: &Eng,
    launch: &TaskLaunch,
    image: &str,
    now: SystemTime,
) -> ImageFetch {
    use ::service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::state::registry_pull;
    use ::service_com_amazonaws_ecrpublic_spencerfrontendservice::state::gallery_pull;

    // Amazon ECR answers for every account's registry its own reading of a reference names,
    // this one's included, so whose registry it is comes back from there rather than being
    // decided here.
    let addressed = reference_into_this_registry(image, &launch.task_arn);
    if let Some(served) = registry_pull::serve_reference_pull(
        engine,
        &addressed,
        launch.execution_role_arn.as_deref(),
        now,
    ) {
        return match served {
            Ok(digest) => ImageFetch::Served(digest),
            Err(registry_pull::PullRefusal::NotAuthorized { message, .. }) => {
                ImageFetch::Refused(pull_access_denied_message(image, &message))
            }
            Err(registry_pull::PullRefusal::NotFound { .. }) => {
                ImageFetch::Refused(image_not_found_message(image))
            }
        };
    }
    // The public gallery answers for the alias it was ASSIGNED: the pull is a question for
    // the registry the reference names, and an alias this gallery was not given names a
    // registry this emulator does not hold.
    match gallery_pull::parse_gallery_reference(image) {
        Some(reference)
            if gallery_pull::serves_registry_alias(engine, &reference.registry_alias) =>
        {
            match gallery_pull::serve_pull(engine, &reference) {
                Ok(digest) => ImageFetch::Served(digest),
                Err(gallery_pull::GalleryPullRefusal::NotFound { .. }) => {
                    ImageFetch::Refused(image_not_found_message(image))
                }
            }
        }
        _ => ImageFetch::Undecided,
    }
}

/// The reference `image` makes into AMAZON ECR, in the AWS-hosted spelling that registry's
/// own reading of a pull reference takes ([`registry_pull::parse_registry_reference`]).
///
/// One registry is addressed by two spellings here, and both name it: AWS's own registry
/// host (`<account>.dkr.ecr.<region>.<suffix>`), and the ADDRESS the registry announces to a
/// client — `Repository$repositoryUri` is "The URI for the repository. You can use this URI
/// for container image push and pull operations", and on this machine that URI is built on
/// an address of the emulator serving the registry, because AWS's own registry host resolves
/// nowhere here ([`addresses_this_emulators_registry`]). A container definition written from
/// what Amazon ECR handed the client therefore carries that address, so it is rewritten into
/// the published spelling and resolved exactly like one that carries it already, rather than
/// read a second time with a reading of this service's own.
///
/// A host that ALREADY names an Amazon ECR registry in AWS's published spelling is left
/// exactly as it is, whichever account it names: such a reference is one that registry reads
/// itself, so rewriting it would move a reference into another account's registry into this
/// account's and serve the same-named repository of ours for it. Which account it names is
/// then Amazon ECR's to decide, as every other part of the pull is. The test for an address
/// of this emulator cannot answer that question on its own, because a registry host's first
/// label is an account id and an all-digit label names no service.
///
/// One address serves every Region's registry, so such a reference names no Region of its
/// own: the Region is the one the TASK runs in, which its own ARN carries. The host is
/// composed with THAT Region's partition suffix, read through Amazon ECR's own reading of it
/// (`registry_dns_suffix`), which is the inverse of the walk that registry resolves the
/// rewritten reference by — so a task of any partition's Region resolves, not only one of the
/// commercial partition.
fn reference_into_this_registry<'image>(
    image: &'image str,
    task_arn: &str,
) -> ::std::borrow::Cow<'image, str> {
    use ::service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::state as ecr_store;

    let Some((authority, path)) = image.split_once('/') else {
        return ::std::borrow::Cow::Borrowed(image);
    };
    if ecr_store::registry_pull::parse_registry_reference(image).is_some()
        || !addresses_this_emulators_registry(authority)
    {
        return ::std::borrow::Cow::Borrowed(image);
    }
    let Some(region) = ::service_common::arn::region_of(task_arn, ARN_NAMESPACE) else {
        return ::std::borrow::Cow::Borrowed(image);
    };
    ::std::borrow::Cow::Owned(format!(
        "{}{ECR_REGISTRY_INFIX}{region}.{}/{path}",
        ecr_store::registry_id(),
        ecr_store::registry_dns_suffix(region)
    ))
}

/// Why the containers of `launch` cannot be launched where the task is placed — the first
/// image no path reaches and the `stoppedReason` that earns — or `None` when every image can
/// be fetched, or when this emulator holds no network facts about the placement at all.
fn unfetchable_image(
    store: &ComAmazonawsEc2Amazonec2State,
    launch: &TaskLaunch,
) -> Option<(String, String)> {
    // The subnet the task's elastic network interface was given, which is the one the
    // launch named first
    // ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::mint_task_attachments`]).
    let subnet_id = launch.subnets.first()?;
    let vpc_id = store.subnets.get(subnet_id)?.vpc_id.clone()?;
    let internet =
        subnet_reaches_internet(store, subnet_id, &vpc_id, launch.assign_public_ip.as_deref());
    launch.images.iter().find_map(|image| {
        let reachable = internet || endpoints_reach_registry(store, &vpc_id, image);
        (!reachable).then(|| (image.clone(), cannot_pull_container_message(image)))
    })
}

/// Whether traffic leaving `subnet_id` reaches the internet, for a task whose
/// `assignPublicIp` is `assign_public_ip`.
///
/// Answered off the route table that APPLIES to the subnet ([`effective_route_table`]) and
/// the default route in it, which is the rule the Amazon VPC User Guide states for every
/// kind of subnet: traffic bound for anything outside the VPC goes where that route sends
/// it, and a subnet whose table has no such route keeps it inside the VPC.
fn subnet_reaches_internet(
    store: &ComAmazonawsEc2Amazonec2State,
    subnet_id: &str,
    vpc_id: &str,
    assign_public_ip: Option<&str>,
) -> bool {
    let Some(table) = effective_route_table(store, subnet_id, vpc_id) else {
        return false;
    };
    table
        .routes
        .iter()
        .flatten()
        .filter(|route| {
            route.destination_cidr_block.as_deref() == Some(DEFAULT_IPV4_DESTINATION)
                // `Route$state` is `blackhole` while "the specified gateway isn't attached
                // to the VPC, or the specified NAT instance has been terminated", and such
                // a route carries nothing.
                && route.state.as_deref() != Some(ROUTE_STATE_BLACKHOLE)
        })
        .any(|route| route_reaches_internet(store, route, assign_public_ip))
}

/// Whether ONE default route carries traffic to the internet, for a task whose
/// `assignPublicIp` is `assign_public_ip`.
fn route_reaches_internet(
    store: &ComAmazonawsEc2Amazonec2State,
    route: &Route,
    assign_public_ip: Option<&str>,
) -> bool {
    if let Some(nat_gateway_id) = route.nat_gateway_id.as_deref() {
        return nat_gateway_reaches_internet(store, nat_gateway_id);
    }
    if route
        .gateway_id
        .as_deref()
        .is_some_and(|gateway_id| gateway_id.starts_with(INTERNET_GATEWAY_ID_PREFIX))
    {
        // An internet gateway carries traffic for an address that is reachable from the
        // internet, which for an `awsvpc` task is the public address `assignPublicIp` asks
        // for — "If you don't specify an IP address ... the task won't have access to the
        // internet" is the same rule the member's own doc states for a task in a public
        // subnet.
        return assign_public_ip == Some(ASSIGN_PUBLIC_IP_ENABLED);
    }
    // A transit gateway carries the traffic off the VPC, to whatever its own route table
    // sends it to.
    route.transit_gateway_id.is_some()
}

/// Whether the NAT gateway `nat_gateway_id` carries internet-bound traffic.
///
/// "With a public NAT gateway, internet-bound traffic from a private subnet can be routed
/// to the NAT gateway ... With a private NAT gateway, internet-bound traffic from a private
/// subnet can't be routed to the gateway" (`CreateNatGateway`), and a gateway that is being
/// deleted, is deleted, or failed to come up carries nothing.
fn nat_gateway_reaches_internet(
    store: &ComAmazonawsEc2Amazonec2State,
    nat_gateway_id: &str,
) -> bool {
    let Some(gateway) = store.nat_gateways.get(nat_gateway_id) else {
        return false;
    };
    let public = gateway
        .connectivity_type
        .as_deref()
        .unwrap_or(CONNECTIVITY_TYPE_PUBLIC)
        == CONNECTIVITY_TYPE_PUBLIC;
    let in_service = matches!(
        gateway.state.as_deref(),
        Some(NAT_GATEWAY_STATE_AVAILABLE | NAT_GATEWAY_STATE_PENDING)
    );
    public && in_service
}

/// Whether the VPC `vpc_id` holds the endpoints that reach the registry `image` is held
/// in, with no route off the VPC at all.
///
/// Only Amazon ECR is reachable this way, and only through all three of the endpoints that
/// service's own documentation names: an interface endpoint for the ECR API, one for its
/// Docker registry, and a gateway endpoint for Amazon S3, which the image layers are
/// fetched from.
fn endpoints_reach_registry(
    store: &ComAmazonawsEc2Amazonec2State,
    vpc_id: &str,
    image: &str,
) -> bool {
    if !is_ecr_image(image) {
        return false;
    }
    [
        (ECR_API_ENDPOINT_SUFFIX, VPC_ENDPOINT_TYPE_INTERFACE),
        (ECR_DKR_ENDPOINT_SUFFIX, VPC_ENDPOINT_TYPE_INTERFACE),
        (S3_ENDPOINT_SUFFIX, VPC_ENDPOINT_TYPE_GATEWAY),
    ]
    .iter()
    .all(|(suffix, endpoint_type)| {
        store.vpc_endpoints.values().any(|endpoint| {
            endpoint.vpc_id.as_deref() == Some(vpc_id)
                && endpoint.vpc_endpoint_type.as_deref() == Some(*endpoint_type)
                && endpoint
                    .service_name
                    .as_deref()
                    .is_some_and(|name| name.ends_with(*suffix))
                // An endpoint Amazon EC2 is still bringing into service is one the VPC
                // has; one that was deleted, rejected or failed is not.
                && matches!(
                    endpoint.state.as_deref(),
                    Some(VPC_ENDPOINT_STATE_AVAILABLE | VPC_ENDPOINT_STATE_PENDING)
                )
        })
    })
}

/// Whether `image` names a repository in Amazon ECR, whose registry is an AWS service and
/// so has VPC endpoints: an ECR image is pulled from `<account>.dkr.ecr.<region>.<suffix>`
/// (Amazon ECR User Guide, the registry URI a pull uses), or from the ADDRESS that registry
/// announces its own repositories at here ([`addresses_this_emulators_registry`]), which
/// names the very same registry.
fn is_ecr_image(image: &str) -> bool {
    let host = registry_host(image);
    (host.contains(ECR_REGISTRY_INFIX) && host.contains(".amazonaws."))
        || addresses_this_emulators_registry(host)
}

/// Whether any of `images` is one a registry serves only to an AUTHENTICATED principal, so
/// that fetching it needs the task execution role Amazon ECS pulls as
/// (`TaskDefinition$executionRoleArn`).
///
/// Amazon ECR is such a registry: a pull from it is two signed calls
/// (`ecr:GetAuthorizationToken` and `ecr:BatchGetImage`), decided over the policies in force
/// on the principal making them. A public registry serves anyone, and a reference naming no
/// registry at all resolves to one ([`DOCKER_HUB_REGISTRY`]), so neither needs a principal —
/// which is why the question is asked of the registry the reference RESOLVES to
/// ([`registry_host`]) rather than of the reference's text.
///
/// The ONE reading both of this service's registration-time rules take: the launch types a
/// revision can run under derive it
/// ([`TaskDefinitionRecord::pull_needs_execution_role`]) and a registration that requires
/// Fargate is refused by it, so the launch types a revision admits and the registrations
/// Amazon ECS accepts cannot disagree. WHETHER a registry serves one particular pull is
/// another question, and Amazon ECR's own to answer about the references it publishes
/// ([`::service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::state::registry_pull`]).
pub fn needs_execution_role_to_pull<'a>(mut images: impl Iterator<Item = &'a str>) -> bool {
    images.any(serves_only_an_authenticated_principal)
}

/// Whether the registry ONE image reference resolves to serves it only to an authenticated
/// principal — the per-reference half of [`needs_execution_role_to_pull`].
fn serves_only_an_authenticated_principal(image: &str) -> bool {
    is_ecr_image(image)
}

/// Whether `host` is an address of THIS EMULATOR rather than the name of a registry
/// published on the internet — which makes it the endpoint Amazon ECR announces its own
/// registry at here.
///
/// `Repository$repositoryUri` is "The URI for the repository. You can use this URI for
/// container image push and pull operations", and the registry is reachable on this machine
/// only where the emulator serves it, so a client that asked that service for the URI is
/// handed an ADDRESS of this emulator and pulls from there. A reference carrying that
/// address therefore names a repository of the account's own registry, exactly as the
/// published `<account>.dkr.ecr.<region>.<suffix>` form does.
///
/// Read through the tree's one reading of the question
/// (`service_common::data_plane_addressing`): a host-shaped value that names no service is
/// an address of this emulator, whichever interface and port it is served on — nothing here
/// reads an environment variable or a bind address, so the answer does not change with how
/// the emulator was started. A reference to a registry of the developer's own addressed by
/// IP reads as this registry too, which is the same answer this emulator gives such an
/// address everywhere else: it is the only registry it serves.
fn addresses_this_emulators_registry(host: &str) -> bool {
    ::service_common::data_plane_addressing::is_host_shaped(host)
        && !::service_common::data_plane_addressing::names_a_service(host)
}

/// The route table that APPLIES to `subnet_id`: the one a caller explicitly associated
/// with it, and otherwise the MAIN table of its VPC.
///
/// "Each subnet in your VPC must be associated with a route table ... A subnet can be
/// explicitly associated with a route table, or implicitly or explicitly associated with
/// the main route table" (Amazon VPC User Guide, "Route tables"), so exactly one table
/// decides where the subnet's traffic goes. The explicit half is Amazon EC2's own reading
/// of its own records
/// ([`ComAmazonawsEc2Amazonec2State::route_table_associated_with_subnet`]), so an
/// association made through that service resolves here exactly as it does there.
fn effective_route_table<'store>(
    store: &'store ComAmazonawsEc2Amazonec2State,
    subnet_id: &str,
    vpc_id: &str,
) -> Option<&'store RouteTable> {
    store
        .route_table_associated_with_subnet(subnet_id)
        .and_then(|route_table_id| store.route_tables.get(&route_table_id))
        .or_else(|| {
            store.route_tables.values().find(|table| {
                table.vpc_id.as_deref() == Some(vpc_id)
                    && table
                        .associations
                        .iter()
                        .flatten()
                        .any(|association| association.main == Some(true))
            })
        })
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Every task whose image fetch has not been attempted yet: one that is on its way UP
    /// (its `desiredStatus` is `RUNNING` and it has not started), whose launch roles have
    /// already been put to the account's IAM records ([`TaskRecord::launch_roles_assumed`])
    /// and that carries no pull instants, with the placement it was launched into, the
    /// images its containers run and the execution role the pull is made as.
    ///
    /// The fetch is made AS the execution role, so a launch whose roles Amazon ECS has not
    /// assumed yet has nothing to fetch with: such a task is left to the pass that follows
    /// the assume rather than fetched for here — which is also what keeps its start withheld
    /// ([`TaskRecord::launch_steps_taken`]).
    ///
    /// A task that shares the networking of the host that runs it carries no placement of
    /// its own, so the PATH half decides nothing for it: the path it fetches an image over
    /// is that host's. The registry half is decided for it like any other task — the image
    /// a reference names is the same image whichever networking the task runs with.
    pub fn launches_awaiting_image_pull(&self) -> Vec<TaskLaunch> {
        self.tasks
            .values()
            .filter(|task| {
                task.launch_failure.is_none()
                    && task.launch_roles_assumed
                    && task.pull_started_at.is_none()
                    && task.desired_status == STATUS_RUNNING
                    && task.is_pending()
            })
            .map(|task| TaskLaunch {
                task_arn: task.task_arn.clone(),
                subnets: task
                    .awsvpc_configuration
                    .as_ref()
                    .map(|awsvpc| awsvpc.subnets.clone())
                    .unwrap_or_default(),
                assign_public_ip: task
                    .awsvpc_configuration
                    .as_ref()
                    .and_then(|awsvpc| awsvpc.assign_public_ip.clone()),
                images: task
                    .containers
                    .iter()
                    .filter_map(|container| container.image.clone())
                    .collect(),
                execution_role_arn: task.execution_role_arn.clone(),
                secrets: self.launched_task_secrets(task),
                log_targets: self.launched_task_log_targets(task),
            })
            .collect()
    }

    /// Record what the image fetch of each named task took, and why it failed where it did
    /// — the `stoppedReason` the stop the clock then owes carries
    /// ([`TaskRecord::fail_launch`]).
    ///
    /// `Task$pullStartedAt` is "the time when the container image pull began" and
    /// `pullStoppedAt` "the time when the container image pull completed": the fetch is made
    /// in this process, so it began and completed at the instant it was attempted, and both
    /// are recorded whether or not it succeeded — a pull that failed was still attempted.
    /// They are recorded for a task whose start fell due while the fetch was being decided
    /// too: the fetch was made for THAT task, so the instants belong on it however far the
    /// clock has since taken it.
    ///
    /// Only a task that is still on its way UP takes a FAILURE: one that has started has
    /// already fetched its images, and one that is stopping is on its way out either way.
    ///
    /// Each container whose image the fetch RESOLVED reports the manifest digest the registry
    /// resolved it to: `Container$imageDigest` is "The container image manifest digest",
    /// which is known from the pull on — and stays absent for an image no registry here
    /// resolved, because no manifest of it was fetched to digest.
    pub fn record_image_pulls(&mut self, verdicts: &[ImagePullVerdict], now: SystemTime) {
        let at = DateTime::from(now);
        for verdict in verdicts {
            if let Some(task) = self.tasks.get_mut(&verdict.task_arn) {
                if task.pull_started_at.is_some() || task.desired_status != STATUS_RUNNING {
                    continue;
                }
                task.pull_started_at = Some(at);
                task.pull_stopped_at = Some(at);
                // The fetch is made for a container Amazon ECS has CREATED on the capacity
                // the task was placed on, so each container reports the runtime id it was
                // created with from here on, whichever way the fetch went
                // ([`ContainerRecord::report_runtime_id`]).
                let task_id = task.task_id.clone();
                for container in &mut task.containers {
                    container.report_runtime_id(&task_id);
                    if let Some((_, digest)) = verdict
                        .image_digests
                        .iter()
                        .find(|(image, _)| Some(image.as_str()) == container.image.as_deref())
                    {
                        container.image_digest = Some(digest.clone());
                    }
                }
                if task.launch_failure.is_none() && task.is_pending() {
                    task.launch_failure = verdict.failure.clone();
                    // A fetch that failed failed for ONE image, and the container that runs
                    // that image is the one Amazon ECS could not create: it is the container
                    // the failure is reported on when the stop falls due
                    // ([`TaskRecord::launch_failure_container`]).
                    task.launch_failure_container =
                        verdict.failed_image.as_deref().and_then(|image| {
                            task.containers
                                .iter()
                                .find(|container| container.image.as_deref() == Some(image))
                                .and_then(|container| container.name.clone())
                        });
                }
            }
        }
    }
}

impl TaskRecord {
    /// Whether Amazon ECS could not launch this task's containers AT ALL, which is exactly
    /// the stop [`Self::fail_launch`] records: a task stopped under
    /// [`STOP_CODE_TASK_FAILED_TO_START`] never ran.
    ///
    /// Read off the stop code the stop carries rather than off the desired status, because a
    /// task Amazon ECS marks for the stop a failed launch earns is on its way out like any
    /// other stopping task — what tells it apart from one the scheduler asked to stop is WHY
    /// it is stopping. This is the failure the deployment circuit breaker counts
    /// ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::deployment_launch_failures`])
    /// and the one a deployment reports as a failed launch.
    pub fn failed_to_launch(&self) -> bool {
        self.stop_code.as_deref() == Some(STOP_CODE_TASK_FAILED_TO_START)
    }

    /// When that launch failed: the instant the stop it earned was recorded at
    /// ([`Self::fail_launch`] stamps it as the task's `stoppingAt`), and `None` for a task
    /// whose launch did not fail.
    pub fn launch_failed_at(&self) -> Option<SystemTime> {
        self.failed_to_launch()
            .then(|| self.stopping_at.or(self.stopped_at))
            .flatten()
            .and_then(|at| SystemTime::try_from(at).ok())
    }

    /// Stop this task because Amazon ECS could not launch its containers at all, as of
    /// `at`: the task never ran, so it gets no `startedAt`, its containers no exit code,
    /// and the stop carries [`STOP_CODE_TASK_FAILED_TO_START`] with `reason` as its
    /// `stoppedReason`.
    ///
    /// The task is on its way out from this instant — `desiredStatus` is `STOPPED`, which
    /// is what a `ListTasks` narrowed to stopped tasks reports it under — and `STOPPED`
    /// itself falls due one [`TRANSITION_SPAN`] later, the same two-step stop every other
    /// stop of a task is recorded as.
    ///
    /// The failure is reported on the CONTAINER it belongs to as well, where the step that
    /// decided it named one ([`TaskRecord::launch_failure_container`]): `Container$reason` is
    /// "a short (1024 max characters) human-readable string to provide additional details
    /// about a running or stopped container", and the additional detail about a container
    /// Amazon ECS could not create is what kept it from being created. A report the AGENT has
    /// made about that container (`SubmitContainerStateChange$reason`) is left as the agent
    /// gave it, and a failure no one container owns — a resource the agent retrieves for the
    /// task as a whole, or a role the launch may not assume — is reported on the task alone.
    pub fn fail_launch(&mut self, reason: &str, at: SystemTime) {
        self.desired_status = STATUS_STOPPED.to_string();
        self.stopped_reason = Some(reason.to_string());
        self.stop_code = Some(STOP_CODE_TASK_FAILED_TO_START.to_string());
        self.stopping_at = Some(DateTime::from(at));
        self.pending_due_at = None;
        self.running_due_at = None;
        self.stopped_due_at = Some(at + TRANSITION_SPAN);
        if let Some(name) = self.launch_failure_container.clone() {
            for container in self
                .containers
                .iter_mut()
                .filter(|container| container.name.as_deref() == Some(name.as_str()))
            {
                if container.reason.is_none() {
                    container.reason = Some(reason.to_string());
                }
            }
        }
        self.version += 1;
    }
}
