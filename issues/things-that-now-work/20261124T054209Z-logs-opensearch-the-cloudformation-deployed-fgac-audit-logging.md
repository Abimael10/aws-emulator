Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs__opensearch.e2e-osdocstat-document-request-status-logs-lifecycle
Issue-Key: logs__opensearch.e2e-osdocstat-document-request-status-logs-lifecycle|_poll_until#6|Timeout|last = probe()
Issue-Commit: 97d900f89f587f485ba8a31fa0a1f26bc3bdf335

# logs__opensearch: the CloudFormation-deployed FGAC audit-logging domain never becomes usable, so the document-request-status log lifecycle reaches no verdict (1 cause)

> _Note: this issue was identified against the code base at commit `97d900f89f587f485ba8a31fa0a1f26bc3bdf335`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomain

## Description
One root cause, reached by 1 check (Cause 1 of 1).

Cause 1 of 1: an OpenSearch domain deployed as part of a CloudFormation stack with fine-grained access control and all four LogPublishingOptions (AUDIT_LOGS, ES_APPLICATION_LOGS, SEARCH_SLOW_LOGS, INDEX_SLOW_LOGS) pointing at the stack's own log groups never advances to a usable state, so nothing downstream of the deploy is observable: the signed REST data plane never answers, the security-plugin audit configuration is never readable or patchable, and no audit record of a document GET ever reaches the AUDIT_LOGS group. Failing check id for this cause: logs__opensearch.e2e-osdocstat-document-request-status-logs-lifecycle.

AWS: Smithy (opensearch): CreateDomain accepts AdvancedSecurityOptions (FGAC) and LogPublishingOptions, a map of LogType (AUDIT_LOGS | ES_APPLICATION_LOGS | SEARCH_SLOW_LOGS | INDEX_SLOW_LOGS) to LogPublishingOption {CloudWatchLogsLogGroupArn, Enabled}; DescribeDomain -> DomainStatus reports Processing=false with an Endpoint once the domain is active, and echoes LogPublishingOptions with the group ARN each type publishes to. Recorded behavior: a CREATE_COMPLETE stack publishes the domain name, the domain endpoint and the four log-group names as outputs; a SigV4-signed GET / on that endpoint answers 200; GET /_opendistro/_security/api/audit answers 200 and a PATCH enabling REST and transport auditing with empty disabled-category lists answers 200 and reads back already-enabled on a second run; a PUT of an index answers 200 and a second identical PUT is refused 400 with error.type resource_already_exists_exception; a document PUT answers 201, a GET of that document 200 with found=true and a GET of an unseeded id 404; each of those two GETs then lands exactly one REST audit record (audit_category AUTHENTICATED, audit_request_layer REST, audit_rest_request_method GET) and exactly one transport record (audit_category GRANTED_PRIVILEGES, audit_transport_request_type GetRequest) in the AUDIT_LOGS group, the 200 and the 404 records being identical outside time/routing/request-identity fields and carrying no status-, response- or code-bearing field, while the other three groups never mention either document id.

Emulator: "Check timed out after 60.0s" — the check makes no observable progress; nothing past the deploy phase is reached, so the domain never reports Processing=false with an Endpoint, the signed data plane never answers, the audit config is never read or patched, and the AUDIT_LOGS group receives nothing. The check's own declared deadlines (deploy 2400 s, data plane 300 s, delivery 900 s) all exceed the replay ceiling, so the evidence is the total absence of convergence rather than a specific refusal.

Why: a client that deploys an audit-logging OpenSearch domain by template waits forever for a usable cluster and gets no audit trail: it cannot show that a document GET was authenticated and authorised, cannot rely on the documented property that an audit record does not leak the response status of the request it records, and cannot confirm that only the AUDIT_LOGS group records the request.

Fix: the opensearch CreateDomain/DescribeDomain lifecycle in the opensearch service crate (an accepted domain — including one created by a CloudFormation resource handler — must advance to Processing=false with a served Endpoint), the domain's SigV4-signed REST data plane including the /_opendistro/_security/api/audit configuration endpoint, and the AUDIT_LOGS publishing path that must ship security-plugin audit records to the configured CloudWatch Logs group without writing them to the slow-log or application-log groups.

