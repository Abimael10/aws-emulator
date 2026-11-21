// SPDX-License-Identifier: CC0-1.0

//! An accepted automated evaluation job RUNS from the call that accepted it, and a client
//! that never reads the job still finds its results — then purges the job and sees it gone.
//!
//! RULE: the work an accepted asynchronous job owes from the moment it is accepted belongs to
//! the call that ACCEPTS it, not to a call that reports on it. `CreateEvaluationJob` answers
//! the modelled 202 and the evaluation runs after that answer — the records of every dataset
//! its `datasetMetricConfigs` names, invoked on the model its `inferenceConfig` names and
//! saved under `outputDataConfig.s3Uri` ("The Amazon S3 location where the results of your
//! evaluation job are saved",
//! `smithy_query(query="shape",shape="EvaluationOutputDataConfig")`) — so a `ListObjectsV2`
//! over that prefix reports the results WITHOUT anything having read the job. A reporting
//! operation materialises only the status transition the clock already owed and answers the
//! stored record: `GetEvaluationJob` reports `InProgress` until the job's completion falls due
//! and the terminal status its run ended in after it, and `BatchDeleteEvaluationJob` then
//! purges the job so a read of its ARN answers 404 and `ListEvaluationJobs` stops reporting
//! it.
//!
//! That ordering is the half this suite adds to
//! `all/bedrock_evaluation_job_saves_its_results_in_s3.rs`, whose harness it shares: the
//! results are asserted present with NO call to the control plane in between, so a run
//! carried inside the first read of the job cannot satisfy it, and the lifecycle is then
//! walked to the purge the documented flow ends in.
//!
//! The rows differ along what the rule generalises over: the task type and built-in dataset
//! whose records are evaluated, how many metrics the entry configures, the shape of the output
//! location (a prefix, a nested prefix, the bucket itself) and the spelling of the evaluated
//! model (a `foundation-model` ARN, a bare model id).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::bedrock_evaluation_job_saves_its_results_in_s3::{
    bedrock, emulator, given_bucket, given_role, listed_keys, poll_until_terminal, s3, BUCKET,
    COMPLETE_AFTER, EVALUATED_MODEL, EVALUATED_MODEL_ARN, METRIC_ACCURACY, METRIC_TOXICITY,
    POLL_BUDGET, POLL_STEP,
};
use crate::cloudfront_key_value_store_handshake::segment as form_encode;

use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::state::evaluation_job_not_found;

/// One row: the job to submit, and where its results are to be found.
struct Row {
    case: &'static str,
    name: &'static str,
    /// `taskType` — a value of the modelled `EvaluationTaskType`.
    task_type: &'static str,
    /// `dataset.name` — a built-in prompt dataset.
    dataset: &'static str,
    /// `metricNames`.
    metrics: Vec<&'static str>,
    /// `inferenceConfig.models[0].bedrockModel.modelIdentifier`, in one of the two spellings
    /// `ModelIdentifier` admits.
    model_identifier: &'static str,
    /// `outputDataConfig.s3Uri`, as the submission spells it.
    output_uri: String,
    /// The key prefix a reader of the output lists.
    prefix: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "question answering over the built-in yes-or-no dataset, under a prefix, by \
                   the model's ARN",
            name: "eval-purged-question-answer",
            task_type: "QuestionAndAnswer",
            dataset: "Builtin.BoolQ",
            metrics: vec![METRIC_ACCURACY],
            model_identifier: EVALUATED_MODEL_ARN,
            output_uri: format!("s3://{BUCKET}/eval-output/"),
            prefix: "eval-output",
        },
        Row {
            case: "classification over the built-in review dataset, scored for two metrics, \
                   under a nested prefix, by the model's bare id",
            name: "eval-purged-classification",
            task_type: "Classification",
            dataset: "Builtin.WomensEcommerceClothingReviews",
            metrics: vec![METRIC_ACCURACY, METRIC_TOXICITY],
            model_identifier: EVALUATED_MODEL,
            output_uri: format!("s3://{BUCKET}/reports/2027/q2"),
            prefix: "reports/2027/q2",
        },
        Row {
            case: "generation over the built-in open-ended dataset, under the bucket itself, \
                   by the model's ARN",
            name: "eval-purged-generation",
            task_type: "Generation",
            dataset: "Builtin.Bold",
            metrics: vec![METRIC_TOXICITY],
            model_identifier: EVALUATED_MODEL_ARN,
            output_uri: format!("s3://{BUCKET}"),
            prefix: "eval-purged-generation",
        },
    ]
}

impl Row {
    /// The result object this row's run saves: the layout a reader of the output walks.
    fn expected_key(&self, job_id: &str) -> String {
        let path = format!(
            "{}/{job_id}/models/{EVALUATED_MODEL}/taskTypes/{}/datasets/{}/output.jsonl",
            self.name, self.task_type, self.dataset
        );
        match self
            .output_uri
            .trim_end_matches('/')
            .strip_prefix(&format!("s3://{BUCKET}/"))
        {
            Some(prefix) => format!("{prefix}/{path}"),
            None => path,
        }
    }
}

/// The submission body of one row.
fn submission(row: &Row, role_arn: &str) -> Value {
    json!({
        "jobName": row.name,
        "roleArn": role_arn,
        "outputDataConfig": {"s3Uri": row.output_uri},
        "evaluationConfig": {"automated": {"datasetMetricConfigs": [{
            "taskType": row.task_type,
            "dataset": {"name": row.dataset},
            "metricNames": row.metrics,
        }]}},
        "inferenceConfig": {"models": [
            {"bedrockModel": {"modelIdentifier": row.model_identifier}},
        ]},
    })
}

