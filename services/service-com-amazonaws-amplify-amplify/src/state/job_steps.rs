// SPDX-License-Identifier: CC0-1.0

//! The execution STEPS a settled job reports, and the per-step output each one publishes
//! a URL for.
//!
//! `Job$steps` is "the execution steps for an execution job"
//! (`smithy_query(query="shape",shape="Job")`) and a `Step` carries, beside its name,
//! status and span, the URLs a client FETCHES that step's output at: `logUrl` is "the URL
//! to the logs for the execution step", `artifactsUrl` "the URL to the build artifact for
//! the execution step", and `screenshots` "the list of screenshot URLs for the execution
//! step, if relevant" (`smithy_query(query="shape",shape="Step")`). The model fixes no
//! step sequence, so WHICH steps one kind of job runs is the service's: a hosting
//! deployment that succeeded is reported with the whole sequence its execution ran — the
//! deploy of the content, then the post-deploy VERIFICATION of the site it published —
//! and each of those steps reports the output it produced.
//!
//! What those URLs lead to is served BY THIS EMULATOR, from this one plane
//! ([`job_output_path`]), out of what the deployment actually did:
//!
//! * the deploy step's LOG is the record of that deployment's own execution — where its
//!   content came from, which paths it published, and why it ended as it did;
//! * the deploy step's ARTIFACT is the manifest of what it published: every asset path
//!   with the number of bytes serving it;
//! * the verification step's ARTIFACT is what the verification found when it read the
//!   published site back, and its `screenshots` map carries one entry per device the
//!   verification visited the site as, each addressing that device's own capture.
//!
//! A capture is what this emulator can really take of a visit: the response the branch's
//! own hosting edge gives that device for the site's root — its status, the media type and
//! the number of bytes. Nothing here renders an image: a `ThumbnailUrl` leads to the
//! capture the verification took, and to nothing that pretends to be a rendering of it.
//!
//! The ADDRESSES are minted when the deployment is STARTED, because signing one needs the
//! request that started it ([`JobOutputTokens::addressed`]), and what each of them serves
//! is filled in when the clock SETTLES that job ([`capture`]) — the instant its execution
//! finished. So a job that has not settled publishes no step and no URL, and a settled one
//! publishes only what its execution produced: a deployment that FAILED published nothing,
//! so it reports its log and neither an artifact nor a verification.
//!
//! What this retains is BOUNDED by what already bounds a job: one set of outputs per
//! settled deployment, each a few lines of text naming the paths that deployment published
//! — at most [`crate::ops::start_deployment::MAX_DEPLOYMENT_ASSETS`] of them, from an
//! archive no larger than [`crate::ops::start_deployment::MAX_ARCHIVE_BYTES`] — on a
//! branch that holds at most [`crate::state::MAX_JOBS_PER_BRANCH`] jobs. No content is
//! copied here: the files themselves stay where the branch publishes them.

use super::*;

/// The name of the step a settled deployment reports its VERIFICATION on.
///
/// `Step$stepName` is modelled as free text (`smithy_query(query="shape",shape="Step")`)
/// and the phase that follows the deploy of the content is the verification of the site it
/// published, which Amplify names `VERIFY`.
pub const VERIFY_STEP_NAME: &str = "VERIFY";

/// The devices one deployment's verification visits the published site as, each as the
/// `Screenshots` key that names it.
///
/// RECORDED, not modelled: `Screenshots` is a map of `ThumbnailName` to `ThumbnailUrl`
/// (`smithy_query(query="shape",shape="Screenshots")`) with no enumerated keys, so which
/// devices a hosting deployment is verified on is the service's, and Amplify reports its
/// post-deploy verification on this fixed set of phone and tablet models. Each one's
/// capture is addressed on its own, so a client reads one device's result without reading
/// another's.
pub const VERIFICATION_DEVICES: &[&str] = &[
    "googlepixel",
    "ipadair2",
    "iphone7plus",
    "iphone8",
    "samsungs7",
];

/// The path within a published site the verification reads back: the site ROOT, which is
/// the address `Branch$displayName` and `App$defaultDomain` compose
/// ([`crate::state::hosted_branch_host`]) and so the one path every published site answers.
const VERIFIED_PATH: &str = "/";

/// The media type of an output that is a LOG: lines of text.
const LOG_MEDIA_TYPE: &str = "text/plain; charset=utf-8";

/// The media type of an output that is a structured record — a manifest, a verification
/// result, a capture.
const RECORD_MEDIA_TYPE: &str = "application/json";

