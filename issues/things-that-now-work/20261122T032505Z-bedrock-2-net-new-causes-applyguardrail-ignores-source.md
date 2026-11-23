Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock.apply-guardrail-pii-anonymize-masks-output bedrock.e2e-evaluation-job-runs-and-is-purged
Issue-Key: bedrock.apply-guardrail-pii-anonymize-masks-output|run#23|AssertionError|assert as_input["action"] == "NONE", (
Issue-Key: bedrock.e2e-evaluation-job-runs-and-is-purged|_poll_until#12|Timeout|time.sleep(interval_s)
Issue-Commit: e07d126abed0042eb6aab083a907fb217beb8cc3

# bedrock: 2 net-new causes — ApplyGuardrail ignores `source` so an ANONYMIZE PII entity masks request-side content, and an automated evaluation job never converges to a terminal status

> _Note: this issue was identified against the code base at commit `e07d126abed0042eb6aab083a907fb217beb8cc3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ApplyGuardrail

## Description
Two net-new root causes found by the behavioral suite, both under the `bedrock` service heading. A fixer must close BOTH before submitting.

Neither is covered by an open bedrock guardrail/evaluation issue: 20261111T062636Z-bedrock-runtime-applyguardrail-response-omits-actionreason-output-blockedresponse.md is about the SHAPE and counter VALUES of the ApplyGuardrail 200 body (actionReason/output/blockedResponse/usage counters), not about whether `source` gates which policies run — in this replay actionReason and the masked outputs were present and correct on source=OUTPUT, and the divergence is that the same evaluation also fires on source=INPUT. 20261111T094120Z (Converse output-side guardrail) and 20261029T172312Z (control-plane over-permissiveness) are different surfaces. 20261026T233704Z and 20261030T151259Z cover BatchDeleteEvaluationJob's refusal/validation WORDING, not an evaluation job's status convergence.

Cause 1 of 2: ApplyGuardrail ignores the required `source` discriminator, so a response-side ANONYMIZE transform is applied to request-side content.
AWS: Smithy (bedrock-runtime) ApplyGuardrail input member `source` targets GuardrailContentSource (@required, enum INPUT|OUTPUT); the sensitive-information policy's PII action enum on the assessment side is GuardrailSensitiveInformationPolicyAction (ANONYMIZED|BLOCKED|NONE), and GuardrailUsage.sensitiveInformationPolicyUnits is an @required integer counting the sensitive-information text units PROCESSED. A guardrail whose sensitiveInformationPolicyConfig.piiEntitiesConfig declares {type: EMAIL, action: ANONYMIZE} is a response-side transform: evaluated with source=OUTPUT on text containing an email address, AWS answers 200 with action="GUARDRAIL_INTERVENED", actionReason="Guardrail masked." (fixed text, whole value), outputs = the request text byte-identical except the matched entity replaced by its type token in braces ("{EMAIL}" for type EMAIL — the token is derived from the entity type, not from any one recording), assessments[0].sensitiveInformationPolicy.piiEntities containing {type: "EMAIL", action: "ANONYMIZED", match: <the matched text from the request>, detected: true}, and usage.sensitiveInformationPolicyUnits = 1. Recorded behavior only (the model does not state it): the SAME guardrail version and the SAME text evaluated with source=INPUT does not run the sensitive-information policy at all — action="NONE", actionReason="No action." (fixed text), outputs = [] (present and empty), no piiEntities entry, and usage.sensitiveInformationPolicyUnits = 0. The unit count tracks the policy being EVALUATED rather than an entity being matched: benign text on source=OUTPUT still bills sensitiveInformationPolicyUnits = 1, which is what makes 0 on source=INPUT mean "never ran".
Emulator: the source=OUTPUT half is correct, but with source=INPUT and the identical guardrail version and text the emulator answers action='GUARDRAIL_INTERVENED' with actionReason='Guardrail masked.' — it runs the ANONYMIZE entity on request-side content, so `source` is not consulted when selecting which policies to evaluate (and, by the same token, the sensitive-information unit accounting cannot be gated on whether the policy ran).
Why: ApplyGuardrail with source=INPUT is the standard pre-screen a client runs on a user prompt before inference. Against the emulator that pre-screen reports an intervention where AWS reports none, and hands back a rewritten prompt with the user's email replaced by {EMAIL}: the client either aborts a prompt AWS would have passed through, or sends the model altered text. Clients that branch on action/actionReason to distinguish "masked" from "clean" take the wrong branch, and billing/metering derived from sensitiveInformationPolicyUnits is charged for a policy evaluation AWS never performed.
Fix: the ApplyGuardrail handler's guardrail policy-engine dispatch and its usage accounting — select the policies to evaluate from the request's `source` (ANONYMIZE PII entities are response-side only) and derive sensitiveInformationPolicyUnits from whether the sensitive-information policy actually ran, then fall through to the non-intervening response (action=NONE, actionReason="No action.", outputs=[]).
Traces: bedrock.apply-guardrail-pii-anonymize-masks-output.

Cause 2 of 2: an automated evaluation job never converges to a terminal status, so the Completed/S3-output/purge lifecycle is unreachable.
AWS: Smithy (bedrock) EvaluationJobStatus = InProgress|Completed|Failed|Stopping|Stopped|Deleting. CreateEvaluationJob accepted with an automated datasetMetricConfigs entry returns a jobArn, and the job advances on the service's own clock from InProgress to the terminal Completed, at which point GetEvaluationJob reports status="Completed" and the objects produced under outputDataConfig.s3Uri are listable in S3. BatchDeleteEvaluationJob on a terminal job then returns errors=[] and an evaluationJobs entry per job (BatchDeleteEvaluationJobItem{jobIdentifier @required, jobStatus @required}), after which GetEvaluationJob answers ResourceNotFoundException and ListEvaluationJobs no longer lists the jobArn.
Emulator: the check never observes a terminal status — it ran out its budget inside the GetEvaluationJob poll ("Check timed out after 60.0s"), so the job stays non-terminal and none of the downstream assertions (Completed, S3 output objects present, batch-delete errors=[], purge visible as ResourceNotFoundException, absence from ListEvaluationJobs) is ever reached. The status the emulator serves never leaves its non-terminal value; there is no real compute behind an emulated job, so there is nothing for it to wait on.
Why: an evaluation job is a state machine clients poll. A job that never reaches Completed makes every automated-evaluation workflow hang against the emulator, and the terminal-state-only paths behind it — retrieving results from S3 and purging with BatchDeleteEvaluationJob — can never be exercised, so a client cannot be tested end to end at all.
Fix: the bedrock evaluation-job state in the control-plane crate plus the CreateEvaluationJob/GetEvaluationJob handlers — advance a created automated job through InProgress to Completed promptly on the emulator's own clock and materialize the output objects under the job's outputDataConfig.s3Uri prefix, so the terminal-state paths (GetEvaluationJob status=Completed, BatchDeleteEvaluationJob purge, ListEvaluationJobs omission) become reachable.
Note on the budget: the check's own poll allows far longer than the harness budget it was cut off by, so the exact convergence latency is not the requirement here — reaching a terminal status at all is.
Traces: bedrock.e2e-evaluation-job-runs-and-is-purged.

## Evidence
Behavioral suite replay (simparity-behavioral-aws-v1): 172/205 passed; 33 failures, of which 31 are marked OWNED by an already-open issue. The two below are the unowned remainder, both listed under the suite's `### bedrock` heading.

