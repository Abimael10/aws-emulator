Issue-Source: trace-sweep-divergence
Issue-Trace: scan-filter-function-operand-count-and-type-rejected
Issue-Step: task#14
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: scan-filter-function-operand-count-and-type-rejected dynamodb-execute-statement-incomplete-condition-rejected dynamodb-execute-statement-insert-missing-sort-key-rejected dynamodb-execute-statement-ion-literal-rejected dynamodb-execute-statement-literal-number-out-of-range-reports-path dynamodb-execute-statement-n-parameter-boundaries-rejected dynamodb-execute-statement-n-parameter-exponent-at-int32-max-reports-underflow dynamodb-execute-statement-n-parameter-exponent-int32-boundary-rejected dynamodb-execute-statement-number-literal-exponent-overflow-rejected dynamodb-execute-statement-reserved-keyword-dotted-path-rejected dynamodb-execute-statement-table-alias-rejected-on-every-statement-kind dynamodb-execute-statement-trailing-or-and-split-exponent-token-rejected dynamodb-execute-statement-where-leading-token-and-delete-from-identifier-rejected dynamodb-partiql-insert-bag-of-items-rejected dynamodb-partiql-set-add-refusal-messages dynamodb-partiql-set-delete-refusal-messages
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|Scan|2xx->4xx|$.Count
Issue-Key: dynamodb|Scan|2xx->4xx|$.Items
Issue-Key: dynamodb|Scan|2xx->4xx|$.ScannedCount
Issue-Key: dynamodb|Scan|2xx->4xx|status
Issue-Key: dynamodb|ExecuteStatement|4xx->4xx|$.message
Issue-Commit: 857f59baf94ca876ddeb888a415f04d0868c32ab

# DynamoDB expression handling diverges: a well-formed Scan FilterExpression is refused, and PartiQL parse failures all report one generic reason