/// ONE output of a settled job's step: what the plane serves at the address that step
/// published for it.
#[derive(Debug, Clone)]
pub struct StepOutput {
    /// The media type the plane answers it with, so a client reads it as what it is.
    pub media_type: &'static str,
    /// The bytes the plane answers with.
    pub content: Vec<u8>,
}

/// Where ONE output of a job's steps is served: the token that addresses it, and the URL
/// the step reports for it.
#[derive(Debug, Clone)]
pub struct StepOutputAddress {
    /// The token in the output's path — minted per output, so no two outputs of a job, and
    /// no two jobs, share an address.
    pub token: String,
    /// The URL the step reports, presigned against the base this emulator publishes.
    pub url: String,
}

/// The tokens one job's outputs are addressed by, taken out of the store that mints them.
///
/// Minted BEFORE the job's own record is reached, because minting reads the store's own
/// counter while the record is held through it: the addresses are composed from these
/// afterwards ([`Self::addressed`]), which needs only the request.
#[derive(Debug, Clone)]
pub struct JobOutputTokens {
    deploy_log: String,
    deploy_artifacts: String,
    verify_artifacts: String,
    captures: Vec<String>,
}

impl JobOutputTokens {
    /// Mint one token per output a deployment's job publishes: the deploy step's log and
    /// artifact, the verification's artifact, and one capture per
    /// [`VERIFICATION_DEVICES`].
    pub fn minted(store: &mut ComAmazonawsAmplifyAmplifyState) -> Self {
        Self {
            deploy_log: store.mint_plane_token(),
            deploy_artifacts: store.mint_plane_token(),
            verify_artifacts: store.mint_plane_token(),
            captures: VERIFICATION_DEVICES
                .iter()
                .map(|_| store.mint_plane_token())
                .collect(),
        }
    }

    /// The addresses these tokens name, as the steps of job `job_id` of app `app_id` report
    /// them: each output's own path, presigned against the base this emulator publishes
    /// exactly as an upload destination is ([`crate::state::deployment_upload_url`]).
    pub fn addressed(
        self,
        ctx: &::smithy_server_runtime::OperationContext<'_>,
        fallback_host: &str,
        app_id: &str,
        job_id: &str,
    ) -> JobOutputs {
        let address = |token: String| StepOutputAddress {
            url: job_output_url(ctx, fallback_host, app_id, job_id, &token),
            token,
        };
        JobOutputs {
            deploy_log: address(self.deploy_log),
            deploy_artifacts: address(self.deploy_artifacts),
            verify_artifacts: address(self.verify_artifacts),
            captures: VERIFICATION_DEVICES
                .iter()
                .zip(self.captures)
                .map(|(device, token)| ((*device).to_string(), address(token)))
                .collect(),
            served: BTreeMap::new(),
        }
    }
}

/// Every output the steps of ONE deployment's job publish: where each is addressed, and
/// what the plane serves there once the clock has settled that job.
#[derive(Debug, Clone)]
pub struct JobOutputs {
    /// Where the deploy step's log is served.
    pub deploy_log: StepOutputAddress,
    /// Where the deploy step's artifact manifest is served.
    pub deploy_artifacts: StepOutputAddress,
    /// Where the verification step's own artifact is served.
    pub verify_artifacts: StepOutputAddress,
    /// Where each device's capture is served, keyed by the device the verification visited
    /// the site as.
    pub captures: BTreeMap<String, StepOutputAddress>,
    /// What this plane answers at each of those addresses, keyed by the token addressing
    /// it — EMPTY until the clock settles the job, because an execution that has not
    /// finished has produced no output.
    pub served: BTreeMap<String, StepOutput>,
}

impl JobOutputs {
    /// Whether this job's execution has already published its outputs, which is what keeps
    /// [`capture`] from rewriting them on every later read.
    fn published(&self) -> bool {
        !self.served.is_empty()
    }
}

/// What a request to a published job-output URL names: the app whose job published it, that
/// job's own id, and the token addressing the one output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobOutputTarget {
    /// The app the job belongs to.
    pub app_id: String,
    /// The `jobId` of the job that published the output.
    pub job_id: String,
    /// The token addressing that one output.
    pub token: String,
}

