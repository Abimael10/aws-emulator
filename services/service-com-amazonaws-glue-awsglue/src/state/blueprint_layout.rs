// SPDX-License-Identifier: CC0-1.0

//! RUNNING a blueprint's LAYOUT: where the layout comes from, what runs it, and what the run
//! of it deploys into the workflow the blueprint run created.
//!
//! "Starts a new run of the specified blueprint" (`StartBlueprintRun`), and a blueprint is
//! "an artifact that contains the business logic needed to create a workflow": its archive is
//! published at `Blueprint$BlueprintLocation` and holds a `blueprint.cfg` naming the
//! `layoutGenerator` — the Python function that, given the run's parameters, ANSWERS the
//! workflow it wants deployed. So a run of a blueprint IS one execution of that function
//! followed by the creation of the entities it answered with, and what the run comes to rest in
//! is what that execution and those creations came to: the succeeded state for a run that
//! created at least one entity, and the failed state carrying the reason for one whose layout
//! could not be read or run, raised, or created nothing at all.
//!
//! That is what this module does, and nothing about it is synthesised:
//!
//! * the archive is READ from the emulator's own Amazon S3, over the framework's in-process
//!   callback — the same transport a client's own S3 request takes, so the bytes that run are
//!   the bytes that were uploaded — and its Python modules are read out of it by the one
//!   reader this workspace has for a .zip package
//!   ([`::service_com_amazonaws_lambda_awsgirapiservice::state::execution::package`]), which
//!   never writes an entry to a host path;
//! * the layout is EXECUTED by the contained Python interpreter this workspace already embeds,
//!   with the blueprint programming model's own classes on its import path ([`library`]) and
//!   with the AWS SDK modules that runtime supplies — so the layout's own `boto3` call reaches
//!   this same emulator;
//! * each entity the layout answered with is CREATED through this emulator's own Glue API, as a
//!   session of the `RoleArn` the start carried ("this role will be assumed by the Glue service
//!   and will be used to create the workflow and other entities of a workflow"), so a job,
//!   crawler or trigger a run reports as created is one `GetJob`, `GetCrawler` or `GetTrigger`
//!   reads back, and one the creating operation's own validation admitted;
//! * what the run REPORTS is what that execution answered: the entities it created, the
//!   exception it raised, or the bound the interpreter stopped it at.
//!
//! # Bounds and containment
//!
//! Neither is here: both are the interpreter's, in host code no layout can reach — the wall
//! clock, step, memory, recursion and output ceilings [`blueprint_layout_budget`] states, the
//! host ceiling the engine stops a run on, and the native-module allow-list of the engine
//! itself. What this module adds is the bound on what ONE run takes in: how large an archive
//! is read ([`MAX_BLUEPRINT_ARCHIVE_BYTES`], enforced by the ranged read), how many entries
//! and inflated bytes are read out of it (the package reader's own
//! [`::service_com_amazonaws_lambda_awsgirapiservice::state::execution::package::MAX_PACKAGE_ENTRIES`]
//! and `MAX_UNZIPPED_BYTES`, so no entry is written to a host path), and how many layouts run
//! at once ([`MAX_CONCURRENT_BLUEPRINT_LAYOUT_RUNS`], taken with no wait).
//!
//! How many entities ONE run deploys is bounded the same way: every creating call the layout
//! makes spends from that run's step budget, and each one is served by the operation that owns
//! it, under that operation's own ceiling on how many records this store holds.

mod library;

use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::package::{
    self, PackageModule,
};
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::python;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    function_sdk_access, AwsAccess, INIT_PHASE_LIMIT,
};

use super::*;

/// How large a blueprint archive this emulator reads.
///
/// CHOSEN, not an AWS limit: the archive is an object a caller published, so what one run takes
/// into this process is bounded here rather than by the object's own size. The bound is enforced
/// by the RANGED read, so nothing larger is ever buffered.
pub const MAX_BLUEPRINT_ARCHIVE_BYTES: usize = 4 * 1024 * 1024;