Cause 1 — [mismatch] bedrock.apply-guardrail-pii-anonymize-masks-output:
  AssertionError: an ANONYMIZE-configured entity is a response-side transform: with source=INPUT the same text must not intervene; got action='GUARDRAIL_INTERVENED' actionReason='Guardrail masked.'
  The check creates a guardrail with sensitiveInformationPolicyConfig.piiEntitiesConfig=[{type: EMAIL, action: ANONYMIZE}], publishes a version, waits for READY, and calls bedrock-runtime ApplyGuardrail (guardrailIdentifier/guardrailVersion/source/content) on one sentence containing an email address.
  - source=OUTPUT: all assertions passed (action=GUARDRAIL_INTERVENED, actionReason="Guardrail masked.", piiEntities entry type=EMAIL action=ANONYMIZED match=<the email> detected=true, usage.sensitiveInformationPolicyUnits=1, outputs = the text with the email replaced by "{EMAIL}").
  - source=INPUT, same guardrail version, same text: expected action="NONE" (with outputs=[], usage.sensitiveInformationPolicyUnits=0, no piiEntities entry); got action='GUARDRAIL_INTERVENED', actionReason='Guardrail masked.' — the assertion at the source-asymmetry step is the first to fail, so the later INPUT-side assertions (outputs==[], units==0, no piiEntities) and the benign-control assertions were not reached.
  Model citation: Smithy com.amazonaws.bedrockruntime ApplyGuardrail input {guardrailIdentifier @required @httpLabel, guardrailVersion @required @httpLabel, source: GuardrailContentSource @required (INPUT|OUTPUT), content @required, outputScope}; GuardrailSensitiveInformationPolicyAction = ANONYMIZED|BLOCKED|NONE; GuardrailUsage.sensitiveInformationPolicyUnits @required integer ("units processed"); GuardrailPiiEntityFilter{match @required, type @required, action @required, detected}. The INPUT/OUTPUT asymmetry itself and the fixed strings "Guardrail masked." / "No action." are recorded behavior.

