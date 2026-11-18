// SPDX-License-Identifier: CC0-1.0

//! The WORK an automated evaluation job performs: the prompts it reads out of its
//! configured datasets, the inference it performs over each one on the model its
//! `inferenceConfig` names, the metrics it scores that inference with, and the result
//! records it writes under its output prefix.
//!
//! An evaluation job is not a status record. The result of an evaluation IS its S3 output:
//! `outputDataConfig` is "the Amazon S3 location where the results of your evaluation job
//! are saved" and its `s3Uri` is "the Amazon S3 URI where the results of the evaluation job
//! are saved" (`smithy_query(query="shape",shape="EvaluationOutputDataConfig")`), and the
//! `automated` configuration carries the `datasetMetricConfigs` — a task type, a dataset and
//! the metric names — that say WHAT is to be run and scored
//! (`smithy_query(query="shape",shape="AutomatedEvaluationConfig,EvaluationDatasetMetricConfig")`).
//! So a job this emulator reports as having run has read the records of every dataset its
//! configuration names, has invoked each model its `inferenceConfig` names on each record,
//! has scored the configured metrics over what that inference answered, and has written
//! those records under the prefix its `outputDataConfig` names. A job whose dataset or
//! output location cannot be used takes the modelled `Failed` path with `failureMessages`
//! instead, and reports no results it does not have.
//!
//! WHERE the records come from and go: this emulator's OWN Amazon S3, over the in-process
//! callback every cross-service call of an emulated service uses
//! ([`super::model_invocation_run::s3_client`] — no socket, no outbound request, no host
//! resolution). The caller's `s3Uri` reaches it only as the bucket and key of a `GetObject`
//! and a `PutObject`, never as a host or a URL.
//!
//! WHAT performs the inference: the Amazon Bedrock Runtime emulator's own generation stage,
//! read through that service's state module — the very function its `InvokeModel` and
//! `Converse` handlers answer from ([`runtime::emulated_completion`]) — so an evaluated
//! record and a synchronous call of the same model are served by one inference rather than
//! by two that could drift. Nothing is echoed back as if generated: the response is that
//! service's own, which says in so many words that no language model runs here.
//!
//! WHAT THE GUARDRAIL DOES TO A RECORD: the inference an evaluation performs is inference of
//! this account, so the guardrail the account ENFORCES on the evaluated model is applied to
//! it, through the one reading of that enforcement the inference plane applies
//! ([`runtime_guardrail::for_model`]). The guardrail REPLACES the model's output for a
//! record it intervened on ("BLOCK – Block the content and replace it with blocked
//! messaging"), and the model's output is what an evaluation record carries, so such a
//! record carries the guardrail's configured `blockedOutputsMessaging` verbatim as the
//! response that was scored — which is what the recorded output of such a job holds.
//!
//! WHAT a verdict rests on: a job is failed over its dataset or its output location only
//! when the object store ITSELF said something about it
//! ([`super::model_invocation_run::names_nothing`]) — an emulator assembled with no object
//! store, or no inference plane, to ask establishes nothing, so such a job keeps the
//! lifecycle it has always had rather than being failed on an unanswered question.

use std::collections::HashMap;

use serde_json::{json, Value};

use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state as runtime;
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state::enforced_guardrail as runtime_guardrail;

use super::evaluation_datasets::{dataset_records, DatasetRecord, DatasetVerdict};
use super::model_invocation_run::{names_nothing, prefix_address, s3_client};
use crate::ops::types::{
    EvaluationConfig, EvaluationDatasetLocation, EvaluationInferenceConfig, EvaluationModelConfig,
    EvaluationOutputDataConfig,
};

/// The most runs one request performs, however many jobs are owed one.
///
/// A listing settles every job of the Region, so without this the work one call does would
/// be the number of jobs times the records of each; with it a call performs at most this
/// many runs and the rest stay owed, so a later call takes them. Beside
/// [`super::model_invocation_run::MAX_RUNS_PER_REQUEST`], which bounds the same thing for
/// the batch inference family, and the same size for the same reason.
pub const MAX_RUNS_PER_REQUEST: usize = 4;

/// The content type the result objects are stored with: their records are JSON objects, one
/// per line.
const OUTPUT_CONTENT_TYPE: &str = "application/json";

/// The name of the result file of one model, task type and dataset of a job.
const OUTPUT_FILE: &str = "output.jsonl";

/// `Builtin.Toxicity` — the built-in metric whose value is a property of the response the
/// evaluated model answered with: how toxic that text is.
///
/// "For automated model evaluation jobs, valid values are `Builtin.Accuracy`,
/// `Builtin.Robustness`, and `Builtin.Toxicity`"
/// (`smithy_query(query="shape",shape="EvaluationDatasetMetricConfig",member="metricNames")`)
/// — the three built-in metrics a `datasetMetricConfigs` entry of an automated job can name.
/// This one is computed by the very content recognizers this emulator's inference plane
/// evaluates a guardrail with ([`response_toxicity`]).
pub const METRIC_TOXICITY: &str = "Builtin.Toxicity";

/// `Builtin.Accuracy` — the built-in metric whose value is how well the response ANSWERS the
/// record, so it is computed from that response and the record's own `referenceResponse`
/// ([`accuracy_score`]).
pub const METRIC_ACCURACY: &str = "Builtin.Accuracy";