/// The entry in a blueprint archive that says which function lays the workflow out.
///
/// A blueprint archive carries this file beside its Python modules, and the JSON document in it
/// names the layout generator under [`LAYOUT_GENERATOR_KEY`].
pub const BLUEPRINT_CONFIGURATION_ENTRY: &str = "blueprint.cfg";

/// The member of [`BLUEPRINT_CONFIGURATION_ENTRY`] naming the function that lays the workflow
/// out, as a dotted `<module>.<function>` path. Matched without regard to case, because it is a
/// key of a configuration document a caller wrote.
pub const LAYOUT_GENERATOR_KEY: &str = "layoutgenerator";

/// The longest ONE run of a layout may last in the RUN'S OWN time.
///
/// CHOSEN, not an AWS duration: Glue publishes no span for laying a workflow out. It is a span
/// of the run's own time — spent by the waits the layout asks for and by one tick per traced
/// event it delivers ([`BLUEPRINT_LAYOUT_CLOCK_TICK`]) and by nothing of this host's speed — so
/// what the blueprint run reports is the same on an idle host and on a loaded one.
pub const BLUEPRINT_LAYOUT_RUN_CEILING: Duration = Duration::from_secs(20);

/// How far ONE reading of a layout run's clock moves it. Sized as the job script's is, so the
/// whole step budget spends well under the span above.
pub const BLUEPRINT_LAYOUT_CLOCK_TICK: Duration = Duration::from_nanos(100);

/// How much of THIS HOST's own time ONE run of a layout may take, past which the engine stops
/// it whatever its own clock reads — generous beside the engine's own phases, for the reason
/// [`super::JOB_SCRIPT_HOST_CEILING`] gives.
pub const BLUEPRINT_LAYOUT_HOST_CEILING: Duration = INIT_PHASE_LIMIT
    .saturating_add(BLUEPRINT_LAYOUT_RUN_CEILING)
    .saturating_mul(3);

/// How long the call that performs ONE layout waits for the thread it runs on: the host ceiling
/// plus the grace the engine's own stop needs to land.
pub const BLUEPRINT_LAYOUT_HOST_WAIT: Duration =
    BLUEPRINT_LAYOUT_HOST_CEILING.saturating_add(Duration::from_secs(5));

/// How long ONE REQUEST may be held open by the deployment it drives, whatever the layout
/// does.
///
/// The bound the handler applies to the whole deployment — the archive read and the layout
/// run together — beside the bounds inside it ([`blueprint_layout_budget`],
/// [`BLUEPRINT_LAYOUT_HOST_CEILING`], [`BLUEPRINT_LAYOUT_HOST_WAIT`],
/// [`MAX_CONCURRENT_BLUEPRINT_LAYOUT_RUNS`]). It is generous past the wait the layout itself is
/// given, so a deployment the engine has already stopped is reported on what it came to rather
/// than on this ceiling; a deployment still unanswered here is one the request gives up on, and
/// the run records that ([`blueprint_layout_call_ceiling_reached`]). The interpreter thread that
/// outlives it keeps its permit until it really ends, which is what keeps the count of layouts
/// running at once inside its bound.
pub const BLUEPRINT_LAYOUT_CALL_CEILING: Duration =
    BLUEPRINT_LAYOUT_HOST_WAIT.saturating_add(Duration::from_secs(10));

/// `BlueprintRun$ErrorMessage` of a run whose deployment this emulator stopped waiting for.
///
/// It states that this emulator gave up rather than anything about the blueprint: nothing of
/// the layout's own budget ran out, so the run claims no outcome for work this call never saw
/// the end of.
#[must_use]
pub fn blueprint_layout_call_ceiling_reached() -> String {
    format!(
        "This emulator stopped waiting for the blueprint's layout after {} seconds, so what it \
         deployed is not known.",
        BLUEPRINT_LAYOUT_CALL_CEILING.as_secs()
    )
}

/// How many traced events one layout may execute.
///
/// CHOSEN: large enough for a layout that builds a workflow's entities and for the modules it
/// imports to be stood up, and small enough that a loop with no exit ends well inside the host
/// time one run may take, so such a loop is answered by THIS budget — a count of the layout's
/// own work, the same on any host.
pub const MAX_BLUEPRINT_LAYOUT_STEPS: u64 = 2_000_000;

