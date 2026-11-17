Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda__logs__resourcegroupstaggingapi.tag-resources-nonexistent-log-group-arn-in-mixed-type-batch-rejected iam__lambda__logs__resourcegroupstaggingapi.shared-tag-across-log-group-and-function-matches-two-arns
Issue-Key: iam__lambda__logs__resourcegroupstaggingapi.tag-resources-nonexistent-log-group-arn-in-mixed-type-batch-rejected|run#20|AssertionError|assert set(failed) == {ghost_arn}, (
Issue-Key: iam__lambda__logs__resourcegroupstaggingapi.shared-tag-across-log-group-and-function-matches-two-arns|_poll_until#23|Timeout|time.sleep(interval_s)
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# iam__lambda__logs__resourcegroupstaggingapi: 2 causes — TagResources reports no FailedResourcesMap entry for an absent log group in a mixed batch, and the shared-tag discovery scenario never converges

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResources

## Description
Two net-new root causes in the Resource Groups Tagging API over logs/lambda resources.

Cause 1 of 2: a never-created log group ARN in a mixed-type batch is reported as tagged.
AWS: Smithy resourcegroupstaggingapi TagResources takes ResourceARNList (1..20) and returns FailedResourcesMap — a per-ARN map of the resources it could NOT tag; an ARN naming a log group that does not exist is reported there (with its status code and error code), while the live Lambda function ARN in the same call succeeds and is absent from the map. The operation itself answers 200.
Emulator: FailedResourcesMap comes back EMPTY — the absent log group is reported as successfully tagged, and the per-resource partial-failure contract is lost.
Why: FailedResourcesMap is the only signal of partial failure in a batch; a client is told it tagged a resource that does not exist, so bulk-tagging jobs report success over typos and deleted resources.
Related open issue for the same operation with a different resource type (it does not carry this replay key): 20261106T224449Z-resourcegroupstaggingapi-tagresources-omits-the-per-resource-failedresourcesmap-entry-for.md.
Fix: the TagResources handler's per-ARN resolution — a resource that cannot be resolved (or tagged) must be added to FailedResourcesMap instead of being counted as a success.
Failing check: iam__lambda__logs__resourcegroupstaggingapi.tag-resources-nonexistent-log-group-arn-in-mixed-type-batch-rejected

Cause 2 of 2: the shared-tag-across-two-services discovery scenario never reaches a verdict.
AWS: recorded behavior only — one tag key/value applied to a log group and to a Lambda function makes GetResources with that TagFilter return BOTH ARNs.
Emulator: the check makes no progress and is killed by the harness at its 60 s budget, so neither ARN is ever matched.
Why: cross-service tag discovery is the whole point of the API; a consumer polling GetResources for a tag it just applied waits forever.
Fix: the tag plane shared by the logs/lambda resources and the GetResources TagFilter matching over it.
Failing check: iam__lambda__logs__resourcegroupstaggingapi.shared-tag-across-log-group-and-function-matches-two-arns

## Evidence
Cause 1: [mismatch] iam__lambda__logs__resourcegroupstaggingapi.tag-resources-nonexistent-log-group-arn-in-mixed-type-batch-rejected — AssertionError: FailedResourcesMap named [], expected exactly the never-created log group 'arn:aws:logs:<region>:<account>:log-group:/<name>' — the live function ARN must not fail and the absent log group must not pass.

Cause 2: [mismatch] iam__lambda__logs__resourcegroupstaggingapi.shared-tag-across-log-group-and-function-matches-two-arns — 'Error: Check timed out after 60.0s'; expected GetResources with the shared TagFilter to return both the log-group ARN and the function ARN.

## Steps to Reproduce
1. Create a Lambda function; call TagResources with a batch of two ARNs — the live function and a log group that was never created — and inspect FailedResourcesMap.
2. Create a log group and a function, apply the same tag key/value to both via TagResources, then poll GetResources with that TagFilter for both ARNs.