/// `QuestionAndAnswer` — the task type whose accuracy is the F1 overlap of the response with
/// the record's reference response.
///
/// A value of the modelled `EvaluationTaskType`
/// (`smithy_query(query="shape",shape="EvaluationTaskType")`: `Summarization`,
/// `Classification`, `QuestionAndAnswer`, `Generation`, `Custom`). "Use automatic model
/// evaluation" in the Amazon Bedrock User Guide publishes what accuracy is computed as per
/// task type: the F1 score for question answering, and whether the predicted class is the
/// real one for text classification ([`TASK_CLASSIFICATION`]) — both a comparison of the
/// response with the reference response the record carries, which is what this emulator
/// holds for a record. The accuracy of the other task types is published as a score of a
/// model this emulator runs none of (a semantic similarity for summarization, a real-world
/// knowledge score for generation), so those are reported as metrics no value was computed
/// for rather than carrying a number computed another way under the same name.
const TASK_QUESTION_AND_ANSWER: &str = "QuestionAndAnswer";

/// `Classification` — the task type whose accuracy is whether the response carries the class
/// the record names, scored one or zero. See [`TASK_QUESTION_AND_ANSWER`] for the source.
const TASK_CLASSIFICATION: &str = "Classification";

/// What a result record reports for a configured metric this emulator computes no value for.
///
/// It names the metric and says what was not computed, so a reader of the results can tell
/// a metric that scored zero from one that was never scored.
#[must_use]
pub fn uncomputed_metric_message(metric: &str) -> String {
    format!("This emulator computes no {metric} score for an evaluated response.")
}

/// The same for a metric whose value is computed for this task type but not for THIS record:
/// there is no reference response to compare the response with.
#[must_use]
pub fn unreferenced_metric_message(metric: &str) -> String {
    format!(
        "This emulator computes no {metric} score for a record that carries no \
         referenceResponse to compare the response with."
    )
}

/// The same for a metric whose published computation for THIS task type needs a score of a
/// model this emulator runs none of.
#[must_use]
pub fn untasked_metric_message(metric: &str, task_type: &str) -> String {
    format!("This emulator computes no {metric} score for a {task_type} task.")
}

/// One dataset a job is configured to evaluate over, with the task type and the metrics
/// that `datasetMetricConfigs` entry named.
#[derive(Debug, Clone)]
pub struct DueDataset {
    /// `taskType` — a value of the modelled `EvaluationTaskType` enum.
    pub task_type: String,
    /// `dataset.name` — a built-in dataset's published name, or the caller's own name for
    /// its own file.
    pub name: String,
    /// `dataset.datasetLocation.s3Uri`, where the entry carried one.
    pub location: Option<String>,
    /// `metricNames` — the metrics the records of this dataset are scored with.
    pub metric_names: Vec<String>,
}

/// The run a job still owes: everything its configuration says about the work, read off the
/// stored record so the run needs no second look at the store.
#[derive(Debug, Clone)]
pub struct DueRun {
    /// The `EvaluationJobArn` of the job the run belongs to.
    pub arn: String,
    /// `jobName`, which is part of where the results are written.
    pub job_name: String,
    /// The Region the job was created in, which is where its records are read and written.
    pub region: String,
    /// The `EvaluationBedrockModel$modelIdentifier` of each model the job evaluates, in the
    /// order its `inferenceConfig` named them.
    pub model_ids: Vec<String>,
    /// The datasets the job's `datasetMetricConfigs` named.
    pub datasets: Vec<DueDataset>,
    /// `outputDataConfig.s3Uri`.
    pub output_uri: String,
}

impl DueRun {
    /// The minted job id out of [`Self::arn`]: the trailing segment of an
    /// `EvaluationJobArn`, which is part of where a job's results live.
    #[must_use]
    pub fn job_id(&self) -> &str {
        crate::state::evaluation_job_id_of(&self.arn)
    }
}

/// What a job's run established.
#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// The run read its datasets, invoked the models over every record and saved the result
    /// records: `records` of them were evaluated, `errors` of them could not be, and the
    /// enforced guardrail intervened on `interventions` of the evaluated ones.
    Ran {
        records: i64,
        errors: i64,
        interventions: i64,
    },
    /// The configuration cannot be run as it stands, so the job fails with these sentences
    /// as its `failureMessages`.
    Failed(Vec<String>),
    /// Nothing could be established or stored, so the run is still owed and a later call
    /// takes it again.
    Deferred,
}

/// One model of a job's `inferenceConfig`, as the inference plane answered about it.
#[derive(Debug, Clone)]
enum EvaluatedModel {
    /// A model that plane serves inference for: the records are invoked on it.
    Served(runtime::ModelEntry),
    /// An identifier it serves no inference for, with the sentence it answers about it. The
    /// records of such a model are reported as records that could not be evaluated, exactly
    /// as a batch inference job reports a record its model could not be invoked for
    /// ([`super::model_invocation_run`]) — so what happened to them is IN the results rather
    /// than missing from them.
    Unservable { identifier: String, message: String },
}

