Issue-Source: trace-sweep-divergence
Issue-Trace: osis-catalog-list-pipeline-blueprints
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: osis-catalog-list-pipeline-blueprints
Issue-Shape-Id: com.amazonaws.osis#AmazonOpenSearchIngestionService
Issue-Key: osis|ListPipelineBlueprints|2xx->2xx|$.Blueprints[*]
Issue-Commit: 091b3f731686c97cd7bc62068e813c07acee4582

# osis ListPipelineBlueprints serves a 10-entry blueprint catalog with emulator-authored metadata instead of the published catalog

> _Note: this issue was identified against the code base at commit `091b3f731686c97cd7bc62068e813c07acee4582`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListPipelineBlueprints

## Description
Cause 1 of 1: ListPipelineBlueprints returns a blueprint catalog that is both far too small and, for the names it does publish, carries metadata the emulator invented rather than the metadata the service publishes. Fixture osis-catalog-list-pipeline-blueprints, step task#1.

AWS: ListPipelineBlueprints (Smithy: com.amazonaws.osis#ListPipelineBlueprints, POST /2022-01-01/osis/listPipelineBlueprints, success 200, output ListPipelineBlueprintsResponse.Blueprints: PipelineBlueprintsSummaryList of PipelineBlueprintSummary{BlueprintName, DisplayName, DisplayDescription, Service, UseCase} — doc: "Retrieves a list of all available blueprints for Data Prepper"). The model constrains the SHAPE only; the catalog CONTENT is service-published data, so for it this is recorded behavior only. The recording shows the service publishing 49 summaries (response indices $.Blueprints[0] .. $.Blueprints[48]), covering blueprint families the emulator does not carry at all — e.g. BlueprintName "AWS-AuroraMySQLPipeline" (DisplayName "Aurora MySQL", Service "Amazon Aurora", UseCase "ZeroETL", DisplayDescription "Synchronizes records from an Aurora MySQL database to an OpenSearch domain or collection.") and "AWS-AuroraPostgreSQLPipeline" (DisplayName "Aurora PostgreSQL", Service "Amazon Aurora", UseCase "ZeroETL") — and it shows the published wording and classification for the names that do overlap: "AWS-ApacheLogEnrichment" has DisplayDescription "Extracts logs by matching the value in the 'log' key against the grok common Apache log pattern. Enriches the logs with GeoIP data.", and "AWS-ApacheLogSampling" has DisplayName "Apache logs sampling", DisplayDescription "Extracts data from Apache logs and routes them to various indexes." and UseCase "Log Analytics". The whole response is 200 with no error code, so there is no message template at stake here — the requirement is the catalog's membership and each summary's five published member values.

Emulator: the handler reports whatever is in the seeded catalog, and that catalog holds only TEN entries (AWS-AlbAccessLogS3Pipeline, AWS-ApacheLogEnrichment, AWS-ApacheLogPipeline, AWS-ApacheLogSampling, AWS-CloudTrailLogPipeline, AWS-VPCFlowLogPipeline, AWS-TraceAnalyticsPipeline, AWS-MetricIngestionPipeline, AWS-DynamoDBChangeDataCapturePipeline, AWS-S3SelectLogPipeline). Status 200 and the response shape are correct; the body is not. Replay reports 47 diverging positions ($.Blueprints[1], $.Blueprints[3] and $.Blueprints[4] through $.Blueprints[48]): everything from index 4 up is absent because the list simply ends, and at the two lower indices the summary for a name that IS present carries different text — AWS-ApacheLogEnrichment is served with DisplayDescription "Adds the geographical location of the client address to each Apache log event.", and AWS-ApacheLogSampling with DisplayName "Apache log sampling", DisplayDescription "Keeps a bounded share of the Apache log events a pipeline receives and indexes those." and UseCase "Sampling" instead of "Log Analytics". The catalog source comments concede this ("The description is this emulator's wording ...", "The rest is this emulator's wording ...").

Why: the blueprint catalog is a discovery surface. A client that lists blueprints to let a user choose one, filters by Service or UseCase (e.g. picking the ZeroETL Aurora blueprints, or every "Log Analytics" blueprint), or lists and then calls GetPipelineBlueprint on the chosen BlueprintName, sees roughly a fifth of the real catalog: 39 published blueprints are undiscoverable, the follow-up GetPipelineBlueprint for any of them cannot succeed, and the two overlapping entries are mis-classified (AWS-ApacheLogSampling is filed under a UseCase that does not exist upstream) and carry descriptions that are not what the service renders in a picker. Anything that snapshots or diffs the catalog, or asserts on its size, is wrong against the emulator in a way no error signals.

Fix: the service's seeded blueprint catalog — the data, not the handler. The handler itself is a faithful report over state (it maps the catalog in blueprint-name order and writes nothing), so the catalog records that back it must be completed to the full published set of 49 blueprints and each entry's BlueprintName/DisplayName/DisplayDescription/Service/UseCase taken from the service's published values rather than paraphrased locally. GetPipelineBlueprint reads the same records, so completing the catalog must keep the two operations agreeing (each new entry also needs its Data Prepper configuration body).

No open osis issue covers this: the existing ones concern the ValidatePipeline stub, NotImplemented on PutResourcePolicy/TagResource/CreatePipelineEndpoint and the ingest data plane, CreatePipeline accepting an unassumable sts_role_arn, and CloudFormation's AWS::OSIS::Pipeline type — none touches the blueprint catalog.

Traces: osis-catalog-list-pipeline-blueprints#task#1 (the only member of this cluster).

## Evidence
Replay verdict: mismatch, expected_status 200, actual_status 200, service osis, operation ListPipelineBlueprints, fixture osis-catalog-list-pipeline-blueprints step task#1.

Diverging response keys (47): $.Blueprints[1], $.Blueprints[3], $.Blueprints[4] ... $.Blueprints[48].

$.Blueprints[1] expected {'BlueprintName': 'AWS-ApacheLogEnrichment', 'DisplayName': 'GeoIP enrichment for Apache logs', 'DisplayDescription': "Extracts logs by matching the value in the 'log' key against the grok common Apache log pattern. Enriches the logs with GeoIP data.", 'Service': 'Apache Logs', 'UseCase': 'GeoIP'}; got the same name with DisplayDescription 'Adds the geographical location of the client address to each Apache log event.'

$.Blueprints[3] expected {'BlueprintName': 'AWS-ApacheLogSampling', 'DisplayName': 'Apache logs sampling', 'DisplayDescription': 'Extracts data from Apache logs and routes them to various indexes.', 'Service': 'Apache Logs', 'UseCase': 'Log Analytics'}; got DisplayName 'Apache log sampling', DisplayDescription 'Keeps a bounded share of the Apache log events a pipeline receives and indexes those.', UseCase 'Sampling'.

$.Blueprints[4] expected {'BlueprintName': 'AWS-AuroraMySQLPipeline', 'DisplayName': 'Aurora MySQL', 'DisplayDescription': 'Synchronizes records from an Aurora MySQL database to an OpenSearch domain or collection.', 'Service': 'Amazon Aurora', 'UseCase': 'ZeroETL'}; got {'BlueprintName': 'AWS-CloudTrailLogPipeline', ...} — the emulator's shorter list has slipped, and every index past the end of its 10 entries is missing entirely ($.Blueprints[5] expected 'AWS-AuroraPostgreSQLPipeline', and so on through $.Blueprints[48]).

Emulator catalog size: 10 entries seeded in the osis state's BLUEPRINT_CATALOG (services/service-com-amazonaws-osis-amazonopensearchingestionservice/src/state.rs, entries at lines 413, 448, 480, 509, 539, 572, 620, 669, 695, 728); handler at services/service-com-amazonaws-osis-amazonopensearchingestionservice/src/ops/list_pipeline_blueprints.rs:157-165 maps that map straight to the response. Recorded catalog size: 49.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture osis-catalog-list-pipeline-blueprints, step task#1.
2. Equivalently, by hand: POST /2022-01-01/osis/listPipelineBlueprints with an empty JSON body (osis ListPipelineBlueprints takes no members) to the osis endpoint.
3. Count the returned Blueprints: the emulator answers 200 with 10 summaries; the recorded service answers 200 with 49.
4. Compare the summary for BlueprintName 'AWS-ApacheLogSampling': the emulator reports DisplayName 'Apache log sampling' / UseCase 'Sampling'; the recording reports 'Apache logs sampling' / 'Log Analytics'. Same for AWS-ApacheLogEnrichment's DisplayDescription.
5. Pick any published blueprint the emulator omits (e.g. AWS-AuroraMySQLPipeline) and call GET /2022-01-01/osis/getPipelineBlueprint/AWS-AuroraMySQLPipeline: it cannot be fetched because it is not in the catalog.