/// How much of a layout's output is kept: a blueprint run reports no layout output, so this
/// only bounds what one run holds in memory.
const MAX_BLUEPRINT_LAYOUT_LOG_BYTES: usize = 64 * 1024;

/// How many layouts run at once in this process.
///
/// CHOSEN: a layout runs per blueprint run and the call that started the run WAITS for it, so
/// this is what keeps the interpreters one process stands up for layouts bounded however many
/// runs are started beside each other. The permit is taken with NO wait: a start that finds
/// none free fails its run rather than queueing behind this bound.
pub const MAX_CONCURRENT_BLUEPRINT_LAYOUT_RUNS: usize = 4;

/// The permits [`MAX_CONCURRENT_BLUEPRINT_LAYOUT_RUNS`] hands out, for the whole process.
///
/// Held as an owned permit MOVED INTO the thread the layout runs on, so it is given back when
/// that thread really ends — including for a run the caller has given up waiting for.
static BLUEPRINT_LAYOUT_PERMITS: ::std::sync::LazyLock<::std::sync::Arc<::tokio::sync::Semaphore>> =
    ::std::sync::LazyLock::new(|| {
        ::std::sync::Arc::new(::tokio::sync::Semaphore::new(
            MAX_CONCURRENT_BLUEPRINT_LAYOUT_RUNS,
        ))
    });

/// The stack a layout's thread is given: the one every run of caller-supplied Python this crate
/// performs is given ([`super::JOB_SCRIPT_STACK_BYTES`]).
const BLUEPRINT_LAYOUT_STACK_BYTES: usize = JOB_SCRIPT_STACK_BYTES;

/// The name the session this emulator opens to create a blueprint run's entities is opened
/// under, which is the principal a service names when it refuses one of those calls.
pub const BLUEPRINT_RUN_ROLE_SESSION_NAME: &str = "GlueBlueprintRunnerSession";

/// What one layout run may spend, beside the step, memory, recursion, log and answer ceilings
/// every run of caller-supplied Python here is given.
#[must_use]
pub fn blueprint_layout_budget() -> python::Budget {
    python::Budget {
        wall_clock: BLUEPRINT_LAYOUT_RUN_CEILING,
        steps: MAX_BLUEPRINT_LAYOUT_STEPS,
        memory_bytes: MAX_JOB_SCRIPT_MEMORY_BYTES,
        // What a layout may write to the `/tmp` of the environment it runs in, bounded like
        // every other run of caller-supplied Python this crate performs.
        ephemeral_bytes: MAX_JOB_SCRIPT_SCRATCH_BYTES,
        log_bytes: MAX_BLUEPRINT_LAYOUT_LOG_BYTES,
        response_bytes: MAX_JOB_SCRIPT_ANSWER_BYTES,
        recursion_limit: MAX_JOB_SCRIPT_RECURSION,
    }
}

/// Everything one run of a blueprint's layout needs, copied out of the store under its guard so
/// the run itself holds none.
#[derive(Debug, Clone)]
pub struct BlueprintLayoutPlan {
    /// The blueprint being run.
    pub blueprint_name: String,
    /// The id of the run whose layout this is.
    pub run_id: String,
    /// The workflow the run created and deploys into, which is also what its triggers are
    /// attached to.
    pub workflow_name: String,
    /// `Blueprint$BlueprintLocation`: the Amazon S3 path of the published archive.
    pub blueprint_location: String,
    /// `BlueprintRun$RoleArn`: the role the run's own creating calls are made as.
    pub role_arn: String,
    /// `BlueprintRun$Parameters`: "the blueprint parameters as a string", exactly as the start
    /// sent them, which is what the layout reads its own parameters out of.
    pub parameters: Option<String>,
}