impl EvaluatedModel {
    /// The name this model's results are reported and saved under: the id the identifier
    /// RESOLVED to, so one model's results live in one place whichever spelling named it,
    /// and the identifier as the job spelled it where it resolved to no model at all.
    fn name(&self) -> &str {
        match self {
            Self::Served(entry) => entry.model_id.as_str(),
            Self::Unservable { identifier, .. } => identifier.as_str(),
        }
    }
}

/// The `s3Uri` a job writes its results under.
#[must_use]
pub fn output_s3_uri(config: &EvaluationOutputDataConfig) -> &str {
    config.s3_uri.as_str()
}

/// The datasets an evaluation configuration says are to be run, or an empty list for a
/// configuration whose records are not evaluated by this service at all.
///
/// Only the `automated` configuration is run here: it is the one that "computes metrics"
/// itself (`smithy_query(query="shape",shape="EvaluationConfig")`), while a `human`
/// configuration's records are rated by the workers of its own workflow rather than by
/// anything this service computes.
#[must_use]
pub fn due_datasets(config: &EvaluationConfig) -> Vec<DueDataset> {
    let EvaluationConfig::Automated(automated) = config else {
        return Vec::new();
    };
    automated
        .dataset_metric_configs
        .iter()
        .map(|entry| DueDataset {
            task_type: entry.task_type.clone(),
            name: entry.dataset.name.clone(),
            location: entry.dataset.dataset_location.as_ref().map(|location| {
                let EvaluationDatasetLocation::s3Uri(uri) = location;
                uri.clone()
            }),
            metric_names: entry.metric_names.clone(),
        })
        .collect()
}

/// The models an inference configuration says are to be invoked, or an empty list for one
/// that names none to invoke.
///
/// A `bedrockModel` names the model whose responses are scored, so it is the one an
/// evaluation invokes. A `precomputedInferenceSource` brings responses the caller computed
/// ELSEWHERE — there is no inference for this service to perform for it — and `ragConfigs`
/// names knowledge bases rather than models; neither is invoked here.
#[must_use]
pub fn due_model_ids(config: &EvaluationInferenceConfig) -> Vec<String> {
    let EvaluationInferenceConfig::Models(models) = config else {
        return Vec::new();
    };
    models
        .iter()
        .filter_map(|model| match model {
            EvaluationModelConfig::bedrockModel(model) => Some(model.model_identifier.clone()),
            EvaluationModelConfig::precomputedInferenceSource(_) => None,
        })
        .collect()
}

/// Run one evaluation job: read the records of every dataset it names, invoke each model it
/// evaluates over each record under the account's enforced guardrail, score the configured
/// metrics, and store the result records under its output prefix.
///
/// It is the work the job's own clock made due with the status that says the job is running,
/// so it is performed once per job ([`crate::state::EvaluationJobRecord::due_run`]) and no
/// later call performs it again: what a client fetched out of the output location once stays
/// what it fetched.
///
/// HOW MUCH work one call does is bounded by the job's own configuration — at most five
/// `datasetMetricConfigs` (the member's modelled `@length(1..5)`) of at most
/// [`super::evaluation_datasets::MAX_DATASET_RECORDS`] records each, over at most two models
/// (`EvaluationModelConfigs`' modelled `@length(1..2)`) — and by nothing a later request
/// sends.
pub async fn run_evaluation<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    due: &DueRun,
) -> RunOutcome {
    // The models are resolved through the very index the Amazon Bedrock Runtime emulator's
    // own inference resolves one through, so an evaluation and a synchronous call agree
    // about which identifiers name an invocable model. A store this engine does not carry at
    // all is nothing established, so the run stays owed.
    let Some(runtime_state) = runtime::service_state(engine) else {
        return RunOutcome::Deferred;
    };
    let models = {
        let store = runtime_state.lock();
        due.model_ids
            .iter()
            .map(|model_id| match store.resolve_model_for_inference(model_id) {
                Ok(entry) => EvaluatedModel::Served(entry.clone()),
                Err(
                    runtime::ModelRejection::Invalid(message)
                    | runtime::ModelRejection::NotFound(message),
                ) => EvaluatedModel::Unservable {
                    identifier: model_id.clone(),
                    message,
                },
            })
            .collect::<Vec<EvaluatedModel>>()
    };

    let Some((bucket, prefix)) = prefix_address(&due.output_uri) else {
        return RunOutcome::Failed(vec![format!(
            "The output location {} does not name a location in Amazon S3. Point \
             outputDataConfig.s3Uri at the prefix the results are to be written under and \
             submit the job again.",
            due.output_uri
        )]);
    };

    let client = s3_client(ctx, &due.region);
    let mut evaluated = 0i64;
    let mut errors = 0i64;
    let mut interventions = 0i64;
    let mut first_error: Option<String> = None;
    for model in &models {
        // The guardrail this account enforces on invocations of THIS model, read once per
        // model rather than per record: the enforcement is account-level state, so it is the
        // same for every record of the run.
        let enforced = match model {
            EvaluatedModel::Served(entry) => runtime_guardrail::for_model(engine, &entry.model_id),
            EvaluatedModel::Unservable { .. } => None,
        };
        for dataset in &due.datasets {
            let prompts =
                match dataset_records(ctx, &due.region, &dataset.name, dataset.location.as_deref())
                    .await
                {
                    DatasetVerdict::Records(records) => records,
                    DatasetVerdict::Rejected(message) => return RunOutcome::Failed(vec![message]),
                    DatasetVerdict::Unknown => return RunOutcome::Deferred,
                };
            let mut lines = String::new();
            for record in &prompts {
                let line = match model {
                    EvaluatedModel::Served(entry) => {
                        let (line, intervened) =
                            evaluated_record(entry, enforced.as_ref(), dataset, record);
                        evaluated += 1;
                        interventions += i64::from(intervened);
                        line
                    }
                    EvaluatedModel::Unservable { message, .. } => {
                        errors += 1;
                        if first_error.is_none() {
                            first_error = Some(message.clone());
                        }
                        unevaluated_record(model.name(), record, message)
                    }
                };
                lines.push_str(&line.to_string());
                lines.push('\n');
            }
            let key = output_key(&prefix, due, model.name(), dataset);
            if let Err(error) = client
                .put_object()
                .bucket(&bucket)
                .key(&key)
                .body(::aws_sdk_s3::primitives::ByteStream::from(
                    lines.into_bytes(),
                ))
                .content_type(OUTPUT_CONTENT_TYPE)
                .send()
                .await
            {
                if names_nothing(&error) {
                    // S3 says there is no such bucket to write into: the job cannot save its
                    // results where it was told to, which is a failure of the job.
                    return RunOutcome::Failed(vec![format!(
                        "The results could not be saved to {}. Make sure that the location \
                         exists and that the service role has access to it, then submit the \
                         job again.",
                        due.output_uri
                    )]);
                }
                // Anything else leaves the results unsaved without saying the location is
                // wrong — an emulator with no object store mounted, say — so the run stays
                // owed and a later call takes it again.
                ::tracing::warn!(
                    bucket = ?bucket,
                    key = ?key,
                    detail = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the evaluation results were not saved in this emulator's Amazon S3"
                );
                return RunOutcome::Deferred;
            }
            ::tracing::info!(
                bucket = ?bucket,
                key = ?key,
                records = prompts.len(),
                "the evaluation results were saved"
            );
        }
    }
    // What the run established, once every result file is saved. A run NO record of which
    // could be evaluated did none of the work the job was configured for, so it fails on what
    // the inference plane answered about the first of them — the rule a batch inference job's
    // run follows for the same case ([`super::model_invocation_run`]) — while a run some of
    // whose records were evaluated reports the results it saved.
    match (evaluated, first_error) {
        (0, Some(message)) => RunOutcome::Failed(vec![message]),
        _ => RunOutcome::Ran {
            records: evaluated,
            errors,
            interventions,
        },
    }
}

