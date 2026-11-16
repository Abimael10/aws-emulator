Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.partiql-delete-noop-on-absent-key-and-update-from-rejected
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|ExecuteStatement|ConditionalCheckFailedException
Issue-Commit: a1915270e40223bfb23a5a7de86fe74aa15a3df0

# dynamodb: a PartiQL DELETE on a key the table does not hold is refused ConditionalCheckFailedException instead of succeeding as a no-op when the WHERE clause carries an extra non-key predicate

> _Note: this issue was identified against the code base at commit `a1915270e40223bfb23a5a7de86fe74aa15a3df0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteStatement

## Description
One net-new root cause.

Cause 1 of 1: ExecuteStatement evaluates the non-key part of a PartiQL DELETE's WHERE clause as a condition expression even when the full primary key named in the WHERE clause matches no item, so the call is refused ConditionalCheckFailedException instead of completing as a no-op. Failing check id: dynamodb.partiql-delete-noop-on-absent-key-and-update-from-rejected.

AWS: Smithy model services/service-com-amazonaws-dynamodb-dynamodb-20120810 — ExecuteStatement(Statement @length 1..8192) → ExecuteStatementOutput{Items: ItemList, NextToken, ConsumedCapacity, LastEvaluatedKey}; modeled errors include ConditionalCheckFailedException (client, HTTP 400) and ResourceNotFoundException. The model does not state the precedence rule, so for the rule itself this is AWS-documented/recorded behavior only: per the PartiQL DELETE reference (https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/ql-reference.delete.html) a DELETE statement addresses exactly one item by its complete primary key, and any further WHERE terms act as a condition on that item. When no item exists for the addressed key there is nothing to apply the condition to, so the key-not-found outcome wins: the statement succeeds with HTTP 200 and, with `RETURNING ALL OLD *`, an empty `Items` list. The recording for this check shows precisely that — `DELETE FROM "<table>" WHERE pk = '<absent-hash>' AND sk = '<absent-range>' AND <non-key-attr> = '<value>' RETURNING ALL OLD *` answers 200 with Items == [] and leaves every stored item untouched. Note the asymmetry the check also exercises as a positive control: the same DELETE against a key the table DOES hold removes the item and returns it in `Items`.

Emulator: the same statement is refused HTTP 400 ConditionalCheckFailedException with message "The conditional request failed" (fixed text, no variable parts) — i.e. the emulator applies the extra non-key predicate as a condition before, or independently of, discovering that the addressed key is absent. Because the call raises, the remainder of the sequence (the malformed `UPDATE ... FROM ... SET ... WHERE` refusal, which AWS reports as ValidationException "Statement wasn't well formed, can't be processed: Expected data manipulation", and the positive controls) is never reached, so this one defect is all that is observed.

Why: a client issuing an idempotent PartiQL DELETE guarded by an extra attribute predicate (the common "delete this row if it is still in state X" pattern) gets a hard 400 ConditionalCheckFailedException on the already-deleted / never-existed key, where AWS reports success. Delete-if-absent therefore stops being idempotent: retry loops and cleanup paths that treat ConditionalCheckFailedException as a real conflict will abort or loop, and callers cannot distinguish "the item is gone" (AWS: 200, Items []) from "the item exists but failed my predicate" (AWS: 400 ConditionalCheckFailedException).

Fix: the ExecuteStatement handler's PartiQL DELETE execution path — the ordering between primary-key lookup and evaluation of the residual non-key WHERE terms as a condition. The key-absent result must short-circuit to a successful no-op (200, Items [] when RETURNING ALL OLD * is present, no mutation of state) before the condition is evaluated; ConditionalCheckFailedException must be raised only when the addressed item exists and fails the predicate.

Related open issue that does NOT cover this one: 20261111T170613Z-dynamodb-partiql-statement-parser-rejects-aws-s-grammar.md owns the PartiQL grammar/parser refusals; this is an execution-semantics/precedence defect in DELETE, not a parse failure.

Traces: dynamodb.partiql-delete-noop-on-absent-key-and-update-from-rejected (first ExecuteStatement call of the check).

## Evidence
Check dynamodb.partiql-delete-noop-on-absent-key-and-update-from-rejected.

Setup: table with KeySchema pk(HASH,S)+sk(RANGE,S), PAY_PER_REQUEST, holding one item {pk:'p1', sk:'s1', status:'active'}.

Statement issued:
  DELETE FROM "<table>" WHERE pk = 'absent' AND sk = 'absent' AND status = 'active' RETURNING ALL OLD *

Expected (AWS, per the check's recorded behavior and the PartiQL DELETE reference): HTTP 200, response Items == [], the unrelated existing item unchanged.

Actual (emulator): HTTP 400
  botocore.errorfactory.ConditionalCheckFailedException: An error occurred (ConditionalCheckFailedException) when calling the ExecuteStatement operation: The conditional request failed

Message anatomy: "The conditional request failed" is wholly fixed text with no variable parts — the divergence is not the wording but that the error class is produced at all for a key the table does not hold. Here the status (400 vs 200) and the error code (ConditionalCheckFailedException vs none) are the requirement being violated.

The check's positive controls, never reached because the first call raised: the same DELETE against the held key ('p1'/'s1') must return Items == [the stored item] and remove it; and `UPDATE "<table>" FROM "<table>" SET status='changed' WHERE pk='p1' AND sk='s1'` must be refused ValidationException with message "Statement wasn't well formed, can't be processed: Expected data manipulation".

## Steps to Reproduce
1. CreateTable with KeySchema pk (HASH, S) + sk (RANGE, S), BillingMode PAY_PER_REQUEST; wait for ACTIVE.
2. PutItem {pk:{S:'p1'}, sk:{S:'s1'}, status:{S:'active'}}.
3. Call ExecuteStatement with Statement = DELETE FROM "<table>" WHERE pk = 'absent' AND sk = 'absent' AND status = 'active' RETURNING ALL OLD *
4. Observe 400 ConditionalCheckFailedException "The conditional request failed". AWS answers 200 with Items == [] and leaves the 'p1'/'s1' item intact.
5. Contrast: repeat step 3 with pk='p1' AND sk='s1' — that form must return the stored item in Items and delete it.
