Issue-Source: trace-sweep-divergence
Issue-Trace: budgets-cost-budget-lifecycle
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: budgets-cost-budget-lifecycle budgets-planned-budget-limits
Issue-Shape-Id: com.amazonaws.budgets#AWSBudgetServiceGateway
Issue-Key: budgets|DescribeBudgetPerformanceHistory|2xx->2xx|$.BudgetPerformanceHistory.BudgetedAndActualAmountsList[*].BudgetedAmount.Amount
Issue-Commit: 1023678a2f4d4b5afbe27b0b7b8a4a94baea46e4

# budgets: DescribeBudgetPerformanceHistory re-formats Spend.Amount through a float, returning 250.0 where AWS echoes the stored numeric string 250

> _Note: this issue was identified against the code base at commit `1023678a2f4d4b5afbe27b0b7b8a4a94baea46e4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeBudgetPerformanceHistory

## Description
One net-new root cause in the budgets emulator's budget-performance-history serialization path.

Cause 1 of 1: DescribeBudgetPerformanceHistory renders Spend.Amount from a parsed floating-point number instead of echoing the numeric string that was stored with the budget limit, so an integral amount comes back with a synthesized '.0' fraction.

AWS: Smithy (services/service-com-amazonaws-budgets-awsbudgetservicegateway/smithy-model/com-amazonaws-budgets-awsbudgetservicegateway.json) models com.amazonaws.budgets#Spend with required members Amount -> NumericValue and Unit -> UnitValue. NumericValue is a STRING shape (@length min 1, pattern ^([0-9]*\.)?[0-9]+$), not a double — 'A string that represents a numeric value' — and the Spend docs give the canonical form as an Amount of '3' for 3 USD, i.e. no fabricated fractional part. DescribeBudgetPerformanceHistory returns BudgetPerformanceHistory.BudgetedAndActualAmountsList, a list of BudgetedAndActualAmounts whose BudgetedAmount/ActualAmount are Spend. Because the member is a string, the budgeted amount a client reads back is the SAME lexical token it supplied on CreateBudget/UpdateBudget (Budget.BudgetLimit.Amount / PlannedBudgetLimits[*].Amount); the recordings confirm this — a budget created with a limit amount of '250' is reported in its performance history as BudgetedAmount.Amount '250'. The rule, not the recorded digits, is the requirement: whatever numeric string was stored for the period's budgeted (or actual) amount is reproduced character-for-character, with no normalization, zero-padding or fraction added or removed. HTTP 200 on both sides; no error is involved.

Emulator: the same step answers HTTP 200 with the correct shape but $.BudgetPerformanceHistory.BudgetedAndActualAmountsList[0].BudgetedAmount.Amount = '250.0' where the recording has '250' (replay verdict 'mismatch', detail "value_mismatch (expected '250', got '250.0')"). The '.0' is not present in any request or stored value, which shows the amount is being round-tripped through a numeric type (f64/Decimal) and re-rendered with a default float formatter rather than carried as the opaque NumericValue string. The same divergence reproduces on a second, independent fixture that exercises PlannedBudgetLimits, so it is the shared Spend serialization and not one fixture's data.

Why: NumericValue being a string makes the amount safe to compare and to key on. A client that asserts the budgeted amount it set ('250' == BudgetedAmount.Amount), that uses the field as a dictionary key or cache key, that feeds it to an exact-decimal parser, or that renders it to a user, sees a value that differs from what it wrote and from what AWS returns; equality checks fail, idempotent reconciliation loops decide the budget drifted and re-issue UpdateBudget, and arbitrary-precision consumers now see a value that has already passed through binary floating point (so a non-integral or high-precision amount can also lose or gain digits, not merely acquire '.0').

Fix: the budgets DescribeBudgetPerformanceHistory response serializer and the state that backs it — Spend.Amount must be stored and emitted as the verbatim NumericValue string from the originating CreateBudget/UpdateBudget (and the computed actual amount must be formatted by an explicit decimal rule, never a float's Display), instead of being parsed into a number on ingest and formatted on output. The same Spend serialization is shared by DescribeBudget(s), BudgetLimit, PlannedBudgetLimits, CalculatedSpend and notification thresholds, so fixing it at the Spend level covers the clustered steps.

Traces: budgets-cost-budget-lifecycle#task#7 (representative) and budgets-planned-budget-limits#task#4.

No open issue owns this: the replay reports no owner and no stub for the cluster. The three open budgets issues are unrelated in cause — the budget-action initial-status issue (20261108T071057Z-budgets-a-freshly-created-budget-action-is-born.md), the DescribeSubscribersForNotification not-found issue (20261108T235222Z-budgets-describesubscribersfornotification-raises-notfoundexception-for-a-deleted-budget.md) and the CloudFormation AWS::Budgets::Budget provisioning issue (20261109T115703Z-budgets-cloudformation-3-net-new-causes-aws-budgets.md) — none of them touch Spend.Amount formatting.

## Evidence
Wire corpus simparity-wire-aws-v1 replayed against the emulator built from this checkout: 89 steps total, 87 passed, 2 mismatched, 0 errored, 0 blocked (score 97.8%); emulator_up=true, bootstrap_signal=false. Both mismatches are this one divergence cluster.

[mismatch] service=budgets, operation=DescribeBudgetPerformanceHistory, expected_status 200 -> actual_status 200
  divergence_key: service=budgets|DescribeBudgetPerformanceHistory|status=200->200|keys=$.BudgetPerformanceHistory.BudgetedAndActualAmountsList[0].BudgetedAmount.Amount
  detail: $.BudgetPerformanceHistory.BudgetedAndActualAmountsList[0].BudgetedAmount.Amount: value_mismatch (expected '250', got '250.0')
  affected (2): budgets-cost-budget-lifecycle#task#7, budgets-planned-budget-limits#task#4

Expected (AWS, per recording): BudgetedAmount.Amount is the numeric string stored for the budget limit, byte-for-byte — '250' for a limit created as '250'.
Actual (emulator): '250.0'.

Every other member of the response — status, BudgetName, BudgetType, TimeUnit, CostTypes, the TimePeriod bounds and the Unit — matched, so the shape and the history record are right and only the Amount lexical form diverges. The digits '250' are this fixture's data, not the requirement; the requirement is that the emitted string equals the stored one. Nothing per-run (request id, clock, host/port, generated budget name) is involved in the comparison.

## Steps to Reproduce
Against the emulator's budgets endpoint, with AccountId from sts:GetCallerIdentity:

1. CreateBudget(AccountId, Budget={BudgetName=B, BudgetLimit={Amount='250', Unit='USD'}, TimeUnit='MONTHLY', BudgetType='COST'}) — note the limit amount is supplied as the integral string '250', with no fractional part.
2. DescribeBudgetPerformanceHistory(AccountId, BudgetName=B) -> read $.BudgetPerformanceHistory.BudgetedAndActualAmountsList[0].BudgetedAmount.Amount. The emulator returns '250.0'; AWS returns '250' (the exact string from step 1).
3. Repeat with a limit whose amount carries explicit precision (e.g. Amount='250.00') and with UpdateBudget setting PlannedBudgetLimits entries — each period's BudgetedAmount.Amount must come back exactly as written ('250.00' stays '250.00'), confirming no float round-trip.
4. Teardown: DeleteBudget(AccountId, BudgetName=B).

Or replay the wire fixtures directly: budgets-cost-budget-lifecycle step task#7 and budgets-planned-budget-limits step task#4.