Traces: logs__opensearch.e2e-osdocstat-document-request-status-logs-lifecycle.

Related open filings that do NOT carry this check id: 20261119T165842Z-logs-opensearch-the-audit-logs-document-get-audit.md (the directly-created FGAC audit domain variant), 20261108T145640Z-logs-opensearch-opensearch-the-audit-log-delivery-scenario-never-converges.md, 20261120T043219Z-logs-opensearch-a-domain-created-for-slow-log.md (slow-log/application-log delivery) and 20261028T032345Z-ec2-elbv2-opensearch-cloudformation-provisioned-aws-opensearchservice-domain-is-refused-by.md (the CFN domain resource being refused outright). The suite did not mark this failure OWNED or ALREADY FILED; a fixer closing the related audit-log filings should verify with the id in trace_ids, which exercises the CloudFormation-deployed, four-log-type variant end to end.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator; service heading logs__opensearch 12/16 (75%), 4 failed — three are OWNED, this id is the one that survives.

[mismatch] logs__opensearch.e2e-osdocstat-document-request-status-logs-lifecycle
  Error: Check timed out after 60.0s

Expected (AWS): the stack deploys to CREATE_COMPLETE with outputs naming the domain, its endpoint and four distinct log groups; DescribeDomain reports Processing=false with that Endpoint and LogPublishingOptions enabling all four types against those groups; a signed GET / answers 200; GET /_opendistro/_security/api/audit answers 200 and the enabling PATCH answers 200 (second run: already-enabled); PUT /<index> answers 200 and a repeat is refused 400 resource_already_exists_exception; the seed document PUT answers 201, GET of it 200 found=true, GET of an unseeded id 404; the AUDIT_LOGS group then holds exactly one AUTHENTICATED REST record and one GRANTED_PRIVILEGES GetRequest transport record per GET, with the 200 and 404 records indistinguishable outside volatile fields and carrying no status/response/code field, and the other three groups mentioning neither document.
Actual (emulator): no assertion past the deploy phase is reached within the replay budget — no active domain endpoint, no data-plane answer, no audit configuration read, no audit record delivered.

Smithy consulted: opensearch CreateDomain input AdvancedSecurityOptions and LogPublishingOptions (map of LogType -> LogPublishingOption {CloudWatchLogsLogGroupArn, Enabled}); DescribeDomain output DomainStatus {Processing, Endpoint, LogPublishingOptions}. Audit-record content and the security-plugin audit endpoint: recorded behavior only.

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; the id in trace_ids reproduces ("Check timed out after 60.0s").
2. CreateStack from the check's osdocstat-domain template: four CloudWatch Logs log groups (audit, application, slow-search, slow-index) with a resource policy granting es.amazonaws.com logs:CreateLogStream + logs:PutLogEvents, and an OpenSearch domain with fine-grained access control and all four LogPublishingOptions enabled against those groups. Wait for CREATE_COMPLETE and read the domain, endpoint and group-name outputs.
3. Call DescribeDomain and poll for Processing=false with an Endpoint. Expected: it becomes active. Actual: it never does, and the check is cut off at the replay ceiling.
4. Then: SigV4-signed GET / on the endpoint -> 200; GET and PATCH /_opendistro/_security/api/audit to enable REST+transport auditing with empty disabled-category lists -> 200 (and already-enabled on a second run); PUT an index (200) and repeat it (400 resource_already_exists_exception); PUT a seed document (201); GET it (200 found=true) and GET an unseeded id (404).
5. Poll the AUDIT_LOGS group with FilterLogEvents. Expected: one AUTHENTICATED REST record and one GRANTED_PRIVILEGES GetRequest record per GET, identical outside volatile fields and with no status/response/code field; the other three groups mention neither id. Actual: unreachable.
6. Re-replay the id in trace_ids to confirm the fix.
