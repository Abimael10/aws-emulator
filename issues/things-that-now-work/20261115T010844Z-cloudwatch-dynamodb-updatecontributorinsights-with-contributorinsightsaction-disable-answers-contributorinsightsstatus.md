Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__dynamodb.obs-dynamodb-2-contributor-insights-hot-key-closed-loop
Issue-Key: cloudwatch__dynamodb.obs-dynamodb-2-contributor-insights-hot-key-closed-loop|run#43|AssertionError|assert off["ContributorInsightsStatus"] == "DISABLING", f"DISABLE returned {off!r}"
Issue-Commit: 3cd41981ee722cd642660e763820306da1bf304f

# cloudwatch__dynamodb: UpdateContributorInsights with ContributorInsightsAction=DISABLE answers ContributorInsightsStatus DISABLED immediately instead of the transitional DISABLING

> _Note: this issue was identified against the code base at commit `3cd41981ee722cd642660e763820306da1bf304f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateContributorInsights

## Description
Cause 1 of 1: the DISABLE action on Contributor Insights skips its transitional state, so the response reports the terminal status straight away.

AWS: UpdateContributorInsights is asynchronous. A call with ContributorInsightsAction=DISABLE is accepted 200 and the response's ContributorInsightsStatus is the transitional `DISABLING`; the status only becomes `DISABLED` once the insight rules have actually been torn down, observable through a later DescribeContributorInsights. Smithy: UpdateContributorInsights.ContributorInsightsAction targets the ContributorInsightsAction enum (ENABLE, DISABLE) and is required; the output's ContributorInsightsStatus targets the ContributorInsightsStatus enum whose values are ENABLING, ENABLED, DISABLING, DISABLED, FAILED — the transitional members exist precisely because the operation does not complete synchronously. Status code 200 in both cases.

Emulator: the DISABLE call answers 200 with ContributorInsightsStatus `DISABLED` (alongside TableName and ContributorInsightsMode), i.e. the DISABLING member is never served.

Why: a client that polls for the transition — the documented way to know the DynamoDB-minted `DynamoDBContributorInsights-<PKC|PKT>-<table>-<epoch>` CloudWatch insight rules have been deleted and that GetInsightRuleReport on the old name will now be refused — sees a terminal status before the teardown has happened, and any state machine that expects to observe DISABLING never does. It also makes the enum's transitional members unreachable, so code branching on them is dead.

The open issue 20261102T120041Z-cloudwatch-dynamodb-dynamodb-publishes-no-aws-dynamodb-cloudwatch.md covers other checks under this service heading and does not carry this check's replay key.

Fix: the UpdateContributorInsights handler and the table's contributor-insights state — return DISABLING on accept and settle to DISABLED only after the insight rules are removed (mirroring ENABLE's ENABLING→ENABLED).

Traces: cloudwatch__dynamodb.obs-dynamodb-2-contributor-insights-hot-key-closed-loop.

## Evidence
Expected: UpdateContributorInsights(TableName=<table>, ContributorInsightsAction='DISABLE') -> 200 with ContributorInsightsStatus == 'DISABLING', settling to 'DISABLED' on a later DescribeContributorInsights.

Actual: 200 with `{'TableName': '<table>', 'ContributorInsightsStatus': 'DISABLED', 'ContributorInsightsMode': 'ACCESSED_AND_THROTTLED_KEYS'}` — assertion `off["ContributorInsightsStatus"] == "DISABLING"` fails with `DISABLE returned {... 'ContributorInsightsStatus': 'DISABLED' ...}`. The table name and request id are per-run values; the diverging field is ContributorInsightsStatus.

## Steps to Reproduce
1. Create a hash-only PAY_PER_REQUEST table and call UpdateContributorInsights with ContributorInsightsAction='ENABLE' (mode ACCESSED_AND_THROTTLED_KEYS); wait for status ENABLED.
2. Call UpdateContributorInsights with ContributorInsightsAction='DISABLE'.
3. Read ContributorInsightsStatus from the response: expected 'DISABLING', actual 'DISABLED'.
Replay check cloudwatch__dynamodb.obs-dynamodb-2-contributor-insights-hot-key-closed-loop to confirm the fix.