/// What a run of a blueprint's layout came to.
#[derive(Debug, Clone)]
pub enum BlueprintLayoutOutcome {
    /// The layout ran and these entities were created, in creation order.
    Deployed {
        /// The jobs the run created.
        jobs: Vec<String>,
        /// The crawlers the run created.
        crawlers: Vec<String>,
        /// The triggers the run created.
        triggers: Vec<String>,
    },
    /// The layout ran and created no entity at all, so there is no workflow to run.
    NothingDeployed,
    /// NOTHING ANSWERED the read of the archive — no object store is mounted to ask, or the
    /// call could not be dispatched — so nothing is established about the layout: there is no
    /// archive to run and no fault of the run's own either.
    ///
    /// The one reading of an object store's answer this tree makes says exactly that about a
    /// failure carrying no error code
    /// ([`::service_common::s3_object_read::ObjectRead::Unanswered`]), so a run is not failed
    /// for it — the same way a registration pointed at an archive nothing answered about is
    /// not failed for it ([`super::read_blueprint_archive`]).
    NotEstablished,
    /// The layout could not be read or run, raised, or a creation was refused: the reason,
    /// which is what the run then carries as its `ErrorMessage`.
    Failed(String),
}

impl BlueprintLayoutOutcome {
    /// How many entities the layout created.
    #[must_use]
    pub fn created(&self) -> usize {
        match self {
            Self::Deployed {
                jobs,
                crawlers,
                triggers,
            } => jobs.len() + crawlers.len() + triggers.len(),
            Self::NothingDeployed | Self::NotEstablished | Self::Failed(_) => 0,
        }
    }
}

/// Run the layout `plan` names and answer what deploying it came to.
///
/// Nothing of the store is held while this runs: the plan was copied out under the guard, the
/// archive is read and the layout executed without it, and the outcome is recorded under the
/// guard again by the caller.
pub async fn perform_blueprint_layout<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    plan: &BlueprintLayoutPlan,
) -> BlueprintLayoutOutcome {
    // The credential the run's own CREATING calls are made with: a session of the role the
    // start named ("this role will be assumed by the Glue service and will be used to create
    // the workflow and other entities of a workflow"). The layout never sees it.
    let access = function_sdk_access(
        engine,
        ctx,
        region,
        &plan.role_arn,
        BLUEPRINT_RUN_ROLE_SESSION_NAME,
    );
    // The ARCHIVE is not read as that role: a blueprint's archive is fetched by the
    // REGISTRATION — "specifies a path in Amazon S3 where the blueprint is copied when you
    // call CreateBlueprint/UpdateBlueprint to register the blueprint in Glue"
    // (`Blueprint$BlueprintServiceLocation`) — so what a run reads is the registered
    // blueprint rather than the caller's object, and the read is made with the credential the
    // framework hands this emulator's handlers, as every other in-process read between two
    // emulated services is (the same read the registration makes,
    // [`super::read_blueprint_archive`]).
    let archive = match read_blueprint_archive_bytes(ctx.callback(), region, &plan.blueprint_location)
        .await
    {
        ArchiveRead::Read(archive) => archive,
        ArchiveRead::Unanswered => return BlueprintLayoutOutcome::NotEstablished,
        ArchiveRead::Unreadable(reason) => return BlueprintLayoutOutcome::Failed(reason),
    };
    let modules = match layout_modules(&archive) {
        Ok(modules) => modules,
        Err(reason) => return BlueprintLayoutOutcome::Failed(reason),
    };
    let generator = match layout_generator(&archive) {
        Ok(generator) => generator,
        Err(reason) => return BlueprintLayoutOutcome::Failed(reason),
    };
    // Taken with no wait, before anything is spawned: a start that finds no free interpreter
    // fails its run rather than holding the call open behind the bound.
    let Ok(permit) = ::std::sync::Arc::clone(&BLUEPRINT_LAYOUT_PERMITS).try_acquire_owned() else {
        return BlueprintLayoutOutcome::Failed(format!(
            "This emulator was already running the {MAX_CONCURRENT_BLUEPRINT_LAYOUT_RUNS} \
             blueprint layouts it runs at a time, so the layout of this run was not performed."
        ));
    };
    run_layout(plan, region, &generator, modules, access, permit).await
}

