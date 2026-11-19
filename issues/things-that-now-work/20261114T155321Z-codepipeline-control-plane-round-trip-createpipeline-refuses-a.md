Issue-Source: trace-sweep-divergence
Issue-Trace: codepipeline-pipeline-source-and-deploy-configuration-round-trips
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: codepipeline-pipeline-source-and-deploy-configuration-round-trips codepipeline-list-pipelines-membership-and-pagination
Issue-Shape-Id: com.amazonaws.codepipeline#CodePipeline_20150709
Issue-Key: codepipeline|CreatePipeline|2xx->4xx|$.pipeline
Issue-Key: codepipeline|CreatePipeline|2xx->4xx|status
Issue-Key: codepipeline|ListPipelines|2xx->2xx|$.pipelines[name="sim2real-probe-codepipeline-lp-a-ce710cbd"].created
Issue-Key: codepipeline|ListPipelines|2xx->2xx|$.pipelines[name="sim2real-probe-codepipeline-lp-a-ce710cbd"].updated
Issue-Key: codepipeline|ListPipelines|2xx->2xx|$.pipelines[name="sim2real-probe-codepipeline-lp-b-caa93803"].created
Issue-Key: codepipeline|ListPipelines|2xx->2xx|$.pipelines[name="sim2real-probe-codepipeline-lp-b-caa93803"].updated
Issue-Commit: 4cf7211f237a5c930178a3131e9243c3c3eb5dd6

# CodePipeline control-plane round-trip: CreatePipeline refuses a well-formed CodeCommit-to-Lambda declaration with HTTP 400, and ListPipelines serializes created/updated at clock precision