Cause 2 — [mismatch] bedrock.e2e-evaluation-job-runs-and-is-purged:
  Error: Check timed out after 60.0s
  The check creates an S3 bucket and an IAM role, calls bedrock CreateEvaluationJob with evaluationConfig.automated.datasetMetricConfigs=[{taskType: QuestionAndAnswer, dataset Builtin.BoolQ, metricNames [Builtin.Accuracy]}], inferenceConfig.models=[{bedrockModel.modelIdentifier: <foundation-model ARN>}] and outputDataConfig.s3Uri, then polls GetEvaluationJob for a terminal status in (Completed, Failed, Stopped). No terminal status is ever observed, so the run ends inside that poll and the following assertions are never evaluated: status=="Completed"; ListObjectsV2 KeyCount>0 under the eval-output/ prefix; BatchDeleteEvaluationJob errors==[] with an evaluationJobs entry; GetEvaluationJob -> ResourceNotFoundException after the purge; jobArn absent from ListEvaluationJobs.
  Model citation: Smithy com.amazonaws.bedrock EvaluationJobStatus = InProgress|Completed|Failed|Stopping|Stopped|Deleting; BatchDeleteEvaluationJobItem{jobIdentifier @required, jobStatus @required}.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Run the behavioral suite (simparity-behavioral-aws-v1), or replay just the two checks below.

Cause 1 (bedrock.apply-guardrail-pii-anonymize-masks-output):
  a. bedrock CreateGuardrail with blockedInputMessaging/blockedOutputsMessaging and sensitiveInformationPolicyConfig={"piiEntitiesConfig":[{"type":"EMAIL","action":"ANONYMIZE"}]}.
  b. CreateGuardrailVersion, then poll GetGuardrail until status=READY.
  c. bedrock-runtime ApplyGuardrail(guardrailIdentifier=<id>, guardrailVersion=<version>, source="OUTPUT", content=[{"text":{"text":"<one sentence containing an email address>"}}]) — observe the masking verdict (this half matches AWS).
  d. Repeat (c) with source="INPUT", identical version and text. AWS: action="NONE", actionReason="No action.", outputs=[], usage.sensitiveInformationPolicyUnits=0, no piiEntities entry. Emulator: action="GUARDRAIL_INTERVENED", actionReason="Guardrail masked.".
  e. Also call (c) with benign text and source="OUTPUT": AWS still reports usage.sensitiveInformationPolicyUnits=1 with action="NONE" and actionReason="No action.", which is what makes 0 units on source=INPUT mean the policy never ran.

Cause 2 (bedrock.e2e-evaluation-job-runs-and-is-purged):
  a. Create an S3 bucket and an IAM role trusted by bedrock.amazonaws.com with s3/bedrock access.
  b. bedrock CreateEvaluationJob with an automated datasetMetricConfigs entry, inferenceConfig.models=[{bedrockModel.modelIdentifier: arn:aws:bedrock:<region>::foundation-model/amazon.nova-lite-v1:0}] and outputDataConfig.s3Uri=s3://<bucket>/eval-output/.
  c. Poll GetEvaluationJob(jobIdentifier=<jobArn>) — the status never reaches a terminal value (Completed/Failed/Stopped); it stays non-terminal indefinitely.
  d. Consequently ListObjectsV2 under eval-output/ returns no objects, and BatchDeleteEvaluationJob -> GetEvaluationJob ResourceNotFoundException -> ListEvaluationJobs omission can never be exercised.