/// The path this plane serves ONE output of one job's steps under.
///
/// It sits under the app the job belongs to — the same `/apps` space the modelled routes
/// use — and names no modelled operation: a job is addressed at
/// `/apps/{appId}/branches/{branchName}/jobs/{jobId}`
/// (`smithy_query(query="operation",operation="GetJob")`), so nothing modelled is reached
/// through `/apps/{appId}/jobs/...`. That is what lets the service serve it from its
/// routing-error seam, exactly as it serves an upload destination
/// ([`crate::state::deployment_upload_path`]).
///
/// The BRANCH is not part of the path for the same reason it is not part of an upload's: a
/// branch name is free text (`BranchName` `pattern ^(?s).+$`) that would have to be escaped
/// into a segment, and the job id and the token already locate the one output.
pub fn job_output_path(app_id: &str, job_id: &str, token: &str) -> String {
    format!("/apps/{app_id}/jobs/{job_id}/outputs/{token}")
}

/// The job output a request path names, or `None` for a path this plane does not publish.
pub fn parse_job_output_path(path: &str) -> Option<JobOutputTarget> {
    let rest = path.strip_prefix("/apps/")?;
    let mut segments = rest.split('/');
    let (Some(app_id), Some("jobs"), Some(job_id), Some("outputs"), Some(token), None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return None;
    };
    if app_id.is_empty() || job_id.is_empty() || token.is_empty() {
        return None;
    }
    Some(JobOutputTarget {
        app_id: app_id.to_string(),
        job_id: job_id.to_string(),
        token: token.to_string(),
    })
}

/// The URL one output of a job's steps is read at: that output's own path, SigV4
/// query-presigned against the base this emulator publishes.
///
/// A step's output is FETCHED by whatever a client hands the URL to — a console, a log
/// viewer — with no `Authorization` header of its own, so the address carries its
/// authorisation in the query, exactly as an upload destination does
/// ([`crate::state::deployment_upload_url`]). That is also what makes it an address of THIS
/// emulator: the signer signs for the base this emulator publishes, never for a host only
/// AWS serves.
///
/// A request that could not be signed is served at that same address, so the output is
/// still announced where it is served — on `fallback_host`, the endpoint the caller that
/// started the job reached.
fn job_output_url(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    fallback_host: &str,
    app_id: &str,
    job_id: &str,
    token: &str,
) -> String {
    let path = job_output_path(app_id, job_id, token);
    let request = ::smithy_server_runtime::SignedUrlRequest::new(
        ::smithy_server_runtime::http::Method::GET,
        ctx.identity().signing_name(),
        &path,
    );
    match ctx.url_signer().sign(request, JOB_OUTPUT_URL_EXPIRY) {
        Ok(signed) => signed.as_str().to_string(),
        Err(_) => format!("https://{fallback_host}{path}"),
    }
}

/// How long an address of a step's output stays valid.
///
/// CHOSEN, not documented: AWS publishes no lifetime for the URLs a step reports. It is the
/// window a reserved deployment has to be started in
/// ([`crate::state::DEPLOYMENT_WINDOW_MILLIS`]), so the output of a job is readable for as
/// long as this emulator's deployment flow gives any one deployment.
const JOB_OUTPUT_URL_EXPIRY: ::std::time::Duration =
    ::std::time::Duration::from_millis(DEPLOYMENT_WINDOW_MILLIS.unsigned_abs());