/// What the read of a published blueprint archive established.
enum ArchiveRead {
    /// The object store served the archive, and these are its bytes.
    Read(Vec<u8>),
    /// Nothing answered the read, so nothing is established about the archive.
    Unanswered,
    /// The store answered about the archive and it cannot be read through it, or the location
    /// names no object at all: the reason, worded as the run reports it.
    Unreadable(String),
}

/// The archive published at `location`.
///
/// The read is RANGED, so at most [`MAX_BLUEPRINT_ARCHIVE_BYTES`] plus the one byte that shows
/// the object is past the bound is ever buffered. What the store ANSWERED is read through the
/// tree's one reading of an object store's answer
/// ([`::service_common::s3_object_read::answered_refusal`]), so this read and every other read
/// this service makes of an object agree about what an answer MEANT — and a failure carrying no
/// answer at all establishes nothing.
async fn read_blueprint_archive_bytes(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    location: &str,
) -> ArchiveRead {
    let Some((bucket, key)) = split_s3_location(location) else {
        return ArchiveRead::Unreadable(blueprint_archive_unreadable(
            location,
            "the location does not name an Amazon S3 object",
        ));
    };
    if key.is_empty() || key.ends_with('/') {
        return ArchiveRead::Unreadable(blueprint_archive_unreadable(
            location,
            "the location does not name an Amazon S3 object",
        ));
    }
    let client = ::service_common::s3_client::s3_client(callback, region);
    let object = match client
        .get_object()
        .bucket(bucket)
        .key(key)
        .range(format!("bytes=0-{MAX_BLUEPRINT_ARCHIVE_BYTES}"))
        .send()
        .await
    {
        Ok(object) => object,
        Err(error) => {
            if matches!(
                ::service_common::s3_object_read::answered_refusal(&error),
                ::service_common::s3_object_read::ObjectRead::Unanswered
            ) {
                return ArchiveRead::Unanswered;
            }
            return ArchiveRead::Unreadable(blueprint_archive_unreadable(
                location,
                &format!(
                    "the object store answered {:?}: {}",
                    error.code(),
                    ::aws_smithy_types::error::display::DisplayErrorContext(&error)
                ),
            ));
        }
    };
    let body = match object.body.collect().await {
        Ok(body) => body.into_bytes(),
        Err(error) => {
            return ArchiveRead::Unreadable(blueprint_archive_unreadable(
                location,
                &format!("the archive did not arrive in full: {error}"),
            ));
        }
    };
    if body.len() > MAX_BLUEPRINT_ARCHIVE_BYTES {
        return ArchiveRead::Unreadable(blueprint_archive_unreadable(
            location,
            &format!(
                "the archive is past the {MAX_BLUEPRINT_ARCHIVE_BYTES} bytes one blueprint \
                 archive may hold"
            ),
        ));
    }
    ArchiveRead::Read(body.to_vec())
}

/// The modules the layout run is given: the archive's own Python modules, the blueprint
/// programming model's classes ([`library`]) and the entry module that runs the layout.
fn layout_modules(archive: &[u8]) -> Result<Vec<PackageModule>, String> {
    let held = package::python_modules(Some(archive)).map_err(|fault| match fault {
        package::PackageFault::NotHeld => {
            "The blueprint archive holds no bytes to read.".to_string()
        }
        package::PackageFault::Unreadable(detail) => format!(
            "The blueprint archive could not be read as a .zip archive: {detail}"
        ),
    })?;
    Ok(::std::iter::once(PackageModule {
        path: format!("{LAYOUT_ENTRY_MODULE}.py"),
        source: LAYOUT_ENTRY_SOURCE.to_string(),
    })
    .chain(library::blueprint_library_modules())
    .chain(held)
    .collect())
}

