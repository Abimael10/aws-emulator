Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs__opensearch.osdocstat-audit-log-records-document-get-without-response-status
Issue-Key: logs__opensearch.osdocstat-audit-log-records-document-get-without-response-status|_poll_until#33|Timeout|time.sleep(interval_s)
Issue-Commit: 1a6df7b32e4c8350e561c708bcd111a16150a036

# logs__opensearch: the AUDIT_LOGS document-GET audit-record scenario still reaches no verdict — an accepted FGAC domain never becomes usable, so no audit record is ever delivered

> _Note: this issue was identified against the code base at commit `1a6df7b32e4c8350e561c708bcd111a16150a036`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomain

## Description
One root cause, reached by 1 check (Cause 1 of 1).

NOTE ON OWNERSHIP: the open issue 20261108T145640Z-logs-opensearch-opensearch-the-audit-log-delivery-scenario-never-converges.md names this same check id in its Issue-Trace-Ids, but the suite did NOT mark this failure OWNED — the replay key it recorded (`...|_poll_until#13|Timeout|time.sleep(interval_s)`, observed at a 180 s ceiling) no longer matches how the failure surfaces now (a bare "Check timed out after 60.0s"). It is re-filed here so the live failure carries a re-replayable key; a fixer closing that issue should treat this as the same defect and verify with the id in trace_ids.

Cause 1 of 1: a CreateDomain that is ACCEPTED with fine-grained access control and AUDIT_LOGS LogPublishingOptions never advances to a state in which the scenario can proceed, so no security-plugin audit record is ever delivered to the granted CloudWatch Logs group and nothing about audit content is observable.

AWS: Smithy (opensearch): CreateDomain accepts LogPublishingOptions as a map keyed by LogType (AUDIT_LOGS here) of LogPublishingOption {CloudWatchLogsLogGroupArn, Enabled}, plus AdvancedSecurityOptions for FGAC; DescribeDomain -> DomainStatus {Processing, Endpoint, ...} reports Processing=false with an Endpoint once the domain is active. Recorded behavior: the accepted domain becomes active and serves its SigV4-signed data plane (GET / answers 200); PATCH /_opendistro/_security/api/audit answers 200 and reads back with auditing enabled and empty disabled_rest_categories; a document GET that hits (200) and one that misses (404) each produce an AUTHENTICATED REST record and a GRANTED_PRIVILEGES transport record in the configured log group, and those records are identical outside time/routing/request-identity fields — in particular they carry no response status, response body or code field, which is the claim under test.

Emulator: "Check timed out after 60.0s" — nothing past the accepted CreateDomain is reached: the domain never reports Processing=false with an Endpoint, the signed data plane never answers, the audit config is never PATCHed and no audit event reaches the log group. The check's own declared deadlines (domain active 1500 s, data plane 300 s, delivery 900 s) all exceed the replay ceiling, so the evidence is the absence of any convergence rather than a specific refusal.

Why: a client that creates an audit-logging domain waits forever for it to become usable, and a security reader gets no audit trail at all — it cannot prove that a document GET was authenticated and authorised, nor rely on the documented property that an audit record does not leak the response status of the request it records.

Fix: the opensearch CreateDomain/DescribeDomain lifecycle in the opensearch service crate (an accepted domain must advance to Processing=false with a served Endpoint), the domain's signed REST data plane, and the AUDIT_LOGS publishing path that must ship security-plugin audit records (CreateLogStream + PutLogEvents) to the configured CloudWatch Logs group.

Traces: logs__opensearch.osdocstat-audit-log-records-document-get-without-response-status.

Related open filings that do not cover this cause: 20261111T000041Z-logs-opensearch-a-domain-with-log-publishing-enabled.md (slow-log/application-log delivery), 20261102T090835Z-logs-opensearch-opensearch-createdomain-never-validates-logpublishingoptions-the-legacy-es.md and 20261114T090212Z-logs-opensearch-2-net-new-causes-a-cfn.md (validation and CFN-granted delivery).

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator; service heading logs__opensearch 9/16 (56%), 7 failed — six are OWNED/ALREADY FILED, this id is the one that survives.

[mismatch] logs__opensearch.osdocstat-audit-log-records-document-get-without-response-status
  Error: Check timed out after 60.0s

Expected (AWS): CreateDomain (OpenSearch_2.11, one t3.small.search node, gp3 10 GiB) with FGAC and LogPublishingOptions={'AUDIT_LOGS': {'CloudWatchLogsLogGroupArn': <granted group arn>, 'Enabled': true}} is accepted; DescribeDomain then reports Processing=false with an Endpoint; a SigV4-signed GET / on that endpoint answers 200; PATCH /_opendistro/_security/api/audit answers 200 and reads back enabled=true with disabled_rest_categories == []; a GET of a seeded document (200) and of a missing one (404) each land an AUTHENTICATED REST record and a GRANTED_PRIVILEGES transport record in the log group, with no status/response/code field.
Actual (emulator): no assertion past the accepted CreateDomain is reached within the replay budget — no Endpoint/Processing=false, no data-plane answer, no audit record.

Smithy consulted: opensearch CreateDomain input LogPublishingOptions (map of LogType -> LogPublishingOption {CloudWatchLogsLogGroupArn, Enabled}) and AdvancedSecurityOptions; DescribeDomain output DomainStatus {Processing, Endpoint}. Audit-record content: recorded behavior only.

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; the id in trace_ids reproduces ("Check timed out after 60.0s").
2. CreateLogGroup and PutResourcePolicy granting es.amazonaws.com logs:CreateLogStream + logs:PutLogEvents on <group-arn>:*.
3. CreateDomain (OpenSearch_2.11, t3.small.search x1, gp3 10 GiB) with FGAC enabled and LogPublishingOptions {'AUDIT_LOGS': {'CloudWatchLogsLogGroupArn': <group arn>, 'Enabled': true}}; it is accepted.
4. Poll DescribeDomain for Processing=false with an Endpoint. Expected: it becomes active. Actual: it never does, and the check is cut off at the replay ceiling.
5. Then: signed GET / on the endpoint -> 200; PATCH /_opendistro/_security/api/audit to enable REST+transport auditing with empty disabled categories -> 200; GET a seeded document (200) and a missing one (404); poll the log group. Expected: an AUTHENTICATED REST record and a GRANTED_PRIVILEGES transport record per GET, carrying no response status. Actual: unreachable.
6. Re-replay the id in trace_ids to confirm the fix.
