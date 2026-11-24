Issue-Source: trace-sweep-divergence
Issue-Trace: logs-integration-lifecycle
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: logs-integration-lifecycle logs-put-integration-malformed-config-error
Issue-Shape-Id: com.amazonaws.cloudwatchlogs#Logs_20140328
Issue-Key: logs|GetIntegration|2xx->2xx|$.integrationDetails.openSearchIntegrationDetails.application.applicationEndpoint
Issue-Key: logs|GetIntegration|2xx->2xx|$.integrationDetails.openSearchIntegrationDetails.collection.collectionEndpoint
Issue-Key: logs|PutIntegration|4xx->4xx|$.message
Issue-Commit: 53ca3190ddccd906197b0d4d5e8474d7add13c5d

# logs OpenSearch integration reads omit endpoints and PutIntegration validation message drops constraints

> _Note: this issue was identified against the code base at commit `53ca3190ddccd906197b0d4d5e8474d7add13c5d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetIntegration

## Description
Two net-new divergences on the CloudWatch Logs OpenSearch integration surface (both clusters carry empty owned_by / stub_path; no open issue holds their replay keys — the nearest, 20261120T145635Z-logs-3-net-new-causes-createexporttask-refuses-an.md, covers AssociateSourceToS3TableIntegration and does not carry either step).

Cause 1 of 2: GetIntegration omits the OpenSearch application and collection endpoints. AWS: Smithy GetIntegration(integrationName) returns integrationName, integrationType, integrationStatus and integrationDetails; for an OPENSEARCH integration, integrationDetails.openSearchIntegrationDetails carries application: OpenSearchApplication and collection: OpenSearchCollection, whose endpoint members are modelled as OpenSearchApplicationEndpoint / OpenSearchCollectionEndpoint — string, @length min 1 max 1024, @pattern ^https://[\.\-_/#:A-Za-z0-9]+\.com$ — i.e. the integration setup creates an application and a collection and publishes the URL of each. The requirement is that both members are PRESENT and well-formed per that pattern for an integration the emulator itself reports as created; the particular host is environment-specific and is not the requirement. Emulator: after PutIntegration creates the integration, GetIntegration returns openSearchIntegrationDetails with application.applicationEndpoint absent (null) and collection.collectionEndpoint absent (null). Why: the vended-logs dashboard URL and the collection's query endpoint are the only way a client reaches the resources the integration created; with both omitted a caller that follows the documented flow has nothing to open or query, and a client that dereferences them gets a null. Fix: the PutIntegration state that must record the created application/collection resources, and GetIntegration's integrationDetails serializer that must emit their endpoints. Traces: logs-integration-lifecycle#task#4.

Cause 2 of 2: PutIntegration's validation message reports only one of the three constraints AWS reports for the failing member. AWS: the refusal is the standard AWS constraint-violation body (status 400, same error code the emulator already returns). Its fixed text is '<N> validation errors detected: Value at '<member path>' failed to satisfy constraint: <constraint phrase>[; Value at ...]', and for a dashboardViewerPrincipals member (target Arn) AWS lists ALL violated/declared member constraints in a bracketed list, in its canonical order: 'Member must satisfy constraint: [Member must have length less than or equal to 2048, Member must have length greater than or equal to 20, Member must satisfy regular expression pattern: [\w#+=/:,.@-]*\*?]'. The variable parts are the error count, the member paths (resourceConfig.openSearchResourceConfig.dashboardViewerPrincipals, resourceConfig.openSearchResourceConfig.dataSourceRoleArn) and the limits; the bracketed constraint list is fixed text for that member. The Smithy JSON models these as plain Arn with no constraint traits, so the constraint list itself is recorded behavior only. Emulator: it emits 'Member must satisfy constraint: [Member must have length greater than or equal to 20]' — the max-length and the regular-expression clauses are dropped, while the dataSourceRoleArn clause in the same message is correct. Why: callers and test suites match on AWS's constraint text to decide which input rule they violated; a truncated bracketed list hides that the value also had to match the ARN pattern, so the caller retries with a long-but-malformed principal and is refused again. Fix: the PutIntegration input-validation layer's constraint-message builder (emit every declared constraint of the list member, in AWS's order, not just the first one that failed). Traces: logs-put-integration-malformed-config-error#task#1.

## Evidence
Cause 1 — fixture logs-integration-lifecycle step task#4, GetIntegration, status 200 -> 200: '$.integrationDetails.openSearchIntegrationDetails.application.applicationEndpoint: missing (expected an https://... endpoint, got None)' and '$.integrationDetails.openSearchIntegrationDetails.collection.collectionEndpoint: missing (expected <shaped-not-asserted>, got None)'. The recorded applicationEndpoint value is a per-environment host and is not the requirement; the divergence is that both members are absent.

Cause 2 — fixture logs-put-integration-malformed-config-error step task#1, PutIntegration, status 400 -> 400 (error code matches, only $.message diverges):
Expected: "2 validation errors detected: Value at 'resourceConfig.openSearchResourceConfig.dashboardViewerPrincipals' failed to satisfy constraint: Member must satisfy constraint: [Member must have length less than or equal to 2048, Member must have length greater than or equal to 20, Member must satisfy regular expression pattern: [\\w#+=/:,.@-]*\\*?]; Value at 'resourceConfig.openSearchResourceConfig.dataSourceRoleArn' failed to satisfy constraint: Member must have length greater than or equal to 20".
Actual: "2 validation errors detected: Value at 'resourceConfig.openSearchResourceConfig.dashboardViewerPrincipals' failed to satisfy constraint: Member must satisfy constraint: [Member must have length greater than or equal to 20]; Value at 'resourceConfig.openSearchResourceConfig.dataSourceRoleArn' failed to satisfy constraint: Member must have length greater than or equal to 20".
Fixed text: the '<N> validation errors detected: ...failed to satisfy constraint: ...' frame and the bracketed constraint list for the list member. Variable: the count, the member paths and the numeric limits.

## Steps to Reproduce
1. Boot the emulator and replay the wire corpus (replay_wire_corpus).
2. Cause 1: replay fixture logs-integration-lifecycle — PutIntegration creates an OPENSEARCH integration (resourceConfig.openSearchResourceConfig with dataSourceRoleArn, dashboardViewerPrincipals, retentionDays), then step task#4 calls GetIntegration(integrationName=<that name>). Expected integrationDetails.openSearchIntegrationDetails.application.applicationEndpoint and .collection.collectionEndpoint present and matching ^https://[\.\-_/#:A-Za-z0-9]+\.com$; observed both absent.
3. Cause 2: replay fixture logs-put-integration-malformed-config-error step task#1 — PutIntegration(integrationType=OPENSEARCH) with a dashboardViewerPrincipals member shorter than 20 characters and a dataSourceRoleArn shorter than 20 characters. Expected the 400 message to list all three constraints for the dashboardViewerPrincipals member (max length 2048, min length 20, pattern [\w#+=/:,.@-]*\*?); observed only the min-length clause.