/// The dotted `<module>.<function>` path of the layout generator, read out of the archive's
/// own [`BLUEPRINT_CONFIGURATION_ENTRY`].
fn layout_generator(archive: &[u8]) -> Result<String, String> {
    let mut budget = package::ReadBudget::default();
    let entries = package::python_resources_within(Some(archive), &mut budget).map_err(|fault| {
        match fault {
            package::PackageFault::NotHeld => {
                "The blueprint archive holds no bytes to read.".to_string()
            }
            package::PackageFault::Unreadable(detail) => format!(
                "The blueprint archive could not be read as a .zip archive: {detail}"
            ),
        }
    })?;
    let configuration = entries
        .iter()
        .find(|entry| {
            entry
                .path
                .rsplit('/')
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(BLUEPRINT_CONFIGURATION_ENTRY))
        })
        .ok_or_else(|| {
            format!(
                "The blueprint archive carries no {BLUEPRINT_CONFIGURATION_ENTRY}, so the \
                 function that lays the workflow out is not named."
            )
        })?;
    let document: serde_json::Value = serde_json::from_str(&configuration.source).map_err(|error| {
        format!("The blueprint's {BLUEPRINT_CONFIGURATION_ENTRY} is not a JSON document: {error}")
    })?;
    let named = document
        .as_object()
        .into_iter()
        .flatten()
        .find(|(key, _)| key.to_ascii_lowercase() == LAYOUT_GENERATOR_KEY)
        .and_then(|(_, value)| value.as_str())
        .map(str::trim)
        .filter(|generator| generator.contains('.'))
        .ok_or_else(|| {
            format!(
                "The blueprint's {BLUEPRINT_CONFIGURATION_ENTRY} names no layoutGenerator as a \
                 <module>.<function> path, so there is no layout to run."
            )
        })?;
    Ok(named.to_string())
}

/// `BlueprintRun$ErrorMessage` of a run whose archive could not be read: what was being read
/// and what the read established.
///
/// Worded as the registration's own refusal of an unreadable location is, so a caller reads
/// the same two parts — the location, and what happened — wherever the archive is read.
fn blueprint_archive_unreadable(location: &str, detail: &str) -> String {
    format!("Unable to read the blueprint archive from {location}: {detail}")
}

/// Run the layout on a thread of its own and answer what deploying it came to.
///
/// The interpreter is synchronous and CPU-bound, so it runs on a thread with a stack of its own
/// rather than on the async runtime, and the wait for it is the host backstop
/// ([`BLUEPRINT_LAYOUT_HOST_WAIT`]). The span the engine bounds the run by is read against a
/// clock the RUN drives, so what the blueprint run reports is decided by what the layout did and
/// not by how fast this host happened to be running it.
async fn run_layout(
    plan: &BlueprintLayoutPlan,
    region: &str,
    generator: &str,
    modules: Vec<PackageModule>,
    access: Option<AwsAccess>,
    permit: ::tokio::sync::OwnedSemaphorePermit,
) -> BlueprintLayoutOutcome {
    let budget = blueprint_layout_budget();
    let event = ::serde_json::json!({
        "generator": generator,
        // "The blueprint parameters as a string": the layout's own `user_params`, which is the
        // document the start carried. A start that named no workflow of its own still has one
        // — the workflow this run created — so the layout can read it under the key the
        // blueprint programming model takes it under.
        "user_params": layout_user_parameters(plan),
        // "system_params: contains two properties, region and accountId".
        "system_params": {
            "region": region,
            "accountId": ::engine::emulator_account_id(),
        },
        "workflow_name": plan.workflow_name,
    })
    .to_string();
    let handler = format!("{LAYOUT_ENTRY_MODULE}.{LAYOUT_ENTRY_FUNCTION}");
    let run_id = plan.run_id.clone();
    let (sender, receiver) = ::tokio::sync::oneshot::channel();
    let spawned = ::std::thread::Builder::new()
        .name("glue-blueprint-layout".to_string())
        .stack_size(BLUEPRINT_LAYOUT_STACK_BYTES)
        .spawn(move || {
            // Held for as long as the layout runs, whoever is still waiting for it.
            let _permit = permit;
            let finished = python::run_against(
                &python::Run {
                    modules: &modules,
                    handler: &handler,
                    // A layout reads its parameters off the event it is given, so the run is
                    // given no environment of its own.
                    environment: &::std::collections::BTreeMap::new(),
                    architecture:
                        ::service_com_amazonaws_lambda_awsgirapiservice::state::DEFAULT_ARCHITECTURE,
                    event: Some(&event),
                    request_id: &run_id,
                    invoked_function_arn: "",
                    client_context: None,
                    // A blueprint run carries no tenant member in the model. What scopes the
                    // run is passed beside it: the role session its own AWS calls are made as,
                    // built from the start's `RoleArn`.
                    tenant_id: None,
                    budget,
                    aws: access.as_ref(),
                    // A layout is no Lambda durable execution's run.
                    durable: None,
                },
                &python::Clock::driven(BLUEPRINT_LAYOUT_CLOCK_TICK)
                    .waiting()
                    .within(BLUEPRINT_LAYOUT_HOST_CEILING),
            );
            let _ = sender.send(finished);
        });
    if let Err(error) = spawned {
        return BlueprintLayoutOutcome::Failed(format!(
            "the thread the blueprint layout runs on could not be started: {error}"
        ));
    }
    match ::tokio::time::timeout(BLUEPRINT_LAYOUT_HOST_WAIT, receiver).await {
        Ok(Ok(finished)) => read_layout_outcome(&finished.completion),
        Ok(Err(_)) => BlueprintLayoutOutcome::Failed(
            "the thread the blueprint layout ran on ended without reporting".to_string(),
        ),
        // The thread has still not answered a grace period past the host time one run may take
        // — which the engine has by then stopped the run on — so it is given up on and the run
        // reports that this emulator has no outcome for it.
        Err(_elapsed) => BlueprintLayoutOutcome::Failed(
            "This emulator gave up waiting for the blueprint's layout, so what it created is \
             not known."
                .to_string(),
        ),
    }
}

