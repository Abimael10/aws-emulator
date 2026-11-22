Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodb-scan-filter-expression-redundant-parens-around-operands-and-function-args
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodb-scan-filter-expression-redundant-parens-around-operands-and-function-args scan-filter-attribute-type-operand-sweep-and-begins-with-order-rejected scan-filter-function-operand-count-and-type-rejected
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|Scan|2xx->4xx|$.Count
Issue-Key: dynamodb|Scan|2xx->4xx|$.ScannedCount
Issue-Key: dynamodb|Scan|2xx->4xx|status
Issue-Key: dynamodb|Scan|2xx->4xx|$.Items
Issue-Commit: 4528b778465addf20721967dea781c4ddd900890

# dynamodb Scan refuses valid FilterExpression forms with HTTP 400 instead of returning the filtered page

> _Note: this issue was identified against the code base at commit `4528b778465addf20721967dea781c4ddd900890`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Scan

## Description
Two net-new causes, both the same fault: the Scan handler's FilterExpression parser refuses grammatically valid DynamoDB filter expressions, so a request AWS answers 200 with a filtered result page is answered with an HTTP 400 refusal body and no Items/Count/ScannedCount at all. These are the POSITIVE-control steps of two negative-test fixtures: the surrounding invalid forms in the same fixtures are refused as expected, but the valid form that must be accepted is refused too, which means the parser is over-rejecting rather than mis-wording a refusal.