> _Note: this issue was identified against the code base at commit `857f59baf94ca876ddeb888a415f04d0868c32ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Scan

## Description
Two net-new expression-parser defects on dynamodb: the Scan filter parser rejects an expression AWS accepts, and the PartiQL (ExecuteStatement) parser collapses every well-formedness failure onto a single reason string, so the refusal does not say what is wrong with the statement.

THIS BUNDLE CONTAINS 2 CAUSES.

Cause 1 of 2 - a well-formed Scan FilterExpression is refused with 400 where AWS evaluates it and returns matching items.
AWS: Smithy: Scan.FilterExpression -> ConditionExpression (a string; the grammar is not modeled, so the accepted forms are recorded behavior only), Scan output Items -> ItemList with Count/ScannedCount (Integer, default 0). This step is the positive control of a fixture whose other steps pin the refusals for wrong operand COUNT and wrong operand TYPE in a filter function: after those negative cases, the same function call written correctly is accepted and AWS answers 200 with Count 1 / ScannedCount 1 and the one matching item.
Emulator: 400 - the response carries no Items, Count or ScannedCount at all, i.e. the filter parser/validator refuses the expression instead of evaluating it. Hypothesis: the operand count/type check that produces the neighbouring (correct) refusals is too strict and also fires on the valid arity/type combination.
Why: a client issuing a legitimate filtered Scan gets a 400 ValidationException instead of its rows. Unlike a reworded message this changes the outcome of a correct request: the read fails outright, so any paging or filtering built on that function is unusable.
Fix: the FilterExpression parser/validator on the Scan read path (the function-call operand count and operand type check), which must accept the valid form and only refuse the malformed ones the sibling steps cover.
Traces: scan-filter-function-operand-count-and-type-rejected#task#14.

Cause 2 of 2 - every malformed PartiQL statement is refused with the same reason, so the specific parse defect is not reported.
AWS: Smithy: ExecuteStatement.Statement -> PartiQLStatement, required, @length 1..8192; refusals are the service-common ValidationException at HTTP 400. AWS words the message as the fixed text "Statement wasn't well formed, can't be processed: " followed by the parser's specific reason for that statement - here "Missing right-hand side expression of infix operator" for a condition whose infix operator has no right operand. The fixed part is the prefix and the reason vocabulary; nothing in the message is taken from the request, so no per-run value is involved.
Emulator: 400 with the right error class but the reason always rendered as "Unexpected term" - "Statement wasn't well formed, can't be processed: Unexpected term". The cluster spans 40 steps over 25 fixtures covering distinct statement defects (missing right-hand operand, number literal out of range / exponent overflow and underflow, reserved keyword in a dotted path, table alias, trailing OR, a leading token in WHERE, an Ion literal, a missing sort key on INSERT), and all of them collapse to that one reason.
Why: a client that parses or logs the reason to tell the author what to fix - or a test that asserts on it - cannot distinguish an out-of-range number literal from a reserved keyword or a dangling operator; every PartiQL mistake looks identical, and the reason text is a documented part of the error message.
Fix: the PartiQL statement parser's error reporting in the ExecuteStatement handler (and the shared path used by ExecuteTransaction/BatchExecuteStatement): each parse failure must carry its own reason after the fixed "Statement wasn't well formed, can't be processed: " prefix.
Traces: dynamodb-execute-statement-incomplete-condition-rejected#task#8 (cluster covers 40 steps across 25 execute-statement fixtures).

Related open issue: 20261120T225636Z-dynamodb-expression-and-cursor-refusals-report-a-different.md covers other expression/cursor refusal wording, but the replay report leaves owned_by empty for both clusters here, so neither is gated by it; they are filed with their own replay keys.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout. Both clusters are verdict=mismatch with empty stub_path and empty owned_by.

Cause 1 - fixture scan-filter-function-operand-count-and-type-rejected, step task#14 (affected_count 1), operation Scan:
  status: expected 200, got 400
  $.Count: missing (expected 1, got None)
  $.ScannedCount: missing (expected 1, got None)
  $.Items: missing (expected a single-item list, the one seeded item matching the filter, got None)
  The emulator's 400 body carries none of the success members, so the request never reached evaluation.

Cause 2 - fixture dynamodb-execute-statement-incomplete-condition-rejected, step task#8 (affected_count 40 across 25 fixtures), operation ExecuteStatement:
  status 400 -> 400 (error class matches)
  $.message: expected "Statement wasn't well formed, can't be processed: Missing right-hand side expression of infix operator", got "Statement wasn't well formed, can't be processed: Unexpected term"
  Fixed text: the prefix "Statement wasn't well formed, can't be processed: "; variable part: the parser reason for the specific statement (here the missing right-hand side of an infix operator). No request or state value appears in this message.
  Other fixtures in the same cluster: dynamodb-execute-statement-literal-number-out-of-range-reports-path, -number-literal-exponent-overflow-rejected, -n-parameter-exponent-at-int32-max-reports-underflow, -reserved-keyword-dotted-path-rejected, -table-alias-rejected-on-every-statement-kind, -trailing-or-and-split-exponent-token-rejected, -where-leading-token-and-delete-from-identifier-rejected, -ion-literal-rejected, -insert-missing-sort-key-rejected.

Model citations: Scan.FilterExpression -> ConditionExpression, Scan output Items/Count/ScannedCount; ExecuteStatement.Statement -> PartiQLStatement required @length 1..8192, ValidationException at 400 (dynamodb-20120810 Smithy model).

## Steps to Reproduce
1. Boot the emulator built from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1: replay scan-filter-function-operand-count-and-type-rejected in order and stop at step task#14 - the step that issues the correctly written filter function after the malformed variants. Assert HTTP 200 with Count=1, ScannedCount=1 and the matching item in Items, and confirm the preceding malformed steps still refuse.
3. Cause 2: replay dynamodb-execute-statement-incomplete-condition-rejected step task#8 and compare $.message verbatim; then replay the other steps of that cluster (e.g. dynamodb-execute-statement-literal-number-out-of-range-reports-path, dynamodb-execute-statement-reserved-keyword-dotted-path-rejected) and confirm each reports its own reason rather than "Unexpected term".
4. Both causes must pass before closing.