/// The steps a job that has just settled reports: the whole sequence its execution ran.
///
/// `outputs` is the addresses that execution published — `None` for a job that ran nothing
/// that produces one (a build this emulator has no environment for, a reserved deployment
/// whose window closed with nobody starting it), which reports the single step its
/// conclusion names and no URL, because there is no output to address.
///
/// A deployment that SUCCEEDED ran two steps and reports both: the deploy of its content,
/// carrying the log of that execution and the artifact it published, and then the
/// verification of the site, carrying its own artifact and one screenshot per device it
/// visited the site as. One that did not succeed published nothing and verified nothing, so
/// it reports the deploy step alone, with its log and the reason it carries.
///
/// The two steps DIVIDE the execution span the clock owed the job — `due` is when it
/// settled and the span is [`crate::state::JOB_SETTLE_MILLIS`] — so the deploy runs in its
/// first half and the verification in its second, and neither is reported with a span the
/// other covers.
pub fn settled_steps(
    conclusion: &JobConclusion,
    start_time: ::aws_smithy_types::DateTime,
    due: i64,
    outputs: Option<&JobOutputs>,
) -> Vec<Step> {
    let ended = ::aws_smithy_types::DateTime::from_millis(due);
    let Some(outputs) = outputs else {
        return vec![Step {
            step_name: conclusion.step_name.clone(),
            start_time,
            status: conclusion.status.clone(),
            end_time: ended,
            status_reason: conclusion.status_reason.clone(),
            // Every URL below addresses an output an execution produced, and nothing
            // executed; `screenshots` and `context` likewise only exist for a step that
            // ran.
            log_url: None,
            artifacts_url: None,
            test_artifacts_url: None,
            test_config_url: None,
            screenshots: None,
            context: None,
        }];
    };
    let verified_from = ::aws_smithy_types::DateTime::from_millis(
        due.saturating_sub(JOB_SETTLE_MILLIS / 2),
    );
    let succeeded = conclusion.status == JOB_STATUS_SUCCEED;
    let deploy = Step {
        step_name: conclusion.step_name.clone(),
        start_time,
        status: conclusion.status.clone(),
        // A deployment that succeeded hands the rest of the job's span to the
        // verification that follows it; one that did not ran to the end of that span.
        end_time: if succeeded { verified_from } else { ended },
        status_reason: conclusion.status_reason.clone(),
        // The log of THIS execution, which the deployment wrote whatever it ended as.
        log_url: Some(outputs.deploy_log.url.clone()),
        // The artifact is what the deployment PUBLISHED, so a deployment that published
        // nothing reports none.
        artifacts_url: succeeded.then(|| outputs.deploy_artifacts.url.clone()),
        // No test ran: `testArtifactsUrl` and `testConfigUrl` address a test execution's
        // output, and a hosting deployment runs none.
        test_artifacts_url: None,
        test_config_url: None,
        // The captures are the VERIFICATION's, reported on its own step below.
        screenshots: None,
        context: None,
    };
    if !succeeded {
        return vec![deploy];
    }
    let verify = Step {
        step_name: VERIFY_STEP_NAME.to_string(),
        start_time: verified_from,
        status: conclusion.status.clone(),
        end_time: ended,
        // The verification found what the deployment published; there is nothing to
        // explain, and what it read back is in its artifact.
        status_reason: None,
        // A verification reads the published site rather than running a build, so it
        // writes no log of its own: what it found is its artifact.
        log_url: None,
        artifacts_url: Some(outputs.verify_artifacts.url.clone()),
        test_artifacts_url: None,
        test_config_url: None,
        screenshots: Some(
            outputs
                .captures
                .iter()
                .map(|(device, address)| (device.clone(), address.url.clone()))
                .collect(),
        ),
        context: None,
    };
    vec![deploy, verify]
}

/// Publish what the steps of a job that has just settled produced: fill each address this
/// job minted with the output served there.
///
/// Called with the site the branch serves NOW, which for the deployment that has just
/// settled is the site it published: the verification reads that site back, which is the
/// only verification of a published site this emulator can perform — and it performs it
/// ONCE, at the instant the execution finished, so a later deployment of the same branch
/// does not rewrite what an older job's verification found.
pub fn capture(
    job: &Job,
    conclusion: &JobConclusion,
    assets: &BTreeSet<String>,
    site: &BTreeMap<String, Vec<u8>>,
    outputs: &mut JobOutputs,
) {
    if outputs.published() {
        return;
    }
    let published = assets
        .iter()
        .map(|path| (path.clone(), site.get(path).map(Vec::len)))
        .collect::<Vec<_>>();
    outputs.served.insert(
        outputs.deploy_log.token.clone(),
        StepOutput {
            media_type: LOG_MEDIA_TYPE,
            content: deploy_log(job, conclusion, &published),
        },
    );
    if conclusion.status != JOB_STATUS_SUCCEED {
        // Nothing was published and nothing was verified, so there is no artifact and no
        // capture to serve: the steps report none either ([`settled_steps`]).
        return;
    }
    outputs.served.insert(
        outputs.deploy_artifacts.token.clone(),
        StepOutput {
            media_type: RECORD_MEDIA_TYPE,
            content: artifact_manifest(&published),
        },
    );
    let visits = VERIFICATION_DEVICES
        .iter()
        .map(|device| (*device, verified_visit(site)))
        .collect::<Vec<_>>();
    outputs.served.insert(
        outputs.verify_artifacts.token.clone(),
        StepOutput {
            media_type: RECORD_MEDIA_TYPE,
            content: verification_record(&visits),
        },
    );
    for (device, visit) in visits {
        let Some(address) = outputs.captures.get(device) else {
            continue;
        };
        outputs.served.insert(
            address.token.clone(),
            StepOutput {
                media_type: RECORD_MEDIA_TYPE,
                content: capture_record(device, &visit),
            },
        );
    }
}