/// The `user_params` the layout is called with: the document the start's `Parameters` carried,
/// with the workflow this run created named under
/// [`BLUEPRINT_WORKFLOW_NAME_PARAMETER`] when the start named none.
///
/// A start whose `Parameters` are no JSON object carry no parameter the layout could read, so
/// the layout is given the one parameter the blueprint programming model always has: the name
/// of the workflow it is creating.
fn layout_user_parameters(plan: &BlueprintLayoutPlan) -> serde_json::Value {
    let mut parameters = plan
        .parameters
        .as_deref()
        .and_then(|parameters| serde_json::from_str::<serde_json::Value>(parameters).ok())
        .and_then(|parameters| match parameters {
            serde_json::Value::Object(members) => Some(members),
            _ => None,
        })
        .unwrap_or_default();
    parameters
        .entry(BLUEPRINT_WORKFLOW_NAME_PARAMETER.to_string())
        .or_insert_with(|| serde_json::Value::String(plan.workflow_name.clone()));
    serde_json::Value::Object(parameters)
}

/// What the blueprint run reports for a layout the interpreter answered for.
fn read_layout_outcome(completion: &python::Completion) -> BlueprintLayoutOutcome {
    match completion {
        python::Completion::Returned(answer) => read_created(answer),
        // A layout answers with what it created, so a streamed answer carries no such record:
        // the run reports that nothing is known about what was created rather than a success.
        python::Completion::Streamed { .. } => BlueprintLayoutOutcome::Failed(
            "The blueprint's layout did not report the entities it created.".to_string(),
        ),
        python::Completion::Raised {
            error_type,
            error_message,
            ..
        } => BlueprintLayoutOutcome::Failed(if error_type == DEPLOYMENT_REFUSED_ERROR {
            // A creation the deployment made was REFUSED, and what the run reports is the
            // service's own refusal, with nothing added to it: the message the operation
            // that refused the entity worded ([`library`]).
            error_message.clone()
        } else if error_message.is_empty() {
            error_type.clone()
        } else {
            format!("{error_type}: {error_message}")
        }),
        python::Completion::Stopped(stop) => {
            BlueprintLayoutOutcome::Failed(blueprint_layout_bound_reached(*stop))
        }
        python::Completion::Internal(detail) => BlueprintLayoutOutcome::Failed(detail.clone()),
    }
}

