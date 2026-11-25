Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__iam__lambda__logs__opensearch__s3.cfn-search-indexing-pipeline-workflow cloudwatch__iam__lambda__logs__opensearch__s3.cfn-search-indexing-pipeline-alarm-config-workflow
Issue-Key: cloudwatch__iam__lambda__logs__opensearch__s3.cfn-search-indexing-pipeline-workflow|_poll_until#28|AssertionError|raise AssertionError(f"{description} not met within {timeout_s}s. Last result: {last!r}")
Issue-Key: cloudwatch__iam__lambda__logs__opensearch__s3.cfn-search-indexing-pipeline-alarm-config-workflow|_poll_until#28|Timeout|time.sleep(interval_s)
Issue-Commit: 5636e5ddac188567344946aa5c15e82054f1c023

# cloudwatch__iam__lambda__logs__opensearch__s3: 2 net-new causes — a signed OpenSearch PUT from a role without es:ESHttpPut is not refused 403, and the alarm-config pipeline workflow never completes

> _Note: this issue was identified against the code base at commit `5636e5ddac188567344946aa5c15e82054f1c023`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteStatement

## Description
Two net-new root causes in this service group. Both checks under this heading failed and neither is marked ALREADY FILED or OWNED in the replay report.

Cause 1 of 2: the OpenSearch domain data plane does not evaluate the caller's IAM identity policy, so an index write signed by a role that holds es:ESHttpGet but NOT es:ESHttpPut is not refused with 403.
Failing check id: cloudwatch__iam__lambda__logs__opensearch__s3.cfn-search-indexing-pipeline-workflow.

AWS: recorded behavior only — the opensearch model covers the control plane (domain creation, AccessPolicies) and models no data-plane authorization contract, so the requirement comes from the recording and from the deployment the check ships. The domain's AccessPolicies delegate the whole decision to IAM by naming the account root for es:ESHttp* on arn:<partition>:es:<region>:<account>:domain/<domain>/*; the indexer role's identity policy therefore decides. A SigV4-signed PUT /<index>/_doc/<id> from a role whose identity policy grants only es:ESHttpGet is answered HTTP 403 by the domain with a body naming the denied action (the string 'es:ESHttpPut' appears in the refusal; the user/role ARN, request id and index name in that message are per-run values), the document is NOT written, and a later signed GET /<index>/_doc/<id> answers 404.

Emulator: the refusal never happens. The indexer Lambda logs one JSON line per record, tagged 'document.rejected' when the PUT status is >= 300 and 'document.indexed' otherwise; the check polls the function's log group for a 'document.rejected' line naming the uploaded key and gets nothing for the whole 420 s barrier: "AssertionError: document.rejected line for docs/sim2real-xsvc-denydoc-92ee5d25c345.json not met within 420.0s. Last result: []". The healthy legs of the same check (wiring, search, logs, metrics, alarm, update, drift) had already passed with the SAME template differing only in that one action list, so the S3 -> Lambda -> OpenSearch path does run; what is missing is the 403. Hypothesis: the data plane admits every signed request that reaches it, so the PUT answers 2xx and the handler logs 'document.indexed' instead, leaving the 403 status, the es:ESHttpPut naming and the GET -> 404 legs all ungraded.

Cause 2 of 2: the alarm-configuration variant of the same CloudFormation pipeline workflow never completes — the replay detail is the whole of "Error: Check timed out after 60.0s", with no AWS error code and no leg evidence, where the check's own metric/alarm barriers are budgeted in minutes.
Failing check id: cloudwatch__iam__lambda__logs__opensearch__s3.cfn-search-indexing-pipeline-alarm-config-workflow.
AWS: recorded behavior only — the check deploys the same search-domain.yaml + search-pipeline.yaml pair and reads back the AWS::Logs::MetricFilter and AWS::CloudWatch::Alarm configuration the templates declare (namespace, metric name, Statistic Sum, Period 60, EvaluationPeriods 1, Threshold 0, ComparisonOperator GreaterThanThreshold, TreatMissingData missing, ActionsEnabled false).
Emulator: killed at the 60 s harness cap with no assertion reached, so no status or code can be quoted; the stack/domain deployment did not get far enough to read anything back.

Why (both): Cause 1 is an authorization bypass at a data plane whose only gate is IAM — a client that scopes an indexer to read-only, or that proves least privilege by showing a write is refused, is told the write succeeded; a role that must not be able to mutate an index can mutate it, and the operator's negative test passes for the wrong reason. Cause 2 leaves the whole declared metric-filter/alarm configuration of a search-indexing stack unverifiable, so a client cannot tell whether its indexing alarm was created as declared.

Fix: Cause 1 — the OpenSearch domain data-plane request path (the component that authenticates/authorizes a SigV4-signed es request before applying it): it must evaluate the signing principal's IAM identity policy for the es:ESHttp<Method> action against the domain/index resource and answer 403 with a body naming the denied action when the action is not granted, leaving the index unmodified. Cause 2 — the CloudFormation provisioning path for this stack set (domain + log group + metric filter + alarm + role + function + bucket notification), which must converge quickly enough for the configuration to be read back.

