Issue-Source: trace-sweep-divergence
Issue-Trace: codepipeline-list-pipelines-membership-and-pagination
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: codepipeline-list-pipelines-membership-and-pagination
Issue-Shape-Id: com.amazonaws.codepipeline#CodePipeline_20150709
Issue-Key: codepipeline|ListPipelines|2xx->2xx|$.pipelines[name="sim2real-probe-codepipeline-lp-a-27c43846"].created
Issue-Key: codepipeline|ListPipelines|2xx->2xx|$.pipelines[name="sim2real-probe-codepipeline-lp-a-27c43846"].updated
Issue-Key: codepipeline|ListPipelines|2xx->2xx|$.pipelines[name="sim2real-probe-codepipeline-lp-b-f136c15c"].created
Issue-Key: codepipeline|ListPipelines|2xx->2xx|$.pipelines[name="sim2real-probe-codepipeline-lp-b-f136c15c"].updated
Issue-Commit: d55a3288bccfe55e7c52387cacb77214d56a079f

# CodePipeline ListPipelines serializes PipelineSummary created and updated at native clock precision

> _Note: this issue was identified against the code base at commit `d55a3288bccfe55e7c52387cacb77214d56a079f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListPipelines

## Description
One cause.

Cause 1 of 1: ListPipelines emits PipelineSummary.created / PipelineSummary.updated as epoch-seconds numbers carrying the full fractional precision of the emulator's internal clock, a value shape CodePipeline never puts on the wire (fixture codepipeline-list-pipelines-membership-and-pagination, step task#8).

AWS: Smithy (services/service-com-amazonaws-codepipeline-codepipeline-20150709/smithy-model/com-amazonaws-codepipeline-codepipeline-20150709.json) ListPipelines output.pipelines targets PipelineList of PipelineSummary; PipelineSummary.created and PipelineSummary.updated both target Timestamp ("The date and time the pipeline was created, in timestamp format." / "The date and time of the last update to the pipeline, in timestamp format."). The service speaks awsJson1_1, whose default timestamp form is epoch-seconds as a JSON number. The model constrains the type but not the precision, so for precision this is recorded behavior only: every CodePipeline timestamp in the recorded corpus carries at most three fractional digits, i.e. millisecond resolution (e.g. a recorded ListActionExecutions lastUpdateTime of 1793700369.272). Millisecond resolution is therefore the wire form of a CodePipeline timestamp. The instants themselves are per-run values and are not requirements; the recording does not assert them (expected '<shaped-not-asserted>'), only their shape.

Emulator: it writes the clock's native precision straight out. On this step both summaries' created and updated come back with six or more fractional digits. Hypothesis: a floating-point seconds-since-epoch is serialized directly instead of being truncated/rounded to milliseconds, which is why created and updated of a pipeline are also bit-identical to each other rather than merely close.

Why: a client that compares, digests or round-trips these numbers against AWS-recorded responses, or that parses them with a millisecond-resolution timestamp parser (as the AWS SDKs' epoch-seconds decoders do), sees a number shape AWS never emits. Equality and checksum comparisons over recorded ListPipelines responses fail even when the instant is irrelevant, and a parser that rejects extra precision errors on a response AWS would have accepted.

Fix: the codepipeline response serializer's epoch-seconds timestamp encoding — the shared Timestamp encoder used by the ListPipelines handler's PipelineSummary projection — must round or truncate to millisecond resolution rather than emitting the clock's native precision. This is one serializer and not one operation: the same shape shows on GetPipelineState created/updated/lastStatusChange and PutApprovalResult approvedAt in this same replay (those steps are owned by other open issues for their other faults).

Related open issues, none of which owns this replay key — the replay reports this cluster as unowned (owned_by empty): 20261111T061123Z-codepipeline-listpipelines-reports-pipelinetype-v1-and-timestamps-carry.md, 20261111T185123Z-codepipeline-listpipelines-summarizes-a-created-pipeline-as-pipelinetype.md, 20261112T032208Z-codepipeline-listpipelines-pipelinesummary-still-reports-v1-for-a-v2.md, 20261112T103336Z-codepipeline-listpipelines-summary-projection-reports-pipelinetype-v1-and.md, 20261112T195134Z-codepipeline-listpipelines-scalar-projection-pipelinetype-v1-for-a.md, 20261114T065137Z-codepipeline-listpipelines-serializes-pipelinesummary-created-updated-with-sub.md and 20261114T155321Z-codepipeline-control-plane-round-trip-createpipeline-refuses-a.md all describe a ListPipelines scalar-projection fault; their recorded keys are bound to earlier runs' generated pipeline names, so this cluster's key is not carried by any of them. Note that the pipelineType V1-for-V2 half of those issues no longer reproduces in this replay: only the timestamp precision does.

Traces: codepipeline-list-pipelines-membership-and-pagination#task#8 (ListPipelines).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, /__admin/health answered 200, data listener accepts; steps_total 344, steps_passed 321).

Cluster: fixture codepipeline-list-pipelines-membership-and-pagination, step task#8, operation ListPipelines, verdict mismatch, expected status 200, actual status 200 (the status and shape are right; only the scalar serialization diverges). affected_count 1. owned_by: empty — no open issue carries this replay key. stub_path: empty.

Diverging keys reported by the replay (expected value not asserted by the recording, i.e. '<shaped-not-asserted>'):
  $.pipelines[name="sim2real-probe-codepipeline-lp-a-27c43846"].created  -> got 1794674116.713714
  $.pipelines[name="sim2real-probe-codepipeline-lp-a-27c43846"].updated  -> got 1794674116.713714
  $.pipelines[name="sim2real-probe-codepipeline-lp-b-f136c15c"].created  -> got 1794674116.7358925
  $.pipelines[name="sim2real-probe-codepipeline-lp-b-f136c15c"].updated  -> got 1794674116.7358925

Six and seven fractional digits respectively, against recorded CodePipeline timestamps that carry at most three (e.g. 1793700369.272 in the recorded ListActionExecutions of this corpus).

Per-run values that are NOT requirements: the pipeline names above (sim2real-probe-codepipeline-lp-a-27c43846, sim2real-probe-codepipeline-lp-b-f136c15c are generated per run), and the instants 1794674116.713714 / 1794674116.7358925 themselves. The requirement is the fractional precision of the serialized epoch-seconds number for PipelineSummary.created and PipelineSummary.updated.

Corroborating steps showing the same serializer (each already owned by another open issue for its other faults, so not re-filed here): codepipeline-s3-source-manual-approval-run-succeeds#task#9 ($.approvedAt got 1794673997.9240928) and codepipeline-s3-source-manual-approval-run-succeeds#task#12 ($.created / $.updated got 1794673992.8899248, $.stageStates[0].actionStates[0].latestExecution.lastStatusChange got 1794673993.2071726).

Model consulted: ListPipelines (output.pipelines: PipelineList; nextToken: NextToken @length 1..2048; paginated inputToken/outputToken nextToken, items pipelines, pageSize maxResults; errors InvalidNextTokenException, ValidationException, both client/400) and shape PipelineSummary (name: PipelineName, version: PipelineVersion, pipelineType: PipelineType, executionMode: ExecutionMode, created: Timestamp, updated: Timestamp).

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay fixture codepipeline-list-pipelines-membership-and-pagination against it.
3. At step task#8 the fixture calls ListPipelines after creating two pipelines with CreatePipeline. Read each PipelineSummary's created and updated.
4. Observe each is an epoch-seconds JSON number with six or seven fractional digits (the emulator's clock precision), where CodePipeline serializes a Timestamp with at most three fractional digits (millisecond resolution), as every recorded CodePipeline timestamp in this corpus does.
5. Confirm the fault is in the shared timestamp encoder rather than in ListPipelines alone: replay codepipeline-s3-source-manual-approval-run-succeeds and read PutApprovalResult approvedAt (step task#9) and GetPipelineState created/updated/lastStatusChange (step task#12) — all carry the same excess precision.
