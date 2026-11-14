Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agent-ingestion-job-lifecycle
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agent-ingestion-job-lifecycle
Issue-Shape-Id: com.amazonaws.bedrockagent#AmazonBedrockAgentBuildTimeLambda
Issue-Key: bedrock-agent|ListIngestionJobs|2xx->2xx|$.ingestionJobSummaries[*]
Issue-Commit: cf4f60c5fc13ba8044ce457fda63f711f871a11a

# bedrock-agent ListIngestionJobs enumerates an ingestion job AWS never created on a non-ingestible data source

> _Note: this issue was identified against the code base at commit `cf4f60c5fc13ba8044ce457fda63f711f871a11a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListIngestionJobs

## Description
1 cause.

Cause 1 of 1: after a StartIngestionJob that AWS refuses, ListIngestionJobs for that knowledge base / data source pair must enumerate nothing, but the emulator reports a persisted job (fixture bedrock-agent-ingestion-job-lifecycle, step task#7).

AWS: Smithy: ListIngestionJobs (POST /knowledgebases/{knowledgeBaseId}/datasources/{dataSourceId}/ingestionjobs/, success 200) returns the @required list ingestionJobSummaries, which is the set of ingestion jobs that were actually started for that data source; StartIngestionJob (PUT on the same collection, success 202) models ConflictException (errorKind client, httpError 400-class 409) and is the only operation that creates such a record. In the recording, the preceding StartIngestionJob on a data source whose status forbids ingestion is refused 409 ConflictException with the message "You cannot start an ingestion job on a data source with status FAILED." (fixed text "You cannot start an ingestion job on a data source with status " plus the data source's current DataSourceStatus from state and a trailing period), so NO job record exists and the subsequent ListIngestionJobs answers 200 with an EMPTY ingestionJobSummaries list. The empty listing is the requirement here; the ids, timestamps and statistics in the recording are per-run values and are not part of it.

Emulator: answers 200 but ingestionJobSummaries carries one entry — the job the un-gated StartIngestionJob persisted (status STARTING, all-zero statistics, the knowledgeBaseId/dataSourceId of the request and a generated ingestionJobId). Replay reports: $.ingestionJobSummaries[0]: extra (expected None, got {'knowledgeBaseId': ..., 'dataSourceId': ..., 'ingestionJobId': ..., 'status': 'STARTING', ...}).

Why: a client that lists a data source's ingestion jobs to decide whether a sync is in flight (or to report sync history) sees a job that AWS never accepted. On AWS the refusal is the whole answer — the list stays empty and the client repairs the data source first; against the emulator it polls a phantom STARTING job, so the failure is reported as a stuck or empty sync rather than as the ConflictException AWS raised.

Fix: ListIngestionJobs itself faithfully enumerates what the store holds (services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/src/ops/list_ingestion_jobs.rs calls state.page_of_ingestion_jobs over the live knowledge base / data source), so the defect is upstream: StartIngestionJob's handler pre-condition and the data-source state machine in the service state must refuse a start on a non-ingestible data source and persist no IngestionJobRecord, which leaves this listing empty. Related open issues — neither carries this step's replay key, so this divergence is not re-replayed by either: 20261029T172025Z-bedrock-agent-accepts-requests-aws-refuses-unsupported-data.md (Cause 2 covers the StartIngestionJob 409 gate itself at task#4) and 20261108T203853Z-bedrock-agent-listingestionjobs-stub.md (stale: ListIngestionJobs is implemented and answers 200, not 501). Closing those does not close this listing divergence unless the start is actually gated and nothing is persisted.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture bedrock-agent-ingestion-job-lifecycle, step task#7 (ListIngestionJobs), status expected 200 / actual 200:
  $.ingestionJobSummaries[0]: extra (expected None, got {'knowledgeBaseId': 'BMCukmmjzM', 'dataSourceId': 'rIiy3yKB4b', 'ingestionJobId': 'EWw462Y41F', 'description': 'Probe-triggered sync', 'status': 'STARTING', 'startedAt': <ts>, 'updatedAt': <ts>, 'statistics': {all zero}})

Expected: 200 with ingestionJobSummaries == [] — the recorded StartIngestionJob at task#4 of the same fixture was refused 409 ConflictException ('You cannot start an ingestion job on a data source with status FAILED.'), so no job exists to list.
Actual: 200 with one summary, the job the emulator's un-gated StartIngestionJob (202 at task#4) persisted.

Smithy (services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/smithy-model/com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda.json): ListIngestionJobs output ingestionJobSummaries @required (IngestionJobSummaries), http POST .../ingestionjobs/ code 200; StartIngestionJob errors include ConflictException (client, 409), success code 202.
Code: services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/src/ops/list_ingestion_jobs.rs lines 243-255 page the stored records unfiltered by any such pre-condition.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture bedrock-agent-ingestion-job-lifecycle.
2. At task#4 StartIngestionJob is issued against a data source whose status forbids ingestion; the emulator answers 202 and persists a job (AWS answers 409 ConflictException).
3. At task#7 call ListIngestionJobs for the same knowledgeBaseId/dataSourceId: the emulator answers 200 with one ingestionJobSummaries entry, where AWS answers 200 with an empty list.