/// The LOG of one deployment's execution: where its content came from, what it published,
/// and how it ended.
///
/// Every line is read off the job's own record — nothing here is invented and nothing is
/// read from the request that fetches it — so the log a client follows from `logUrl` is the
/// account of what this emulator actually did for that job.
fn deploy_log(
    job: &Job,
    conclusion: &JobConclusion,
    published: &[(String, Option<usize>)],
) -> Vec<u8> {
    let summary = &job.summary;
    let mut lines = vec![format!("{} job {}", conclusion.step_name, summary.job_id)];
    if let Ok(started) = summary
        .start_time
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
    {
        lines.push(format!("started {started}"));
    }
    if let Some(source_url) = &summary.source_url {
        match &summary.source_url_type {
            Some(kind) => lines.push(format!("source {source_url} ({kind})")),
            None => lines.push(format!("source {source_url}")),
        }
    }
    lines.push(format!("publishing {} file(s)", published.len()));
    for (path, bytes) in published {
        match bytes {
            Some(bytes) => lines.push(format!("published {path} ({bytes} bytes)")),
            None => lines.push(format!("not published {path}")),
        }
    }
    lines.push(format!("{} {}", conclusion.step_name, conclusion.status));
    if let Some(reason) = &conclusion.status_reason {
        lines.push(reason.clone());
    }
    lines.join("\n").into_bytes()
}

/// The deploy step's ARTIFACT: the manifest of what the deployment published — every asset
/// path with the number of bytes serving it, and the totals a client checks a download
/// against.
fn artifact_manifest(published: &[(String, Option<usize>)]) -> Vec<u8> {
    let files = published
        .iter()
        .map(|(path, bytes)| {
            ::serde_json::json!({ "path": path, "bytes": bytes.unwrap_or_default() })
        })
        .collect::<Vec<_>>();
    let total: usize = published
        .iter()
        .filter_map(|(_, bytes)| *bytes)
        .fold(0, usize::saturating_add);
    document(&::serde_json::json!({
        "fileCount": files.len(),
        "totalBytes": total,
        "files": files,
    }))
}

/// What ONE visit of the published site found: the answer the branch's own hosting edge
/// gives for the site root.
#[derive(Debug, Clone)]
struct VerifiedVisit {
    /// The HTTP status that visit is answered with.
    status: u16,
    /// The media type of the document served, for a visit that was served one.
    media_type: Option<String>,
    /// How many bytes that document carries.
    bytes: usize,
}

/// Read the published site back the way a visitor of it does: the site ROOT, resolved
/// against the files the deployment published by the same resolution the hosting edge
/// answers a visit with ([`crate::state::resolve_hosted_request`]), so the verification
/// cannot disagree with what that content serves.
///
/// No app rule is applied: what a deployment is verified on is the content IT published,
/// not the rewrite and redirect rules the app is configured with afterwards — those are
/// the app's, and a later change to them does not make an older deployment unverified.
fn verified_visit(site: &BTreeMap<String, Vec<u8>>) -> VerifiedVisit {
    match resolve_hosted_request(&[], site, VERIFIED_PATH) {
        HostedAnswer::File { key, content } => VerifiedVisit {
            status: 200,
            media_type: Some(hosted_file_media_type(&key).to_string()),
            bytes: content.len(),
        },
        HostedAnswer::Redirect { redirect, .. } => VerifiedVisit {
            status: redirect.status(),
            media_type: None,
            bytes: 0,
        },
        HostedAnswer::NotFound | HostedAnswer::Unauthorized => VerifiedVisit {
            status: 404,
            media_type: None,
            bytes: 0,
        },
    }
}

/// The verification step's ARTIFACT: the path it read back and what every device's visit
/// found there.
fn verification_record(visits: &[(&str, VerifiedVisit)]) -> Vec<u8> {
    document(&::serde_json::json!({
        "verifiedPath": VERIFIED_PATH,
        "devices": visits
            .iter()
            .map(|(device, visit)| capture_document(device, visit))
            .collect::<Vec<_>>(),
    }))
}

/// ONE device's CAPTURE: what the site answered that device's visit with.
fn capture_record(device: &str, visit: &VerifiedVisit) -> Vec<u8> {
    document(&capture_document(device, visit))
}

/// The capture of one device's visit as a document, shared by the capture served on its own
/// and the verification record listing every device's.
fn capture_document(device: &str, visit: &VerifiedVisit) -> ::serde_json::Value {
    ::serde_json::json!({
        "device": device,
        "path": VERIFIED_PATH,
        "status": visit.status,
        "mediaType": visit.media_type,
        "bytes": visit.bytes,
    })
}

/// One output rendered as the bytes the plane serves.
fn document(value: &::serde_json::Value) -> Vec<u8> {
    ::serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec())
}