> _Note: this issue was identified against the code base at commit `4cf7211f237a5c930178a3131e9243c3c3eb5dd6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePipeline

## Description
Two net-new root causes in the codepipeline control plane, both on the write/read path of a pipeline's own definition. 2 causes: a CreatePipeline that AWS accepts is refused with a 400, and the PipelineSummary timestamps ListPipelines reports are serialized at the emulator clock's precision instead of CodePipeline's wire form. Both must be closed before this issue is done.

Cause 1 of 2: CreatePipeline refuses a well-formed two-stage declaration (CodeCommit Source -> Lambda Invoke Deploy) with HTTP 400 and creates no pipeline (fixture codepipeline-pipeline-source-and-deploy-configuration-round-trips, step task#7).
AWS: Smithy CreatePipeline — input {pipeline: PipelineDeclaration (@required), tags: TagList}, output {pipeline: PipelineDeclaration, tags: TagList}, success status 200; every modeled error is a client 400 (ConcurrentModificationException, InvalidActionDeclarationException, InvalidBlockerDeclarationException, InvalidStageDeclarationException, InvalidStructureException, InvalidTagsException, LimitExceededException, PipelineNameInUseException, TooManyTagsException, ValidationException). The operation doc states only one structural requirement about the artifact store: "you must include either artifactStore or artifactStores in your pipeline, but you cannot use both". PipelineDeclaration requires name (PipelineName @length 1..100, @pattern ^[A-Za-z0-9.@\-_]+$), roleArn (RoleArn @pattern ^arn:aws(-[\w]+)*:iam::[0-9]{12}:role/) and stages. The recorded response for this step is HTTP 200 carrying the stored declaration back: artifactStore {type S3, location <artifact bucket>}, executionMode SUPERSEDED, pipelineType V2, version 1, roleArn an IAM role of the account, stage "Source" with one Source/AWS/CodeCommit/1 action (configuration RepositoryName, BranchName, PollForSourceChanges; inputArtifacts [], outputArtifacts [{name: <artifact>}], runOrder 1) and stage "Deploy" with one Invoke/AWS/Lambda/1 action (configuration FunctionName, UserParameters; inputArtifacts [{name: <artifact>}], outputArtifacts [], runOrder 1). So a declaration of exactly that shape — two stages, a source-only first stage, one non-source action, every consumed artifact produced by an earlier action, artifact counts inside each action type's published bounds, exactly one artifact store — is ACCEPTED by AWS and answered 200 with the declaration echoed back.
Emulator: the same step answers HTTP 400 and carries no pipeline member at all (replay: "status: expected 200, got 400" and "$.pipeline: missing"), so the create is refused and nothing is stored. Hypothesis: one of the pre-store gates in the CreatePipeline handler refuses a declaration AWS accepts — the handler runs, in order, (a) a role-existence probe that turns IAM's NoSuchEntity for the declaration's roleArn into InvalidStructureException "CodePipeline is not authorized to perform AssumeRole on role <roleArn>", then (b) the shared declaration walk, then (c) the per-action action-type resolution against the service's built-in action-type catalog, which refuses an action whose actionTypeId is not in that catalog (InvalidActionDeclarationException "ActionType (Category: '...', Provider: '...', Owner: '...', Version: '...') in action '<name>' is not available in region '<REGION>'") or whose inputArtifacts/outputArtifacts count exceeds the catalog's bound for that type. The declaration in this fixture satisfies every rule those gates state as written, and the failing combination that no other corpus pipeline carries is a CodeCommit source feeding an Invoke/AWS/Lambda action that declares one input artifact; the replay cluster records the refused status but not the error body, so WHICH of those gates fired has to be read off the handler. Whichever it is, the requirement is the one the recording states: this declaration is accepted with 200 and the stored declaration echoed back, not refused.
Why: the pipeline never exists. A client that creates a source-and-deploy pipeline this way gets a 400 for a declaration AWS stores, and every subsequent call against it — GetPipeline, GetPipelineState, StartPipelineExecution, ListPipelines membership, UpdatePipeline — fails or reports the pipeline missing. A CI/IaC caller cannot distinguish this from a genuinely malformed template, and the whole CodeCommit-to-Lambda deployment path is unreachable on the emulator.
Fix: the CreatePipeline handler's pre-store validation chain, which must accept this declaration: the role-existence gate and the shared declaration validator plus its per-action action-type resolution (see Traces for the file and line citations). The accepted path must answer 200 with the stored PipelineDeclaration (and tags only when the caller applied some).
Traces: codepipeline-pipeline-source-and-deploy-configuration-round-trips#task#7 (CreatePipeline, expected 200, actual 400). Components: services/service-com-amazonaws-codepipeline-codepipeline-20150709/src/ops/create_pipeline.rs lines 181-232 (role gate and the DeclarationProblem -> error mapping), src/state.rs lines 4363-4650 (validate_declaration), src/state/action_type_resolution.rs lines 175-213 (validate_action_type) and src/state/built_in_catalog.rs lines 219-427 (the published artifact bounds those checks read).

Cause 2 of 2: ListPipelines serializes PipelineSummary.created / PipelineSummary.updated with sub-millisecond precision (fixture codepipeline-list-pipelines-membership-and-pagination, step task#8).
AWS: Smithy ListPipelines -> ListPipelinesOutput.pipelines : PipelineList of PipelineSummary {name, version, pipelineType, executionMode, created : Timestamp ("The date and time the pipeline was created, in timestamp format"), updated : Timestamp}; codepipeline speaks awsJson1_1, whose default timestamp form is epoch-seconds as a JSON number. Recorded behavior only for the precision: every timestamp in this corpus's recorded CodePipeline responses carries at most three fractional digits (millisecond resolution, e.g. a recorded ListActionExecutions lastUpdateTime of 1793700369.272). Millisecond resolution is therefore the wire form of a CodePipeline timestamp; the instant itself is a per-run value, which is why the recording asserts the shape of created/updated rather than their value.
Emulator: the step answers 200 (expected 200) but every summary fails the timestamp shape assertion: created and updated come back as epoch-seconds numbers with seven fractional digits (this run: 1794671182.3194258 for one pipeline, 1794671182.3383875 for the other; those instants and the generated pipeline names are per-run values, not requirements). Hypothesis: the float seconds-since-epoch of the internal clock is written straight into the JSON body instead of being truncated or rounded to milliseconds first (state.rs's to_date_time builds the DateTime from secs + subsec_nanos with no millisecond quantization).
Why: a client that compares, digests or round-trips these numbers against AWS-produced values, or parses them with a millisecond-resolution timestamp parser, sees a numeric form AWS never emits; equality and fingerprint comparisons over ListPipelines responses fail even when the instant is irrelevant, and a consumer that re-sends the value as a millisecond epoch silently loses the sub-millisecond tail.
Fix: the codepipeline response serializer's epoch-seconds timestamp encoding, shared by the PipelineSummary projection the ListPipelines handler builds — it must emit at most millisecond resolution rather than the clock's native precision.
Traces: codepipeline-list-pipelines-membership-and-pagination#task#8 (ListPipelines). Same serializer, filed under their own issues for their other faults: codepipeline-s3-source-manual-approval-run-succeeds#task#9 ($.approvedAt got 1794671064.362193) and #task#12 ($.created/$.updated got 1794671059.3316019, latestExecution.lastStatusChange got 1794671064.362193).

Related open issues named rather than relied on: 20261111T061123Z-codepipeline-listpipelines-reports-pipelinetype-v1-and-timestamps-carry.md, 20261111T185123Z-codepipeline-listpipelines-summarizes-a-created-pipeline-as-pipelinetype.md, 20261112T032208Z-codepipeline-listpipelines-pipelinesummary-still-reports-v1-for-a-v2.md, 20261112T103336Z-codepipeline-listpipelines-summary-projection-reports-pipelinetype-v1-and.md, 20261112T195134Z-codepipeline-listpipelines-scalar-projection-pipelinetype-v1-for-a.md and 20261114T065137Z-codepipeline-listpipelines-serializes-pipelinesummary-created-updated-with-sub.md all describe Cause 2's serializer fault, but each binds its ListPipelines replay keys to the generated pipeline names of an earlier run, so this run's cluster is reported unowned (owned_by []) and none of their pointers re-runs this step. The pipelineType V1-vs-V2 half of those issues no longer diverges on this step; only created/updated do. No open issue mentions Cause 1 (20261111T064249Z-codepipeline-pipeline-declaration-validation-2-net-new-causes.md covers only the WORDING of two InvalidActionDeclarationException refusals of genuinely malformed declarations, not a refusal of a valid one).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true (/__admin/health 200, data listener accepting), 60 fixtures replayed, 344 steps, 321 passed, 23 mismatched, score_pct 93.3, 14 divergence clusters. Both clusters below report stub_path empty and owned_by [] (unowned).

Cause 1 — codepipeline-pipeline-source-and-deploy-configuration-round-trips#task#7, operation CreatePipeline, verdict mismatch, affected_count 1.
  status: expected 200, got 400.
  $.pipeline: missing — expected the stored declaration {artifactStore {location <generated artifact bucket>, type S3}, executionMode SUPERSEDED, name <generated pipeline name>, pipelineType V2, roleArn arn:aws:iam::000000000000:role/<generated role name>, version 1, stages [ {name Source, actions [{name Source, actionTypeId {category Source, owner AWS, provider CodeCommit, version 1}, configuration {BranchName main, PollForSourceChanges false, RepositoryName <generated repo>}, inputArtifacts [], outputArtifacts [{name src}], runOrder 1}]}, {name Deploy, actions [{name Deploy, actionTypeId {category Invoke, owner AWS, provider Lambda, version 1}, configuration {FunctionName <generated function>, UserParameters <JSON string>}, inputArtifacts [{name src}], outputArtifacts [], runOrder 1}]} ]} — got None.
  Expected: HTTP 200 with the declaration echoed back. Actual: HTTP 400 with no pipeline member, so nothing was stored. The cluster records the refused status only; the 400's error code and message are not in the replay detail, and all ten errors CreatePipeline models are 400s, so the body must be read from the handler. The bucket, repository, role, function and pipeline names above are per-run generated values and are not requirements; the requirement is that a declaration of this SHAPE is accepted with 200.

Cause 2 — codepipeline-list-pipelines-membership-and-pagination#task#8, operation ListPipelines, verdict mismatch, expected status 200, actual status 200, affected_count 1.
  $.pipelines[name="sim2real-probe-codepipeline-lp-a-ce710cbd"].created: value_mismatch (expected '<shaped-not-asserted>', got 1794671182.3194258)
  $.pipelines[name="sim2real-probe-codepipeline-lp-a-ce710cbd"].updated: value_mismatch (expected '<shaped-not-asserted>', got 1794671182.3194258)
  $.pipelines[name="sim2real-probe-codepipeline-lp-b-caa93803"].created: value_mismatch (expected '<shaped-not-asserted>', got 1794671182.3383875)
  $.pipelines[name="sim2real-probe-codepipeline-lp-b-caa93803"].updated: value_mismatch (expected '<shaped-not-asserted>', got 1794671182.3383875)
  Expected wire form: an epoch-seconds JSON number with at most three fractional digits, as in every recorded CodePipeline timestamp in this corpus (e.g. 1793700369.272). Actual: seven fractional digits. The pipeline names and the instants themselves are per-run values; the requirement under test is the serialized precision.

Model consulted: services/service-com-amazonaws-codepipeline-codepipeline-20150709/smithy-model/com-amazonaws-codepipeline-codepipeline-20150709.json — CreatePipeline (input pipeline: PipelineDeclaration @required, tags: TagList; output pipeline + tags; errors ConcurrentModificationException, InvalidActionDeclarationException, InvalidBlockerDeclarationException, InvalidStageDeclarationException, InvalidStructureException, InvalidTagsException, LimitExceededException, PipelineNameInUseException, TooManyTagsException, ValidationException — all client 400), PipelineDeclaration (name/roleArn/stages @required; roleArn @pattern ^arn:aws(-[\w]+)*:iam::[0-9]{12}:role/; artifactStore vs artifactStores note), InvalidStructureException (client, httpError 400, message @length 1..5000), ListPipelines output.pipelines: PipelineList, and PipelineSummary (created/updated: Timestamp).

## Steps to Reproduce
Cause 1 (CreatePipeline refused):
1. Build and boot the emulator from this checkout (/__admin/health answers 200).
2. Replay fixture codepipeline-pipeline-source-and-deploy-configuration-round-trips. Its earlier steps create a versioned S3 artifact bucket, a CodeCommit repository, a Lambda function, a Glue job/role and an IAM role for CodePipeline — all of those steps pass.
3. At step task#7 call CreatePipeline with a two-stage declaration: artifactStore {type S3, location <the bucket>}, pipelineType V2, executionMode SUPERSEDED, roleArn the created role, stage "Source" holding one Source/AWS/CodeCommit/1 action (configuration RepositoryName/BranchName/PollForSourceChanges, inputArtifacts [], outputArtifacts [{name src}], runOrder 1) and stage "Deploy" holding one Invoke/AWS/Lambda/1 action (configuration FunctionName/UserParameters, inputArtifacts [{name src}], outputArtifacts [], runOrder 1).
4. Observe HTTP 400 with no pipeline in the body, where AWS answers 200 and echoes the stored declaration. Read the returned error code/message to see which pre-store gate fired (the roleArn existence probe in src/ops/create_pipeline.rs, or state::validate_declaration / state::validate_action_type).
5. Confirm nothing was stored: ListPipelines does not contain the pipeline and GetPipeline on it answers PipelineNotFoundException.

Cause 2 (ListPipelines timestamp precision):
1. Against the same emulator, replay fixture codepipeline-list-pipelines-membership-and-pagination.
2. At step task#8, after the fixture's two CreatePipeline calls, call ListPipelines.
3. Read each PipelineSummary's created and updated: both are epoch-seconds JSON numbers with six or seven fractional digits (e.g. 1794671182.3194258).
4. Compare with the recorded CodePipeline responses in this corpus, where every timestamp carries at most three fractional digits (e.g. 1793700369.272). The step replays 200/200 and fails only the timestamp shape assertion, on all four keys.

## Resolution

Cause 2 — closed. Every timestamp this service puts on the wire is encoded by one encoder
(`state::to_date_time`), which truncates to millisecond resolution, so no projection can report
an instant at a different precision from another's. The four `ListPipelines` replay keys of this
issue report `fixed`. Pinned by `core/router/tests/all/codepipeline_source_and_deploy_declaration.rs`,
over the `ListPipelines` summaries and each `GetPipeline` metadata, through the one body walk the
sibling suite `codepipeline_action_execution_provenance.rs` owns.

Cause 1 — the declaration IS accepted; no pre-store gate refuses it. `CreatePipeline` serves this
exact declaration with HTTP 200 and stores it: in the same replay, the fixture's own
`setup#5` step sends the identical request and is answered 200, and the new suite above asserts
acceptance and the `GetPipeline` round trip over a table of such declarations (CodeCommit source
to Lambda invoke, S3 source to S3 deploy, a three-stage pipeline with an approval between
producer and consumer, and an invoke action at the input-artifact maximum its action type
publishes).

The two `codepipeline|CreatePipeline|2xx->4xx|…` replay keys stay red because that fixture
replays this one create TWICE in a single emulator — once in its setup phase (`setup#5`) and
again as the graded step (`task#7`) — so the second call names a pipeline the account already
holds. Instrumented replays established this: at `task#7` the store already held a pipeline under
the requested name, created ~38 ms earlier, with a structure `state::declarations_match` reports
identical, in the same Region and account, after exactly two creates of that name and with no
`DeletePipeline` in between; refusing the first create instead made the harness name it
`…#setup#5`, whose own recorded expectation is HTTP 200.

`PipelineNameInUseException` is the modelled answer for that second call ("The specified pipeline
name is already in use"; `CreatePipeline` carries no idempotency token), so answering it 200 —
even for a byte-identical declaration — would contradict both the model and this repo's recorded
behaviour, which
`services/service-com-amazonaws-codepipeline-codepipeline-20150709/tests/all/validation_and_messages.rs`
pins by sending one body twice and requiring that refusal. The new suite asserts that rule too,
for the identical structure and for a different one, and that the refused create leaves the
stored pipeline and its version untouched. The remaining red keys are therefore a property of the
fixture's setup/task split, not of this service: re-file them against the fixture if they are to
be closed.