Both causes are reported by the replay with an empty owner list. The open dynamodb expression issues do NOT cover them: 20261111T221143Z-dynamodb-condition-key-and-filter-expression-defects-are.md and 20261117T172826Z-dynamodb-expression-evaluation-10-net-new-causes-transaction.md are about expressions that are wrongly ACCEPTED or mis-evaluated, and 20261120T225636Z-dynamodb-expression-and-cursor-refusals-report-a-different.md is about the wording of refusals that are correctly issued (it owns step task#9 of the very same fixture as Cause 1, while task#8 below is unowned). Neither covers a valid expression being refused outright.

Cause 1 of 2: redundant parentheses around operands and function arguments in a Scan FilterExpression are refused.
  AWS: Smithy: ScanInput.FilterExpression documentation - "A string that contains conditions that DynamoDB applies after the Scan operation, but before the data is returned to you. Items that do not satisfy the FilterExpression criteria are not returned." The model constrains FilterExpression only as a string; it states no grammar, so the accepted grammar is recorded behavior only. The recording shows DynamoDB parsing a filter expression whose operands and function arguments are wrapped in redundant parentheses, evaluating it, and answering HTTP 200 with the matching page - in the recorded run Count and ScannedCount are both 2 (those counts are the recording's data, not a requirement; the requirement is a 200 carrying Count/Items/ScannedCount computed from the scanned table).
  Emulator: answers HTTP 400. The response body is an error envelope, so $.Count and $.ScannedCount are absent entirely (replay detail: "status: expected 200, got 400; $.Count: missing (expected 2, got None); $.ScannedCount: missing (expected 2, got None)"). The expected response carries no error members, so the replay does not capture the emulator's error code or message text - whatever it is, no refusal at all is correct here.
  Why: a client that parenthesises filter operands for readability - which SDK query builders and hand-written expressions both do - gets its Scan rejected outright. There is no partial result and no retry that helps: the caller sees a 400 ValidationException-shaped failure for a request real DynamoDB serves, so a scan-and-filter page loop terminates with an error instead of data.
  Fix: the FilterExpression parser used by Scan's validation path in the dynamodb service crate. The condition-expression grammar must treat a parenthesised operand and a parenthesised function argument as the operand/argument itself (parentheses are grouping, allowed anywhere an operand is allowed) rather than as a syntax error, and the request must then fall through to filter evaluation and the normal 200 page serializer.
  Traces: dynamodb-scan-filter-expression-redundant-parens-around-operands-and-function-args#task#8.

Cause 2 of 2: well-typed attribute_type / begins_with filter functions are refused, including when the filter legitimately matches nothing.
  AWS: Smithy: ScanInput.FilterExpression (same documentation as above); the model names no function set, so the operand rules for attribute_type and begins_with are recorded behavior only. The recording shows DynamoDB accepting the well-typed forms in these sweeps - attribute_type with a type operand, and begins_with with its operands in the documented order - evaluating the filter against the scanned items and answering HTTP 200. In the recorded run the filter selected nothing, so the page is Count 0, Items [] and ScannedCount 1; the zero/empty values are that recording's data, while the requirement is the 200 page itself: ScannedCount counts the items the Scan read and Count/Items report only those satisfying the filter, so a filter that matches nothing is an empty page, never an error.
  Emulator: answers HTTP 400 (replay detail: "status: expected 200, got 400; $.Count: missing (expected 0, got None); $.Items: missing (expected [], got None); $.ScannedCount: missing (expected 1, got None)"). As in Cause 1 the expected body has no error members, so the refusal's code and message are not captured by the replay; the defect is the refusal itself.
  Why: a client cannot distinguish "no item matched my filter" from "my filter was rejected", because the emulator turns the empty-result case into a failure. Any code that scans with attribute_type or begins_with to test for absence - existence probes, type guards, prefix sweeps - fails instead of reporting zero matches, and the per-operand type sweep an SDK's own test matrix performs cannot pass.
  Fix: same component as Cause 1 - the Scan FilterExpression validator/evaluator in the dynamodb service crate. Accept the documented operand shapes for attribute_type and begins_with instead of rejecting them, and let an empty match set produce a 200 with Count 0, Items [] and ScannedCount equal to the number of items read, rather than a 400.
  Traces: scan-filter-attribute-type-operand-sweep-and-begins-with-order-rejected#task#10, scan-filter-attribute-type-operand-sweep-and-begins-with-order-rejected#task#11, scan-filter-function-operand-count-and-type-rejected#task#14.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 8158 steps, 7808 passed, 350 mismatched, 49 divergence clusters; these are the only two clusters with an empty owned_by and an empty stub_path).

Cause 1 - cluster divergence_key "service=dynamodb|Scan|status=200->400|keys=$.Count,$.ScannedCount,status", fixture dynamodb-scan-filter-expression-redundant-parens-around-operands-and-function-args, step task#8, affected_count 1:
  expected_status 200, actual_status 400
  detail: status: expected 200, got 400
          $.Count: missing (expected 2, got None)
          $.ScannedCount: missing (expected 2, got None)
  Expected: a 200 Scan page carrying Count/Items/ScannedCount for the filter.
  Actual: a 400 refusal, so none of those members are present.

Cause 2 - cluster divergence_key "service=dynamodb|Scan|status=200->400|keys=$.Count,$.Items,$.ScannedCount,status", fixture scan-filter-attribute-type-operand-sweep-and-begins-with-order-rejected, step task#10, affected_count 3 (task#10, task#11, and scan-filter-function-operand-count-and-type-rejected#task#14):
  expected_status 200, actual_status 400
  detail: status: expected 200, got 400
          $.Count: missing (expected 0, got None)
          $.Items: missing (expected [], got None)
          $.ScannedCount: missing (expected 1, got None)
  Expected: a 200 empty page - Count 0, Items [], ScannedCount 1 (one item read, none matched).
  Actual: a 400 refusal with no page members.

In both clusters the neighbouring steps of the same fixtures - the genuinely invalid expression forms - are refused as the recording requires (several are reported in other clusters only for refusal WORDING, e.g. the same fixture's task#9), which isolates the defect to the valid forms being over-rejected rather than to error text.

Model evidence: Smithy ScanInput.FilterExpression is an unconstrained string documented as "conditions that DynamoDB applies after the Scan operation, but before the data is returned to you. Items that do not satisfy the FilterExpression criteria are not returned." - nothing in the model authorises refusing these expressions, and the recordings show AWS serving them 200.

## Steps to Reproduce
1. git clone the repository and build/boot the emulator from this checkout (the replay harness does this; /__admin/health answers 200 and the data listener accepts).
2. Replay the wire corpus simparity-wire-aws-v1 (replay_wire_corpus).
3. Cause 1: replay fixture dynamodb-scan-filter-expression-redundant-parens-around-operands-and-function-args and look at step task#8 - a dynamodb Scan whose FilterExpression wraps operands and function arguments in redundant parentheses. The recorded response is HTTP 200 with a Count/ScannedCount page; the emulator answers HTTP 400 and the page members are absent.
4. Cause 2: replay fixture scan-filter-attribute-type-operand-sweep-and-begins-with-order-rejected steps task#10 and task#11, and fixture scan-filter-function-operand-count-and-type-rejected step task#14 - dynamodb Scans whose FilterExpression uses attribute_type / begins_with with well-typed operands in the documented order. The recorded response is HTTP 200 with an empty page (Count 0, Items [], ScannedCount equal to the items read); the emulator answers HTTP 400.
5. Confirm the fix by re-replaying all four steps above: each must come back HTTP 200 with Count, Items and ScannedCount present, and the invalid-form steps in the same fixtures must still be refused.
