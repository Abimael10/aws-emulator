Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodb-condition-both-operands-parenthesized
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodb-condition-both-operands-parenthesized dynamodb-putitem-condition-operand-parentheses dynamodb-putitem-item-attr-member-type-mismatches-rejected dynamodb-update-item-condition-size-mismatch-evaluates-false dynamodb-update-item-parenthesized-operand-forms-accepted
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|PutItem|2xx->4xx|$
Issue-Key: dynamodb|PutItem|2xx->4xx|status
Issue-Key: dynamodb|UpdateItem|2xx->4xx|$
Issue-Key: dynamodb|UpdateItem|2xx->4xx|status
Issue-Key: dynamodb|Query|2xx->4xx|$.Count
Issue-Key: dynamodb|Query|2xx->4xx|$.Items
Issue-Key: dynamodb|Query|2xx->4xx|$.ScannedCount
Issue-Key: dynamodb|Query|2xx->4xx|status
Issue-Key: dynamodb|GetItem|2xx->2xx|$.Item.mark
Issue-Key: dynamodb|GetItem|2xx->2xx|$.Item.marker.S
Issue-Key: dynamodb|UpdateItem|4xx->2xx|$
Issue-Key: dynamodb|UpdateItem|4xx->2xx|status
Issue-Key: dynamodb|GetItem|2xx->2xx|$.Item.touched
Issue-Key: dynamodb|GetItem|2xx->2xx|$.Item.touched.S
Issue-Commit: 656cbf9b11e99225d4fd85cb8b7394b019211c7b

# DynamoDB condition expressions: parenthesized operands are refused and a size() comparison mis-evaluates (8 causes)

