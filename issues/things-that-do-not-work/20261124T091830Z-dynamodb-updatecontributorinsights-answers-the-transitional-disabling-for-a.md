Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodb-ttl-contributor-insights-and-replica-autoscaling-config
Issue-Step: task#13
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodb-ttl-contributor-insights-and-replica-autoscaling-config
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|UpdateContributorInsights|2xx->2xx|$.ContributorInsightsStatus
Issue-Commit: 0ed36ac69b61f04777ef4cd85714b039c0593841

# dynamodb UpdateContributorInsights answers the transitional DISABLING for a DISABLE with nothing left to tear down, instead of the settled DISABLED

> _Note: this issue was identified against the code base at commit `0ed36ac69b61f04777ef4cd85714b039c0593841`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateContributorInsights

## Description
Cause 1 of 1: UpdateContributorInsights returns the transitional ContributorInsightsStatus for every accepted DISABLE, including one whose target has no teardown left to perform, so the terminal status is never served at that point.

AWS: Smithy (services/service-com-amazonaws-dynamodb-dynamodb-20120810 model): UpdateContributorInsights takes a required TableName (TableArn, @length 1..1024), an optional IndexName (@length 3..255, pattern ^[a-zA-Z0-9_.-]+$), a required ContributorInsightsAction enum (ENABLE, DISABLE) and an optional ContributorInsightsMode; the output carries TableName, IndexName, ContributorInsightsStatus and ContributorInsightsMode, and ContributorInsightsStatus is the enum ENABLING, ENABLED, DISABLING, DISABLED, FAILED. The model binds only InternalServerError and ResourceNotFoundException, so the status a given DISABLE answers is recorded behavior only. The recording for this step is HTTP 200 with ContributorInsightsStatus 'DISABLED': AWS reports the TRANSITIONAL member only while an actual teardown of the insight rules is still outstanding, and reports the SETTLED terminal member 'DISABLED' for a DISABLE at a point where contributor insights are already (or already becoming) off and there is nothing further to tear down. ContributorInsightsStatus is the only diverging field; the status string is the whole requirement here — there is no message, and TableName/IndexName in the reply are request values, not part of it. Hypothesis: the discriminator in this fixture is that the DISABLE at this step is a repeat/no-op DISABLE against a target whose insights are not in an ENABLED-and-active state, which AWS answers with the already-settled status.

Emulator: the same call answers HTTP 200 with ContributorInsightsStatus 'DISABLING' — the transitional member is emitted unconditionally on accept of a DISABLE, with no dependence on whether a teardown is actually pending, so 'DISABLED' is never the status this reply carries.

Why: ContributorInsightsStatus is the only signal a client has for whether the DynamoDB-minted CloudWatch insight rules behind a table/index are still being removed. A client that treats DISABLING as "teardown in flight" and polls DescribeContributorInsights until it clears will loop on a request AWS already reported as settled, and a state machine that branches on the terminal DISABLED at this point takes the wrong branch. Because the divergence is a single enum value inside a 200, nothing surfaces it as an error.

Fix: the UpdateContributorInsights handler and the table/index contributor-insights state it reads — the status in the reply must be derived from the stored contributor-insights state after applying the action (transitional only while a teardown remains outstanding, settled DISABLED otherwise) rather than being a constant written by the DISABLE branch.

Related open issues, neither of which carries this replay key (this step is not owned by either): 20261115T010844Z-cloudwatch-dynamodb-updatecontributorinsights-with-contributorinsightsaction-disable-answers-contributorinsightsstatus.md asserts the opposite direction for its own behavioral check (a DISABLE after a settled ENABLE must answer DISABLING, which the emulator then answered DISABLED), and cause 1 of 20261118T105911Z-dynamodb-9-net-new-causes-a-disable-accepted.md is about DISABLE being ACCEPTED mid-ENABLING rather than about the status a DISABLE reports. Together with this divergence they say the status must be state-dependent, not a constant in either direction.

Traces: fixture dynamodb-ttl-contributor-insights-and-replica-autoscaling-config step task#13 (the only member of this cluster).

## Evidence
Wire replay of trace set simparity-wire-aws-v1, fixture dynamodb-ttl-contributor-insights-and-replica-autoscaling-config, step task#13, operation UpdateContributorInsights, verdict mismatch, divergence_key service=dynamodb|UpdateContributorInsights|status=200->200|keys=$.ContributorInsightsStatus, affected_count 1.

Expected status 200, actual status 200 — the status line matches; the body diverges on one field:
  $.ContributorInsightsStatus: value_mismatch (expected 'DISABLED', got 'DISABLING')

Expected: HTTP 200 with ContributorInsightsStatus == 'DISABLED'.
Actual:   HTTP 200 with ContributorInsightsStatus == 'DISABLING'.

Both values are members of the modeled ContributorInsightsStatus enum (ENABLING, ENABLED, DISABLING, DISABLED, FAILED), so this is a state-derivation divergence, not a serialization or enum-spelling one. The table name, index name and request id in the exchange are per-run values and are not part of the requirement; the diverging field is ContributorInsightsStatus alone.

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from it.
2. Replay the wire corpus (replay_wire_corpus) and look at fixture dynamodb-ttl-contributor-insights-and-replica-autoscaling-config.
3. Steps task#1..task#12 of that fixture pass; step task#13 issues UpdateContributorInsights with ContributorInsightsAction=DISABLE against the fixture's table/index after the preceding TTL and contributor-insights steps have already driven contributor insights out of the active ENABLED state.
4. Compare the reply's ContributorInsightsStatus: the recording has 200 / 'DISABLED', the emulator answers 200 / 'DISABLING'.
5. Re-replay dynamodb-ttl-contributor-insights-and-replica-autoscaling-config#task#13 to confirm a fix; the surrounding steps of the same fixture (including the earlier contributor-insights steps that legitimately report a transitional status) must stay passing, so the fix must make the status state-dependent rather than flip the constant.