/// List the output prefix until it reports `key`, WITHOUT reading the job: the results are
/// saved by the run the submission started, so nothing has to touch the control plane for
/// them to appear. The wait is for the CONDITION, bounded by a budget generous next to the
/// span the completion is owed in.
async fn keys_appear_without_reading_the_job(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
    key: &str,
) {
    let started = ::std::time::Instant::now();
    loop {
        let keys = listed_keys(router, case, prefix).await;
        if keys.iter().any(|listed| listed == key) {
            return;
        }
        assert!(
            started.elapsed() < POLL_BUDGET,
            "case {case:?}: the results of an accepted job must appear under {prefix:?} \
             without the job being read (the run span is {COMPLETE_AFTER:?}), expected \
             {key:?}, listing reports {keys:?}"
        );
        ::tokio::time::sleep(POLL_STEP).await;
    }
}

/// Read the job until its ARN names nothing: the purge the batch delete accepted is owed and
/// a read reports it once it has taken effect.
async fn poll_until_purged(router: &MultiServiceRouter, case: &str, arn: &str) {
    let started = ::std::time::Instant::now();
    loop {
        let (status, document) = bedrock(
            router,
            "GET",
            &format!("/evaluation-jobs/{}", form_encode(arn)),
            None,
        )
        .await;
        if status == 404 {
            let message = document.get("message").and_then(Value::as_str).unwrap_or("");
            assert_eq!(
                message,
                evaluation_job_not_found(arn),
                "case {case:?}: a purged job's ARN must be reported as naming no job, got \
                 {document:?}"
            );
            return;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: a purged job's ARN is either read or gone, got {status} with \
             {document:?}"
        );
        assert!(
            started.elapsed() < POLL_BUDGET,
            "case {case:?}: a batch-deleted job must stop being readable inside \
             {POLL_BUDGET:?}, still answered {document:?}"
        );
        ::tokio::time::sleep(POLL_STEP).await;
    }
}

/// Every `jobArn` `ListEvaluationJobs` reports in this account-region.
async fn listed_job_arns(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, document) = bedrock(router, "GET", "/evaluation-jobs", None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the listing must be served, got {status} with {document:?}"
    );
    document
        .get("jobSummaries")
        .and_then(Value::as_array)
        .map(|summaries| {
            summaries
                .iter()
                .filter_map(|summary| summary.get("jobArn").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn an_accepted_evaluation_job_saves_its_results_before_anything_reads_it_and_is_purged() {
    let router = emulator();
    given_bucket(&router).await;
    let role_arn = given_role(&router).await;

    for row in rows() {
        let case = row.case;
        let (status, accepted) = bedrock(
            &router,
            "POST",
            "/evaluation-jobs",
            Some(submission(&row, &role_arn)),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: the submission must be accepted, got {status} with {accepted:?}"
        );
        let arn = accepted
            .get("jobArn")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            !arn.is_empty(),
            "case {case:?}: the submission answers the job's ARN, got {accepted:?}"
        );
        let job_id = arn.rsplit('/').next().unwrap_or_default().to_string();

        // THE RULE: the results are there without the control plane having been called again.
        let key = row.expected_key(&job_id);
        keys_appear_without_reading_the_job(&router, case, row.prefix, &key).await;
        let (status, body) = s3(&router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the result object must be readable, got {status} with {body:?}"
        );
        let records: Vec<Value> = body
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|error| panic!("case {case:?}: {line:?} is not JSON: {error}"))
            })
            .collect();
        assert!(
            !records.is_empty(),
            "case {case:?}: the result object carries the evaluated records, got {body:?}"
        );
        for (position, record) in records.iter().enumerate() {
            assert!(
                record
                    .pointer("/modelResponses/0/response")
                    .and_then(Value::as_str)
                    .is_some_and(|response| !response.is_empty()),
                "case {case:?}: record {position} carries what the inference answered, got \
                 {record}"
            );
        }

        // Only now is the job read: it reports the terminal status its run ended in.
        let settled = poll_until_terminal(&router, case, &arn).await;
        assert_eq!(
            settled.get("status").and_then(Value::as_str),
            Some("Completed"),
            "case {case:?}: a job whose run was performed reaches Completed, got {settled:?}"
        );
        assert!(
            settled.get("failureMessages").is_none(),
            "case {case:?}: a job that ran reports no failure, got {settled:?}"
        );

        // And the purge the documented flow ends in: the delete is accepted for a job in a
        // terminal status, reports it under its bare id, and the job then names nothing.
        let (status, deleted) = bedrock(
            &router,
            "POST",
            "/evaluation-jobs/batch-delete",
            Some(json!({"jobIdentifiers": [arn]})),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: the purge must be accepted, got {status} with {deleted:?}"
        );
        assert!(
            deleted
                .get("errors")
                .and_then(Value::as_array)
                .is_some_and(|errors| errors.is_empty()),
            "case {case:?}: the purge of a terminal job reports no errors, got {deleted:?}"
        );
        assert_eq!(
            deleted.pointer("/evaluationJobs/0/jobIdentifier"),
            Some(&json!(job_id)),
            "case {case:?}: the accepted item names the job's bare id, got {deleted:?}"
        );
        assert_eq!(
            deleted.pointer("/evaluationJobs/0/jobStatus"),
            Some(&json!("Deleting")),
            "case {case:?}: the accepted item reports the job as being deleted, got {deleted:?}"
        );

        poll_until_purged(&router, case, &arn).await;
        let listed = listed_job_arns(&router, case).await;
        assert!(
            !listed.contains(&arn),
            "case {case:?}: a purged job must stop being listed, got {listed:?}"
        );
    }
}
