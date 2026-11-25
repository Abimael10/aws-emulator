Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__iam__lambda__logs__opensearch__s3.cfn-search-indexing-pipeline-alarm-config-workflow
Issue-Key: cloudwatch__iam__lambda__logs__opensearch__s3.cfn-search-indexing-pipeline-alarm-config-workflow|_poll_until#30|Timeout|return last
Issue-Commit: 643d7fc40de53dee6cfee0ebba73d0a58d0d5505

# cloudwatch__iam__lambda__logs__opensearch__s3: the CloudFormation search-indexing-pipeline alarm/config workflow never completes — the check exhausts its 60 s budget with no verdict

> _Note: this issue was identified against the code base at commit `643d7fc40de53dee6cfee0ebba73d0a58d0d5505`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: the multi-stack search-indexing-pipeline deployment-order scenario does not complete against the emulator, so none of its alarm/configuration assertions are reached.

AWS:      recorded behavior only — the recording completes this scenario inside the suite's 60 s per-check budget. The graded behavior is CloudFormation's deployment-order contract across three templates the check ships (search-domain.yaml, search-pipeline.yaml and the deliberately broken search-pipeline-readonly-role.yaml): each CreateStack settles to a terminal stack status, the OpenSearch domain and the indexing Lambda plus its log group and S3 bucket come up in dependency order, the CloudWatch alarm declared over the pipeline is created and reports a state, and the read-only-role variant is refused / rolls back rather than deploying a working pipeline.

Emulator: the check reaches no verdict at all — reported as "Check timed out after 60.0s". The stack creations and the SigV4-signed domain calls the check makes do not finish inside the budget the recorded run fits in, so neither the happy-path ordering nor the broken-role rejection is observable.

Why:      A client relying on AWS's behavior gets an unbounded wait where AWS answers in seconds: a CreateStack that never settles blocks every deploy tool driving it, and because the scenario never reaches its assertions the emulator's deployment-ordering and alarm wiring for this estate are entirely unverified — a stack that silently deployed the read-only-role pipeline as if it were valid would be indistinguishable from this result.

Fix:      The CloudFormation stack lifecycle for this estate (CreateStack/DescribeStacks reaching a terminal status promptly for the AWS::OpenSearchService::Domain, AWS::Lambda::Function, AWS::Logs::LogGroup, AWS::S3::Bucket and AWS::CloudWatch::Alarm resources these templates declare), plus whichever resource provisioning in the sequence does not return. The timeout carries no stack frame, so the stalling call must be identified by replaying the listed check and timing each step.

Traces:   cloudwatch__iam__lambda__logs__opensearch__s3.cfn-search-indexing-pipeline-alarm-config-workflow.

## Evidence
Failing check cloudwatch__iam__lambda__logs__opensearch__s3.cfn-search-indexing-pipeline-alarm-config-workflow, [mismatch]:

  Error: Check timed out after 60.0s

The check (GROUP=deployment-order, SERVICES=[cloudwatch, iam, lambda, logs, opensearch, s3]) deploys three templates from its deployments/ directory — search-domain.yaml, search-pipeline.yaml and search-pipeline-readonly-role.yaml — and signs its own requests with botocore SigV4Auth/AWSRequest.

Expected (AWS): the scenario completes within the 60 s budget; each CreateStack settles to a terminal status in dependency order, the alarm over the indexing pipeline exists and reports a state, and the read-only-role variant is rejected/rolled back.
Actual (emulator): no response/verdict at all inside 60 s — the check is killed by its budget, so none of the deployment-order or alarm assertions are produced.

## Steps to Reproduce
1. CreateStack from search-domain.yaml (the OpenSearch domain) and wait for a terminal status.
2. CreateStack from search-pipeline.yaml (the indexing Lambda, its execution role, log group, S3 bucket and the CloudWatch alarm over the pipeline) and wait for a terminal status; then read the alarm back and drive the pipeline with SigV4-signed domain calls.
3. CreateStack from search-pipeline-readonly-role.yaml and expect it to be refused / roll back.
   Expected: every step settles to a terminal status within the check's 60 s budget and the assertions above are gradable.
   Observed: the sequence does not complete within the 60 s check budget, so nothing is graded.