/// The key the result file of one model, task type and dataset of a job is stored under.
///
/// The results of a job are saved UNDER the location the submission named, in the layout
/// the real service's output carries: the job's own name and id, then the model whose
/// responses these are, then the task type and the dataset they are for — so one job's
/// results never overwrite another's and a reader can tell which model, task and dataset a
/// file holds.
fn output_key(prefix: &str, due: &DueRun, model_id: &str, dataset: &DueDataset) -> String {
    let path = format!(
        "{}/{}/models/{model_id}/taskTypes/{}/datasets/{}/{OUTPUT_FILE}",
        due.job_name,
        due.job_id(),
        dataset.task_type,
        dataset.name
    );
    if prefix.is_empty() {
        path
    } else {
        format!("{prefix}/{path}")
    }
}

/// One result record of a model the inference plane serves no inference for: the input
/// record, and what that plane answered about the identifier the job named.
///
/// A record that could not be evaluated says so IN the results, beside the records that
/// were, which is how a batch inference job reports a record its model could not be invoked
/// for ([`super::model_invocation_run`]). It carries no response and no score, because
/// nothing was inferred and nothing was scored.
fn unevaluated_record(model_identifier: &str, record: &DatasetRecord, message: &str) -> Value {
    json!({
        "inputRecord": input_record(record),
        "modelResponses": [{"modelIdentifier": model_identifier}],
        "error": {"errorMessage": message},
    })
}

/// The `inputRecord` of a result record: the prompt the dataset carried, with the reference
/// response and the category it carried beside it.
fn input_record(record: &DatasetRecord) -> Value {
    let mut input = json!({"prompt": record.prompt});
    for (member, value) in [
        ("referenceResponse", record.reference_response.as_ref()),
        ("category", record.category.as_ref()),
    ] {
        if let (Some(object), Some(value)) = (input.as_object_mut(), value) {
            object.insert(member.to_string(), Value::String(value.clone()));
        }
    }
    input
}

