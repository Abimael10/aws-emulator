// SPDX-License-Identifier: CC0-1.0

//! The PUBLICATION of what a training run produced: the model artifact and the checkpoints it
//! built, and the instants the clock owes their appearance under the job's locations.
//!
//! "Uploading - Training is complete and the model artifacts are being uploaded to the S3
//! location" (`DescribeTrainingJob$SecondaryStatus`): the artifact appears under
//! `OutputDataConfig.S3OutputPath` once THAT stage is over and not before it. A client that
//! polls `SecondaryStatus` to decide when the artifact exists — a download, packaging or
//! deployment chain — reads the two in that order, so an object present while the job is still
//! `Starting` would tell it the artifact is ready before the job says so. The checkpoints a
//! job asked for belong to the stage that TRAINS, which is the stage a container writes them
//! during (`CheckpointConfig`), so they appear from that stage on.
//!
//! The run itself is performed inside the create that accepted the job
//! ([`super::run_training_job`]), which is earlier than the stages that publish what it
//! produced. So what the run built is HELD on the job
//! ([`super::TrainingJob::pending_artifact`], [`super::TrainingJob::pending_checkpoints`]) and
//! written when its own stage falls due:
//!
//! * [`publish_when_due`] arms those instants from the create, because a client that submits a
//!   job and then watches the output prefix may never call this service again;
//! * [`publish_due_artifact`] makes the publications a read finds already due, which is the
//!   same materialisation [`super::TrainingJob::settle_status`] makes of the stages themselves.
//!
//! Both claim the one held object ([`super::TrainingJob::claim_due_artifact`],
//! [`super::TrainingJob::claim_due_checkpoints`]), so whichever gets there first writes it
//! exactly once, and a run whose job was asked to stop before the stage fell due publishes
//! nothing at all: the stop abandons the run, so what it produced is never revealed.

use super::*;

/// One object a finished run produced, waiting for the stage that publishes it: the model
/// artifact, or one of the checkpoints the job asked for.
///
/// The bytes are the ones the run built ([`super::model_archive`] for the artifact) and the
/// location is where the member that reports it says it goes; both are decided by the run, so
/// the publication writes what the run produced rather than building anything of its own.
#[derive(Debug, Clone)]
pub struct PendingArtifact {
    /// Where the object goes: `<S3OutputPath>/<TrainingJobName>/output/model.tar.gz` for the
    /// model artifact, and under `CheckpointConfig.S3Uri` for a checkpoint.
    pub location: S3Location,
    /// The bytes the run built, as the object's body.
    pub archive: Vec<u8>,
}

/// How long past the due instant a publication armed by the create is attempted, so a task
/// woken a hair early still finds the stage due.
const DUE_SLACK: Duration = Duration::from_millis(50);

/// Arm the publication of one job's artifact for the instant its upload stage falls due.
///
/// Returns at once: the write happens in a task of its own, because the create has to answer
/// while the job is still `InProgress` and the artifact is not published until the stage that
/// uploads it. The task waits no longer than the span the whole of a job's work is given
/// ([`super::TRAINING_JOB_ENDS_AFTER`]), so a clock jump cannot leave it sleeping, and it
/// claims the same held archive a read claims.
///
/// One task is armed per ACCEPTED job and holds nothing but that wait and the job's name —
/// the archive stays in the store until it is claimed — so what is in flight is bounded by the
/// jobs the store admits at all ([`super::TRAINING_JOB_QUOTA`]).
pub fn publish_when_due<Eng: Send + Sync + 'static>(
    engine: ::std::sync::Arc<Eng>,
    callback: smithy_server_runtime::EmbeddedEmulator,
    region: String,
    job_name: String,
    due_at: SystemTime,
) {
    ::tokio::spawn(async move {
        let wait = due_at
            .duration_since(SystemTime::now())
            .unwrap_or_default()
            .saturating_add(DUE_SLACK)
            .min(super::TRAINING_JOB_ENDS_AFTER.saturating_add(DUE_SLACK));
        ::tokio::time::sleep(wait).await;
        publish_due_artifact(&*engine, &callback, &region, SystemTime::now(), &job_name).await;
    });
}

/// Make the publication of `job_name`'s artifact that the CLOCK has already made due.
///
/// Every read of a training job calls this before it reports: the object was owed at this
/// instant whether or not anybody polled, so writing it changes nothing a caller could not
/// have observed by waiting — and it is what makes the artifact present for the read that
/// reports the stage which uploads it. A job with nothing held, or whose stage is not due yet,
/// is left exactly as it was found.
///
/// The store's guard is given up before Amazon S3 is called, so this service's state is never
/// held across a call into another service's.
pub async fn publish_due_artifact<Eng: Send + Sync + 'static>(
    engine: &Eng,
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    now: SystemTime,
    job_name: &str,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let (claimed, checkpoints) = {
        let mut store = state.lock();
        match store.training_jobs.get_mut(job_name) {
            Some(job) => (job.claim_due_artifact(now), job.claim_due_checkpoints(now)),
            None => (None, Vec::new()),
        }
    };
    let client = s3_client_through(callback, region);
    // The checkpoints belong to the TRAINING stage, which falls due before the one that
    // uploads the artifact, so they are written first — and a write Amazon S3 refuses ends the
    // publication there: the job has failed on that leg, so the later legs never happen and the
    // artifact of a failed job is not left behind either.
    for checkpoint in checkpoints {
        let written = write_object(&client, &checkpoint.location, checkpoint.archive).await;
        if let Err(detail) = written {
            let reason = checkpoint_upload_failed(&checkpoint.location.uri(), &detail);
            if let Some(job) = state.lock().training_jobs.get_mut(job_name) {
                job.record_artifact_failure(reason);
            }
            return;
        }
    }
    let Some(artifact) = claimed else {
        return;
    };
    let written = write_object(&client, &artifact.location, artifact.archive).await;
    if let Err(detail) = written {
        // The upload leg of the run failed, which is the JOB's own failure: the reason names
        // that leg, the same way a read the run could not make names the download leg.
        let reason = model_upload_failed(&artifact.location.uri(), &detail);
        if let Some(job) = state.lock().training_jobs.get_mut(job_name) {
            job.record_artifact_failure(reason);
        }
    }
}