Traces: cloudwatch__iam__lambda__logs__opensearch__s3.cfn-search-indexing-pipeline-workflow (cause 1), cloudwatch__iam__lambda__logs__opensearch__s3.cfn-search-indexing-pipeline-alarm-config-workflow (cause 2).

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against the booted emulator: 1945/2850 passed (68.2%); 725 mismatched, 180 errored; bootstrap_signal false. Per-service cloudwatch__iam__lambda__logs__opensearch__s3 0/2 (0%), 2 failed — neither is marked ALREADY FILED or OWNED.

Cause 1 — [mismatch] cloudwatch__iam__lambda__logs__opensearch__s3.cfn-search-indexing-pipeline-workflow
  File "check_...cfn_search_indexing_pipeline_workflow.py", line 770, in _assert_readonly_role_indexes_nothing
    rejected = _poll_until(...)
  File "...", line 249, in _poll_until
    raise AssertionError(f"{description} not met within {timeout_s}s. Last result: {last!r}")
  AssertionError: document.rejected line for docs/sim2real-xsvc-denydoc-92ee5d25c345.json not met within 420.0s. Last result: []

The failing leg deploys search-pipeline-readonly-role.yaml, which differs from the healthy search-pipeline.yaml by exactly one action list in IndexerRole (es:ESHttpPut + es:ESHttpPost + es:ESHttpGet -> es:ESHttpGet only) and points at the SAME domain, whose AccessPolicies allow es:ESHttp* to the account root. The handler logs {'event': 'document.indexed'|'document.rejected', 'index', 'key', 'status', 'detail', 'requestId'} with 'document.rejected' chosen when the signed PUT status >= 300.
Expected (AWS): after the .json upload, the log group holds a 'document.rejected' line for that key with status == 403 and a detail containing 'es:ESHttpPut'; no 'document.indexed' line exists for it; a signed GET /<index>/_doc/<url-encoded key> from the runner answers 404.
Actual (emulator): no 'document.rejected' line ever appears (empty list for 420 s), so the write was not refused; the 403/es:ESHttpPut/404 legs are ungraded.

Cause 2 — [mismatch] cloudwatch__iam__lambda__logs__opensearch__s3.cfn-search-indexing-pipeline-alarm-config-workflow
  Error: Check timed out after 60.0s
That timeout is the entire diagnostic; the check reached no assertion, so there is no diverging status or error code to quote.

Smithy consulted: the opensearch model exposes the domain control plane only (no data-plane authorization operation), so the es:ESHttp* enforcement requirement is recorded behavior plus the shipped templates.

Bootstrap gate: get_bootstrap_status() reports pending == [] — cloudwatch, iam, lambda, logs, opensearch and s3 are all stood up in this checkout, so these are fidelity findings, not bootstrap gaps.

## Steps to Reproduce
Boot the emulator and point cloudformation, opensearch, iam, lambda, logs and s3 clients at its data listener.
Cause 1:
1. Deploy search-domain.yaml: an AWS::OpenSearchService::Domain with no VpcOptions and AccessPolicies allowing es:ESHttp* to arn:<partition>:iam::<account>:root on arn:<partition>:es:<region>:<account>:domain/<domain>/*. Record DomainEndpoint and Arn.
2. Deploy search-pipeline.yaml against that domain (role granted es:ESHttpPut/ESHttpPost/ESHttpGet) and confirm an uploaded .json document is indexed and queryable — the healthy control.
3. Deploy search-pipeline-readonly-role.yaml against the SAME domain: identical except the indexer role grants only es:ESHttpGet.
4. THE GRADED STEP: PutObject a docs/<unique>.json document into that stack's bucket, so the notification invokes the indexer, which SigV4-signs PUT /<index>/_doc/<url-encoded key>?refresh=true as its execution role.
   EXPECTED (AWS): the domain answers 403 with a body naming es:ESHttpPut; the handler logs {'event': 'document.rejected', 'status': 403, 'detail': '...es:ESHttpPut...'} and no 'document.indexed' line; a signed GET /<index>/_doc/<id> from the runner answers 404.
   OBSERVED: no 'document.rejected' line in /aws/lambda/<function> for 420 s — the write was not refused.
Cause 2:
5. Run the alarm-configuration variant of the same deployment (domain + pipeline with the AWS::Logs::MetricFilter and AWS::CloudWatch::Alarm) and read the alarm back with DescribeAlarms.
   EXPECTED (AWS): the alarm exists with Statistic Sum, Period 60, EvaluationPeriods 1, Threshold 0, ComparisonOperator GreaterThanThreshold, TreatMissingData missing, ActionsEnabled false, and the metric filter pattern '{ $.event = "document.indexed" }'.
   OBSERVED: the check is killed at the 60 s harness cap before any read.
6. Re-replay both ids: cloudwatch__iam__lambda__logs__opensearch__s3.cfn-search-indexing-pipeline-workflow and cloudwatch__iam__lambda__logs__opensearch__s3.cfn-search-indexing-pipeline-alarm-config-workflow.
