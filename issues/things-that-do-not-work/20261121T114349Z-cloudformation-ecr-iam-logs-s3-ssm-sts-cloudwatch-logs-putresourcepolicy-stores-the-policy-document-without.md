Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-aws-custom-resource-logs-resource-policy-lifecycle-round-trips
Issue-Key: cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-aws-custom-resource-logs-resource-policy-lifecycle-round-trips|run#17|AssertionError|assert created_doc == want_v1, (
Issue-Commit: 3f1d064d908eb4a469d73223dfef0ca475b0d2e0

# CloudWatch Logs PutResourcePolicy stores the policy document without AWS's single-element canonicalization, so DescribeResourcePolicies reads Action back as a one-item array instead of a bare string

> _Note: this issue was identified against the code base at commit `3f1d064d908eb4a469d73223dfef0ca475b0d2e0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutResourcePolicy

## Description
One net-new root cause, grounded in the failing check cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-aws-custom-resource-logs-resource-policy-lifecycle-round-trips.

Cause 1 of 1: logs:PutResourcePolicy persists the submitted policyDocument byte-for-byte instead of canonicalizing it the way the CloudWatch Logs resource-policy store does, so a statement whose `Action` arrived as a single-element array is read back as a one-element array rather than the bare string AWS returns.

AWS: Smithy (logs) models PutResourcePolicy input `policyDocument` (PolicyDocument, @length min 1 max 51200) and `policyName`, and DescribeResourcePolicies output `resourcePolicies` (list of ResourcePolicy carrying policyName/policyDocument/lastUpdatedTime); the model says nothing about the document's text, so this is recorded behavior only. Recorded: when a policy whose statement has `"Action": ["logs:PutLogEvents"]` (one element) is put, DescribeResourcePolicies serves that statement back with `"Action": "logs:PutLogEvents"` — the scalar form. Every other member of the statement (Sid, Effect, Principal, Resource, Condition) and the document's Version are returned as submitted, and a genuinely multi-element Action (recorded in the same check's update step with ["logs:PutLogEvents", "logs:CreateLogStream"]) stays an array. The rule, stated generally: a single-element IAM policy list member is stored collapsed to its single scalar value; the member names, the ARN in `Resource`, the account id in `Condition`, and the number of actions are all values taken from the request, not fixed text.

Emulator: DescribeResourcePolicies returns the document exactly as submitted — `"Action": ["logs:PutLogEvents"]` — so the stored document differs from AWS's for the identical PutResourcePolicy call. The call itself succeeds (HTTP 200, no error code); only the stored/returned representation diverges.

Why: a client that writes a resource policy and then reads it back cannot compare the two. Infrastructure tooling that round-trips the document to detect drift (CDK AwsCustomResource onUpdate, Terraform-style diffing, any `read-modify-write` of an existing policy) sees a spurious difference on every read and either reports perpetual drift or rewrites the policy on every deploy; code that branches on `isinstance(stmt["Action"], str)` after a read takes the wrong branch.

Fix: the CloudWatch Logs resource-policy state/validation path behind PutResourcePolicy — the component that parses and persists policyDocument must apply AWS's policy canonicalization (collapse a one-element list member such as Action/Resource/Principal to its scalar) before storing, so DescribeResourcePolicies and the PutResourcePolicy response both serve the canonical form.

No open issue covers this: the fixed issue 20261109T143515Z-logs-5-net-new-causes-getlogobject-pointer-refusal.md touched the logs resource-policy limit and PutResourcePolicy *parse-failure* messages, not the stored document's canonical form, and the call here is not refused, so this is not a reopen of it.

Traces: cloudformation__ecr__iam__logs__s3__ssm__sts.deploy-cdk-aws-custom-resource-logs-resource-policy-lifecycle-round-trips (check line 77).

## Evidence
Check deploy-cdk-aws-custom-resource-logs-resource-policy-lifecycle-round-trips, line 77, after a CDK AwsCustomResource onCreate called logs:PutResourcePolicy with a single action and the check re-read it with logs:DescribeResourcePolicies:

AssertionError: the created policy document was {'Version': '2012-10-17', 'Statement': [{'Sid': 'Sim2realAllowEventsWrite', 'Effect': 'Allow', 'Principal': {'Service': 'events.amazonaws.com'}, 'Action': ['logs:PutLogEvents'], 'Resource': 'arn:aws:logs:us-west-2:000000000000:log-group:/sim2real/logpol-49ff971d4762:*', 'Condition': {'StringEquals': {'aws:SourceAccount': '000000000000'}}}]}, expected {'Version': '2012-10-17', 'Statement': [{'Sid': 'Sim2realAllowEventsWrite', 'Effect': 'Allow', 'Principal': {'Service': 'events.amazonaws.com'}, 'Action': 'logs:PutLogEvents', 'Resource': 'arn:aws:logs:us-west-2:000000000000:log-group:/sim2real/logpol-49ff971d4762:*', 'Condition': {'StringEquals': {'aws:SourceAccount': '000000000000'}}}]}

The check states the rule in its own comment at line 1804: "AWS stores a single-element Action array as a bare string, not a one-item list." The only difference between got and expected is `Action`: `['logs:PutLogEvents']` vs `'logs:PutLogEvents'`. The log-group ARN, the account id in the Condition, the Sid and the log-group name in the recording are values generated per run and are not part of the requirement; the requirement is the scalar-vs-array shape of a one-element list member. Status and error code do not diverge: PutResourcePolicy and DescribeResourcePolicies both answered 200.

## Steps to Reproduce
1. Create a log group, e.g. /sim2real/<name>.
2. Call logs:PutResourcePolicy with policyName=<name> and policyDocument = {"Version":"2012-10-17","Statement":[{"Sid":"AllowEventsWrite","Effect":"Allow","Principal":{"Service":"events.amazonaws.com"},"Action":["logs:PutLogEvents"],"Resource":"arn:aws:logs:<region>:<account>:log-group:/sim2real/<name>:*","Condition":{"StringEquals":{"aws:SourceAccount":"<account>"}}}]} — note Action is a one-element array.
3. Call logs:DescribeResourcePolicies and json-parse the policyDocument of the entry whose policyName is <name>.
Observed: the statement's Action is the one-element array ["logs:PutLogEvents"].
Expected: the statement's Action is the bare string "logs:PutLogEvents"; every other member is unchanged.
4. Repeat with Action=["logs:PutLogEvents","logs:CreateLogStream"]: the read-back must keep a two-element array (this leg already matches, confirming only the one-element collapse is missing).