/// One result record: the input record, the response the evaluated model answered with, and
/// the score of every metric the dataset's entry configured — with whether the account's
/// enforced guardrail intervened on this record's inference.
///
/// The response is the inference stage's own ([`runtime::emulated_completion`], the one
/// every surface of that service answers from), composed to the model's own output ceiling.
/// Where the enforced guardrail intervened, the response is that guardrail's configured
/// `blockedOutputsMessaging`, because the guardrail replaced the model's output and the
/// model's output is what this record carries.
fn evaluated_record(
    model: &runtime::ModelEntry,
    enforced: Option<&runtime_guardrail::EnforcedGuardrail>,
    dataset: &DueDataset,
    record: &DatasetRecord,
) -> (Value, bool) {
    let input_tokens = runtime::count_text_tokens(&record.prompt);
    // One prompt is one message of input, and the ceiling is the model's own: an evaluation
    // names no generation budget of its own.
    let completion = runtime::emulated_completion(
        &model.model_id,
        1,
        input_tokens,
        model.max_output_tokens,
        &[],
    );
    let mut response = completion.text;
    let mut intervened = false;
    if let Some(enforced) = enforced {
        let prompt = vec![record.prompt.clone()];
        if enforced.input_intervention(&prompt).is_some()
            || enforced.output_intervention(&response).is_some()
        {
            response = enforced.blocked_outputs_messaging.clone();
            intervened = true;
        }
    }

    let mut scores: Vec<Value> = Vec::new();
    let mut unscored: Vec<Value> = Vec::new();
    for metric in &dataset.metric_names {
        match metric_score(metric, &dataset.task_type, record, &response) {
            Some(result) => scores.push(json!({"metricName": metric, "result": result})),
            None => unscored.push(json!({
                "metricName": metric,
                "message": unscored_metric_message(metric, &dataset.task_type, record),
            })),
        }
    }

    let mut line = json!({
        "inputRecord": input_record(record),
        "modelResponses": [{
            "response": response,
            "modelIdentifier": model.model_id,
        }],
        "automatedEvaluationResult": {"scores": scores},
    });
    if let (Some(object), false) = (line.as_object_mut(), unscored.is_empty()) {
        // The metrics this emulator computed no value for, named in the record rather than
        // left out of it: a reader can then tell a metric that scored zero from one that was
        // never scored.
        object.insert("unscoredMetrics".to_string(), Value::Array(unscored));
    }
    (line, intervened)
}

/// The score of one configured metric over the record and the response the evaluated model
/// answered it with, or `None` for a metric no value was computed for
/// ([`unscored_metric_message`] says which of the reasons it was).
///
/// The built-in metrics of an automated job are `Builtin.Accuracy`, `Builtin.Robustness` and
/// `Builtin.Toxicity` (the `metricNames` member's own documentation). Toxicity is a property
/// of the response alone and accuracy a comparison of it with the record's reference
/// response, so both are computed here from what the run actually has. Robustness is
/// published as a score over the responses to PERTURBED inputs, and the response this
/// emulator's inference plane answers does not vary with the wording of the prompt, so
/// nothing it holds computes a robustness value — it is reported as a metric no value was
/// computed for, as is any metric name outside the built-in set (a custom metric is scored by
/// the judge model its own configuration names).
fn metric_score(
    metric: &str,
    task_type: &str,
    record: &DatasetRecord,
    response: &str,
) -> Option<f64> {
    match metric {
        METRIC_TOXICITY => Some(response_toxicity(response)),
        METRIC_ACCURACY => accuracy_score(task_type, reference_response(record)?, response),
        _ => None,
    }
}

/// The reference response a record carries for its answer to be compared with, or `None` for
/// a record that carries none to compare against.
fn reference_response(record: &DatasetRecord) -> Option<&str> {
    record
        .reference_response
        .as_deref()
        .map(str::trim)
        .filter(|reference| !reference.is_empty())
}

/// Which of the reasons a configured metric carries no score in a record, stated so a reader
/// of the results can tell a metric that scored zero from one that was never scored, and a
/// metric this emulator never scores from one this RECORD gave it nothing to score.
fn unscored_metric_message(metric: &str, task_type: &str, record: &DatasetRecord) -> String {
    match (metric, accuracy_is_computed_for(task_type)) {
        (METRIC_ACCURACY, false) => untasked_metric_message(metric, task_type),
        (METRIC_ACCURACY, true) if reference_response(record).is_none() => {
            unreferenced_metric_message(metric)
        }
        _ => uncomputed_metric_message(metric),
    }
}

/// Whether the accuracy of a task type is one this emulator computes: the task types whose
/// published accuracy is a comparison of the response with the record's reference response
/// ([`TASK_QUESTION_AND_ANSWER`], [`TASK_CLASSIFICATION`]).
fn accuracy_is_computed_for(task_type: &str) -> bool {
    matches!(task_type, TASK_QUESTION_AND_ANSWER | TASK_CLASSIFICATION)
}

/// How accurately the response answers the record, in the 0..1 scale a metric score is
/// reported on — or `None` for a task type whose accuracy is published as the score of a
/// model this emulator runs none of.
///
/// Question answering is scored with the F1 overlap of the response with the reference
/// response ([`token_f1`]) and classification with whether the response carries the class the
/// record names ([`carries_class`]), which is what each is published as
/// ([`TASK_QUESTION_AND_ANSWER`]).
fn accuracy_score(task_type: &str, reference: &str, response: &str) -> Option<f64> {
    match task_type {
        TASK_QUESTION_AND_ANSWER => Some(token_f1(reference, response)),
        TASK_CLASSIFICATION => Some(if carries_class(reference, response) {
            1.0
        } else {
            0.0
        }),
        _ => None,
    }
}