> _Note: this issue was identified against the code base at commit `656cbf9b11e99225d4fd85cb8b7394b019211c7b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutItem

## Description
Eight net-new causes on dynamodb, all rooted in the condition-expression component: the parser refuses a parenthesized operand that DynamoDB's grammar accepts, the size() comparison path evaluates a false condition as true, and both leave the item in a state no AWS client would ever observe. Related but NOT covering: open issue 20261117T172826Z-dynamodb-expression-evaluation-10-net-new-causes-transaction.md (expression evaluation) does not carry any of these replay keys.

Cause 1 of 8: PutItem refuses a ConditionExpression whose comparator operands are parenthesized.
AWS: Smithy: PutItem.ConditionExpression targets ConditionExpression, a free-form expression string with no modeled syntax restriction; DynamoDB's condition grammar permits a parenthesized operand on either side of a comparator, so a condition of the form '(#name) = (:value)' is well formed and the write is evaluated and applied. Recorded response: HTTP 200 with an empty JSON body.
Emulator: HTTP 400 ValidationException with message '1 validation error detected: Invalid ConditionExpression: Syntax error; token: ")", near: "#v) ="'. The fixed text of that message class is '1 validation error detected: Invalid ConditionExpression: Syntax error; token: "<token>", near: "<fragment>"', whose variable parts are the offending token and the surrounding source fragment taken from the request; here the whole refusal is spurious because the expression is legal.
Why: a client that parenthesizes operands (very common when an expression is assembled programmatically) has every conditional write rejected as malformed, so a guarded put is impossible.
Fix: the ConditionExpression grammar/parser used by the PutItem handler's validation.
Traces: dynamodb-condition-both-operands-parenthesized#task#6 (cluster of 5 steps, incl. dynamodb-putitem-condition-operand-parentheses#task#4/#task#7/#task#10).

Cause 2 of 8: UpdateItem refuses the same parenthesized-operand condition.
AWS: Smithy: UpdateItem.ConditionExpression, same free-form ConditionExpression target; a condition such as '(#s) = (:s)' is accepted and the update applied. Recorded response: HTTP 200, empty body.
Emulator: HTTP 400 ValidationException, message '1 validation error detected: Invalid ConditionExpression: Syntax error; token: ")", near: "#s) ="' (same template, token and fragment from the request).
Why: guarded updates with parenthesized operands never run; the caller sees a malformed-expression error for a legal expression.
Fix: same parser, reached from the UpdateItem handler's validation.
Traces: dynamodb-condition-both-operands-parenthesized#task#10 (cluster of 4 steps).

Cause 3 of 8: Query refuses a request whose expression carries parenthesized operands.
AWS: Smithy: Query.KeyConditionExpression/FilterExpression target ConditionExpression; the parenthesized form is legal, so AWS answers HTTP 200 with Count, Items and ScannedCount for the matching item.
Emulator: HTTP 400 — Count, Items and ScannedCount are absent entirely.
Why: a read that AWS serves returns an error, so the client cannot verify the state it just wrote.
Fix: same expression parser, on the Query read path.
Traces: dynamodb-condition-both-operands-parenthesized#task#12.

Cause 4 of 8: because the conditional PutItem of cause 1 was refused, the item is missing the attribute the write would have set.
AWS: GetItem returns the item carrying the attribute set by the accepted conditional put (recorded: a string marker attribute present).
Emulator: that attribute is absent from the returned Item.
Why: the observable table state diverges from AWS after an identical call sequence, so any read-after-write assertion fails.
Fix: follows from cause 1 (PutItem validation); no separate state change expected.
Traces: dynamodb-condition-both-operands-parenthesized#task#7 (cluster of 5 steps, incl. dynamodb-update-item-parenthesized-operand-forms-accepted#task#5/#task#11).

Cause 5 of 8: same state divergence on the second parenthesis fixture — the marker attribute still holds its seed value instead of the value the conditional put set.
AWS: GetItem returns the marker attribute with the value written by the accepted conditional PutItem.
Emulator: GetItem returns the seed value, because the put was refused.
Why: identical to cause 4 — divergent table state after an accepted-by-AWS write.
Fix: follows from cause 1.
Traces: dynamodb-putitem-condition-operand-parentheses#task#6 (cluster of 3 steps).

Cause 6 of 8: an UpdateItem whose ConditionExpression compares size() against a value that does not match is applied instead of refused.
AWS: Smithy: error ConditionalCheckFailedException (client, HTTP 400) — 'A condition specified in the operation failed to be evaluated.' When the size() comparison is false, DynamoDB refuses the write with HTTP 400, __type 'com.amazonaws.dynamodb.v20120810#ConditionalCheckFailedException' and the fixed message 'The conditional request failed' (this message has no variable parts).
Emulator: HTTP 200 with an empty body — the condition is treated as satisfied and the update is applied.
Why: optimistic-concurrency and size-guard conditions silently do not guard; a client that relies on the 400 to detect a lost update instead corrupts the item.
Fix: the size() operand evaluation in the condition-expression evaluator used by the UpdateItem handler.
Traces: dynamodb-update-item-condition-size-mismatch-evaluates-false#task#4.

Cause 7 of 8: the item carries an attribute written by the update that AWS refuses.
AWS: GetItem returns the item WITHOUT the attribute the refused update would have set (the member is absent).
Emulator: the attribute is present with the refused update's value.
Why: the table holds data AWS would never contain; subsequent conditional logic reads a value that does not exist on AWS.
Fix: follows from cause 6.
Traces: dynamodb-update-item-condition-size-mismatch-evaluates-false#task#5.

Cause 8 of 8: after the corrected update, the attribute still holds the value written by the wrongly-accepted update.
AWS: GetItem returns the attribute with the value set by the update whose condition did hold.
Emulator: returns the earlier (wrongly applied) value, so the correct write is masked.
Why: the final state is wrong even after the client repairs its condition, which makes the divergence durable.
Fix: follows from cause 6.
Traces: dynamodb-update-item-condition-size-mismatch-evaluates-false#task#7 (cluster of 2 steps).

## Evidence
Cause 1 — expected 200 {} ; actual 400 {"__type":"ValidationException","message":"1 validation error detected: Invalid ConditionExpression: Syntax error; token: \")\", near: \"#v) =\""} (PutItem, dynamodb-condition-both-operands-parenthesized#task#6, 5 steps).
Cause 2 — expected 200 {} ; actual 400 ValidationException '... Syntax error; token: ")", near: "#s) ="' (UpdateItem, #task#10, 4 steps).
Cause 3 — expected 200 with Count=1, Items=[1 item], ScannedCount=1 ; actual 400, all three members missing (Query, #task#12).
Cause 4 — expected Item.mark={"S":"put-landed"} ; actual missing (GetItem, #task#7, 5 steps).
Cause 5 — expected Item.marker.S='m1' ; actual 'seed0' (GetItem, dynamodb-putitem-condition-operand-parentheses#task#6, 3 steps).
Cause 6 — expected 400 {"__type":"com.amazonaws.dynamodb.v20120810#ConditionalCheckFailedException","message":"The conditional request failed"} ; actual 200 {} (UpdateItem, dynamodb-update-item-condition-size-mismatch-evaluates-false#task#4).
Cause 7 — expected Item.touched absent ; actual {"S":"s-wrong"} (GetItem, #task#5).
Cause 8 — expected Item.touched.S='s-right' ; actual 's-wrong' (GetItem, #task#7, 2 steps).

## Steps to Reproduce
1. Replay fixture dynamodb-condition-both-operands-parenthesized (steps task#6, task#7, task#10, task#12), dynamodb-putitem-condition-operand-parentheses (task#4, task#6), dynamodb-update-item-parenthesized-operand-forms-accepted (task#4, task#5) and dynamodb-update-item-condition-size-mismatch-evaluates-false (task#4, task#5, task#7) against the emulator.
2. For causes 1-5: create a table, then issue PutItem/UpdateItem/Query with a ConditionExpression whose comparator operands are parenthesized, e.g. ConditionExpression='(#v) = (:v)' with ExpressionAttributeNames/{Values} supplying #v and :v; observe 400 ValidationException instead of 200, then GetItem and observe the write never landed.
3. For causes 6-8: seed an item, then UpdateItem with ConditionExpression='size(#a) = :n' where :n does not equal the attribute's size; observe 200 instead of 400 ConditionalCheckFailedException, then GetItem and observe the attribute was written.