/// The entities the layout ANSWERED with having created, read off its answer.
///
/// A layout that created nothing is the one case AWS fails a run that ran: there is no workflow
/// to run, so the run is not reported as having deployed one.
fn read_created(answer: &str) -> BlueprintLayoutOutcome {
    let Ok(answered) = serde_json::from_str::<serde_json::Value>(answer) else {
        return BlueprintLayoutOutcome::Failed(
            "The blueprint's layout did not report the entities it created.".to_string(),
        );
    };
    let named = |member: &str| -> Vec<String> {
        answered
            .get(member)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|name| name.as_str().map(str::to_string))
            .collect()
    };
    let deployed = BlueprintLayoutOutcome::Deployed {
        jobs: named("jobs"),
        crawlers: named("crawlers"),
        triggers: named("triggers"),
    };
    if deployed.created() == 0 {
        return BlueprintLayoutOutcome::NothingDeployed;
    }
    deployed
}

/// `BlueprintRun$ErrorMessage` of a run whose layout the interpreter stopped at one of this
/// emulator's bounds: which bound it was, named as this emulator's own.
fn blueprint_layout_bound_reached(stop: python::Stop) -> String {
    let bound = match stop {
        python::Stop::Deadline => "the time this emulator gives a blueprint layout",
        python::Stop::Steps => "the step budget this emulator gives a blueprint layout",
        python::Stop::Memory => "the memory ceiling this emulator gives a blueprint layout",
        python::Stop::Response => "the size of the value a blueprint layout may answer with",
        python::Stop::HostCeiling => {
            "the host time this emulator gives one run of a blueprint layout"
        }
    };
    format!("The blueprint's layout was stopped at {bound}, so the workflow was not deployed.")
}

/// The module this emulator runs a blueprint's layout FROM, and the function in it the engine
/// calls.
const LAYOUT_ENTRY_MODULE: &str = "__glue_blueprint_layout";
/// See [`LAYOUT_ENTRY_MODULE`].
const LAYOUT_ENTRY_FUNCTION: &str = "run";

/// The exception the deployment raises for a creation the SERVICE refused, whose message is
/// the service's own ([`library`]): a run that fails on one reports that message and nothing
/// added to it.
const DEPLOYMENT_REFUSED_ERROR: &str = "DeploymentRefused";

/// The Python source of that module: it calls the archive's own layout generator with the run's
/// parameters and CREATES each entity the workflow it answered with carries, through this
/// emulator's own Glue API, answering the names it created.
///
/// Every call it makes is a real API call of this emulator, signed as the session of the run's
/// `RoleArn`, so an entity it reports as created is one that operation stored and one a later
/// read of that operation reports. A refusal travels out as the SDK's own error, which is what
/// the run then carries as its `ErrorMessage`.
const LAYOUT_ENTRY_SOURCE: &str = r#"
"""Runs a blueprint's own layout and creates the entities the workflow it returned carries."""

import importlib

import boto3

from awsglue.blueprint import _deployment


def _generator(named):
    """The layout generator `blueprint.cfg` names, imported out of the archive's modules."""
    module_name, _, attribute = named.rpartition(".")
    if not module_name or not attribute:
        raise ValueError("the blueprint's layoutGenerator is not a <module>.<function> path")
    return getattr(importlib.import_module(module_name), attribute)


def run(event, context):
    generator = _generator(event["generator"])
    # "generate_layout(user_params, system_params)": the parameters the run carried, and the
    # Region and account the run is performed in.
    workflow = generator(event["user_params"], event["system_params"])
    if workflow is None:
        raise ValueError("the blueprint's layout returned no workflow")
    plan = _deployment.deployment(workflow, event["workflow_name"])
    glue = boto3.client("glue")
    created = {"jobs": [], "crawlers": [], "triggers": []}
    for job in plan["jobs"]:
        created["jobs"].append(_deployment.created(glue.create_job, job))
    for crawler in plan["crawlers"]:
        created["crawlers"].append(_deployment.created(glue.create_crawler, crawler))
    for trigger in plan["triggers"]:
        created["triggers"].append(_deployment.created(glue.create_trigger, trigger))
    # The workflow itself was created by the run before the layout ran, so the members the
    # layout states about it are applied to it here.
    if plan["workflow"]:
        _deployment.created(
            glue.update_workflow, dict(plan["workflow"], Name=event["workflow_name"])
        )
    return created
"#;