/// The words of a text, as both halves of a comparison are read: lower-cased runs of
/// alphanumeric characters, so punctuation and spacing cannot make two spellings of one word
/// differ.
fn words(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The F1 overlap of the response with the reference response: the harmonic mean of how much
/// of the response is in the reference (precision) and how much of the reference is in the
/// response (recall), over the words of each. A response sharing no word with the reference
/// scores zero, and one that is the reference scores one.
fn token_f1(reference: &str, response: &str) -> f64 {
    let expected = words(reference);
    let answered = words(response);
    if expected.is_empty() || answered.is_empty() {
        return 0.0;
    }
    // The words of the reference still to be matched, counted: a word the reference carries
    // twice is matched twice and no more, which is what makes this an overlap of the two
    // texts rather than a test of membership.
    let mut outstanding: HashMap<&str, usize> = HashMap::new();
    for word in &expected {
        *outstanding.entry(word.as_str()).or_default() += 1;
    }
    let mut shared = 0usize;
    for word in &answered {
        if let Some(left) = outstanding.get_mut(word.as_str()).filter(|left| **left > 0) {
            *left -= 1;
            shared += 1;
        }
    }
    if shared == 0 {
        return 0.0;
    }
    let precision = shared as f64 / answered.len() as f64;
    let recall = shared as f64 / expected.len() as f64;
    2.0 * precision * recall / (precision + recall)
}

/// Whether the response carries the class the record names: its words, in order, somewhere in
/// the words of the response.
fn carries_class(reference: &str, response: &str) -> bool {
    let label = words(reference);
    let answered = words(response);
    !label.is_empty()
        && answered.len() >= label.len()
        && answered
            .windows(label.len())
            .any(|window| window == label.as_slice())
}

/// How toxic the response is, in the 0..1 scale a metric score is reported on.
///
/// Computed in process by the very content recognizers the inference plane's own guardrail
/// evaluation uses ([`runtime::guardrail_checks::evaluate`]): the score is the strongest
/// severity any modelled content-filter category was placed in the response at, so a
/// response nothing was placed in scores zero and the two surfaces cannot disagree about
/// what a text carries.
fn response_toxicity(response: &str) -> f64 {
    use runtime::guardrail_checks::{
        evaluate, CheckedText, RequestedChecks, CONTENT_FILTER_RESULT_ORDER,
    };

    let requested = RequestedChecks {
        content_filter: Some(
            CONTENT_FILTER_RESULT_ORDER
                .into_iter()
                .map(str::to_string)
                .collect(),
        ),
        prompt_attack: None,
        sensitive_information: None,
    };
    let texts = vec![CheckedText {
        message_index: 0,
        content_index: 0,
        text: response.to_string(),
    }];
    evaluate(&requested, &texts)
        .content_filter
        .unwrap_or_default()
        .into_iter()
        .map(|finding| finding.severity_score)
        .max_by(f64::total_cmp)
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    //! The parts of the work step that need no object store and no inference plane: what a
    //! configuration says is to be run, where a result file goes, and what a metric scores.

    use super::*;
    use crate::ops::types::{
        AutomatedEvaluationConfig, EvaluationBedrockModel, EvaluationDataset,
        EvaluationDatasetMetricConfig, EvaluationPrecomputedInferenceSource, HumanEvaluationConfig,
    };

    fn dataset_entry(task_type: &str, name: &str, location: Option<&str>) -> EvaluationDatasetMetricConfig {
        EvaluationDatasetMetricConfig {
            task_type: task_type.to_string(),
            dataset: EvaluationDataset {
                name: name.to_string(),
                dataset_location: location
                    .map(|uri| EvaluationDatasetLocation::s3Uri(uri.to_string())),
            },
            metric_names: vec![METRIC_TOXICITY.to_string()],
        }
    }

    /// RULE: the datasets to run are the `datasetMetricConfigs` of an `automated`
    /// configuration, each with the task type, dataset and metrics it named; a `human`
    /// configuration names no run this service performs.
    #[test]
    fn the_datasets_to_run_come_off_the_automated_configuration() {
        let automated = EvaluationConfig::Automated(AutomatedEvaluationConfig {
            dataset_metric_configs: vec![
                dataset_entry("Generation", "Builtin.RealToxicityPrompts", None),
                dataset_entry("Summarization", "own-prompts", Some("s3://bucket/prompts.jsonl")),
            ],
            ..AutomatedEvaluationConfig::default()
        });
        let due = due_datasets(&automated);
        let read: Vec<(&str, &str, Option<&str>)> = due
            .iter()
            .map(|dataset| {
                (
                    dataset.task_type.as_str(),
                    dataset.name.as_str(),
                    dataset.location.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            read,
            vec![
                ("Generation", "Builtin.RealToxicityPrompts", None),
                (
                    "Summarization",
                    "own-prompts",
                    Some("s3://bucket/prompts.jsonl")
                ),
            ],
            "case \"two automated dataset entries\": expected both, got {read:?}"
        );
        assert!(
            due.iter()
                .all(|dataset| dataset.metric_names == vec![METRIC_TOXICITY.to_string()]),
            "case \"two automated dataset entries\": each dataset keeps its own metrics, got \
             {due:?}"
        );

        let human = EvaluationConfig::Human(HumanEvaluationConfig {
            dataset_metric_configs: vec![dataset_entry("Generation", "own-prompts", None)],
            ..HumanEvaluationConfig::default()
        });
        assert!(
            due_datasets(&human).is_empty(),
            "case \"a human configuration\": its records are rated by workers, so this service \
             runs none of them, got {:?}",
            due_datasets(&human)
        );
    }

    /// RULE: the models to invoke are the `bedrockModel` entries of an `inferenceConfig`,
    /// and a configuration naming responses computed elsewhere names none to invoke.
    #[test]
    fn the_models_to_invoke_are_the_bedrock_models_the_configuration_names() {
        let bedrock = |model_id: &str| {
            EvaluationModelConfig::bedrockModel(EvaluationBedrockModel {
                model_identifier: model_id.to_string(),
                ..EvaluationBedrockModel::default()
            })
        };
        // (case, the configuration, the identifiers it names)
        let rows: Vec<(&str, EvaluationInferenceConfig, Vec<&str>)> = vec![
            (
                "one model",
                EvaluationInferenceConfig::Models(vec![bedrock("amazon.nova-lite-v1:0")]),
                vec!["amazon.nova-lite-v1:0"],
            ),
            (
                "two models, the modelled maximum",
                EvaluationInferenceConfig::Models(vec![
                    bedrock("amazon.nova-lite-v1:0"),
                    bedrock("arn:aws:bedrock:us-east-1::foundation-model/anthropic.claude-v2"),
                ]),
                vec![
                    "amazon.nova-lite-v1:0",
                    "arn:aws:bedrock:us-east-1::foundation-model/anthropic.claude-v2",
                ],
            ),
            (
                "responses computed elsewhere",
                EvaluationInferenceConfig::Models(vec![
                    EvaluationModelConfig::precomputedInferenceSource(
                        EvaluationPrecomputedInferenceSource {
                            inference_source_identifier: "recorded.v3".to_string(),
                        },
                    ),
                ]),
                vec![],
            ),
        ];
        for (case, config, expected) in rows {
            let read = due_model_ids(&config);
            assert_eq!(
                read, expected,
                "case {case:?}: expected the identifiers {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: a job's results are saved UNDER the location the submission named, one file per
    /// model, task type and dataset, under the job's own name and id.
    #[test]
    fn a_result_file_is_named_for_the_job_model_task_and_dataset() {
        let due = DueRun {
            arn: "arn:aws:bedrock:us-east-1:000000000000:evaluation-job/abcdef123456".to_string(),
            job_name: "toxicity-review".to_string(),
            region: "us-east-1".to_string(),
            model_ids: vec!["amazon.nova-lite-v1:0".to_string()],
            datasets: Vec::new(),
            output_uri: String::new(),
        };
        let dataset = DueDataset {
            task_type: "Generation".to_string(),
            name: "Builtin.RealToxicityPrompts".to_string(),
            location: None,
            metric_names: vec![METRIC_TOXICITY.to_string()],
        };
        // (case, the prefix the submission named, the key the file goes to)
        let rows = [
            (
                "a prefix",
                "eval-output",
                "eval-output/toxicity-review/abcdef123456/models/amazon.nova-lite-v1:0/\
                 taskTypes/Generation/datasets/Builtin.RealToxicityPrompts/output.jsonl",
            ),
            (
                "a nested prefix",
                "reports/2026/q1",
                "reports/2026/q1/toxicity-review/abcdef123456/models/amazon.nova-lite-v1:0/\
                 taskTypes/Generation/datasets/Builtin.RealToxicityPrompts/output.jsonl",
            ),
            (
                "the bucket itself",
                "",
                "toxicity-review/abcdef123456/models/amazon.nova-lite-v1:0/taskTypes/\
                 Generation/datasets/Builtin.RealToxicityPrompts/output.jsonl",
            ),
        ];
        for (case, prefix, expected) in rows {
            let key = output_key(prefix, &due, "amazon.nova-lite-v1:0", &dataset);
            assert_eq!(
                key, expected,
                "case {case:?}: expected the key {expected:?}, got {key:?}"
            );
        }
    }

    /// One row of the accuracy table: what the record and the response carry, and what the
    /// metric must come to over them (`None` for a case no value is computed for).
    struct AccuracyCase {
        case: &'static str,
        /// `taskType`, which decides HOW accuracy is computed.
        task_type: &'static str,
        /// `referenceResponse`, where the record carries one.
        reference: Option<&'static str>,
        /// The response the evaluated model answered with.
        response: &'static str,
        /// The score, or `None` where the metric is reported as one no value was computed for.
        score: Option<f64>,
    }

    /// One dataset record, with the reference response a metric is computed against where it
    /// carries one.
    fn record(prompt: &str, reference: Option<&str>) -> DatasetRecord {
        DatasetRecord {
            prompt: prompt.to_string(),
            reference_response: reference.map(str::to_string),
            category: None,
        }
    }

    /// RULE: a toxicity score is computed from the response, so a response the recognizers
    /// place content in scores above one they place nothing in, and every score is in the
    /// 0..1 range a metric is reported on.
    #[test]
    fn the_toxicity_metric_is_computed_from_the_response() {
        // (case, the response, whether the recognizers must place content in it)
        let rows = [
            ("an emulated completion", "No language model runs in this emulator.", false),
            ("a factual sentence", "The Pacific is the largest ocean on Earth.", false),
            ("a stated threat", "I will punch you in the face tomorrow.", true),
            ("a wished-for harm", "They should be shot for what they did.", true),
        ];
        // The score of a response the recognizers place nothing in, which every row of the
        // table is compared against: a metric score is computed from the response, so a
        // response nothing is placed in scores the bottom of the range.
        let benign = 0.0f64;
        let asked = record("any prompt", None);
        for (case, response, toxic) in rows {
            let score = metric_score(METRIC_TOXICITY, TASK_QUESTION_AND_ANSWER, &asked, response)
                .unwrap_or_else(|| panic!("case {case:?}: {METRIC_TOXICITY} is computed"));
            assert!(
                (0.0..=1.0).contains(&score),
                "case {case:?}: expected a score in 0..1, got {score}"
            );
            if toxic {
                assert!(
                    score > benign,
                    "case {case:?}: expected a score above the {benign} a response nothing is \
                     placed in scores, got {score}"
                );
            } else {
                assert_eq!(
                    score, benign,
                    "case {case:?}: the recognizers place nothing in this response, so it \
                     scores {benign}, got {score}"
                );
            }
        }
        let robustness = metric_score(
            "Builtin.Robustness",
            TASK_QUESTION_AND_ANSWER,
            &record("any prompt", Some("any answer")),
            "any response",
        );
        assert!(
            robustness.is_none(),
            "case \"a built-in metric this emulator computes no value for\": expected no score, \
             got {robustness:?}"
        );
    }

    /// RULE: `Builtin.Accuracy` is computed from the response and the record's own
    /// `referenceResponse` — the F1 overlap of the two for a question-answering task and
    /// whether the response carries the class for a classification one — and is reported as a
    /// metric no value was computed for where the record carries no reference or the task
    /// type's accuracy is published as the score of a model this emulator runs none of.
    #[test]
    fn the_accuracy_metric_is_computed_from_the_response_and_the_reference() {
        let rows = vec![
            AccuracyCase {
                case: "question answering, the reference answered word for word",
                task_type: TASK_QUESTION_AND_ANSWER,
                reference: Some("Paris"),
                response: "Paris",
                score: Some(1.0),
            },
            AccuracyCase {
                case: "question answering, the reference inside a sentence",
                task_type: TASK_QUESTION_AND_ANSWER,
                reference: Some("yes"),
                response: "Yes, it is.",
                // One of the three words answered is one of the one word expected.
                score: Some(2.0 * (1.0 / 3.0) * 1.0 / ((1.0 / 3.0) + 1.0)),
            },
            AccuracyCase {
                case: "question answering, a response sharing no word with the reference",
                task_type: TASK_QUESTION_AND_ANSWER,
                reference: Some("yes"),
                response: "The Pacific is the largest ocean.",
                score: Some(0.0),
            },
            AccuracyCase {
                case: "question answering, punctuation and case differing only",
                task_type: TASK_QUESTION_AND_ANSWER,
                reference: Some("366 days"),
                response: "366 DAYS.",
                score: Some(1.0),
            },
            AccuracyCase {
                case: "question answering, a record carrying no reference",
                task_type: TASK_QUESTION_AND_ANSWER,
                reference: None,
                response: "Paris",
                score: None,
            },
            AccuracyCase {
                case: "question answering, a reference of blank text",
                task_type: TASK_QUESTION_AND_ANSWER,
                reference: Some("   "),
                response: "Paris",
                score: None,
            },
            AccuracyCase {
                case: "classification, the class the record names",
                task_type: TASK_CLASSIFICATION,
                reference: Some("positive"),
                response: "This review is Positive overall.",
                score: Some(1.0),
            },
            AccuracyCase {
                case: "classification, another class",
                task_type: TASK_CLASSIFICATION,
                reference: Some("negative"),
                response: "This review is positive overall.",
                score: Some(0.0),
            },
            AccuracyCase {
                case: "summarization, published as the score of a model this emulator runs \
                       none of",
                task_type: "Summarization",
                reference: Some("A granite lighthouse."),
                response: "A granite lighthouse.",
                score: None,
            },
            AccuracyCase {
                case: "generation, published as a real-world knowledge score",
                task_type: "Generation",
                reference: Some("Paris"),
                response: "Paris",
                score: None,
            },
        ];
        for row in rows {
            let case = row.case;
            let asked = record("any prompt", row.reference);
            let score = metric_score(METRIC_ACCURACY, row.task_type, &asked, row.response);
            match (row.score, score) {
                (Some(expected), Some(score)) => {
                    assert!(
                        (0.0..=1.0).contains(&score) && (score - expected).abs() < 1e-9,
                        "case {case:?}: expected the score {expected}, got {score}"
                    );
                }
                (None, None) => {
                    let message =
                        unscored_metric_message(METRIC_ACCURACY, row.task_type, &asked);
                    assert!(
                        message.contains(METRIC_ACCURACY),
                        "case {case:?}: the record says which metric was not scored, got \
                         {message:?}"
                    );
                }
                (expected, score) => panic!(
                    "case {case:?}: expected the score {expected:?}, got {score:?}"
                ),
            }
        }
    }
}
