Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.attribute-updates-update-item-item-size-409600-byte-boundary dynamodb.update-item-request-shape-byte-charges-by-clause-kind dynamodb.update-item-set-remove-fixed-action-cost-ceiling dynamodb.set-arithmetic-grouping-size-utf16-and-add-empty-string-to-set dynamodb.update-expression-malformed-syntax-rejected dynamodb.update-item-nested-arithmetic-mixed-clauses-and-if-not-exists-parent dynamodb.attribute-updates-add-delete-put-return-values-on-existing-item dynamodb.awsjson-error-code-carrier-is-body-only-rejected dynamodb.delete-item-key-duplicate-set-member-byte-order-rejected dynamodb.duplicate-set-members-in-condition-key-and-transact-rejected dynamodb.query-undefined-expression-value-precedes-resource-not-found dynamodb.update-item-expression-substitution-total-size-over-2mb-rejected dynamodb.update-table-vector-index-requires-pay-per-request
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|UpdateItem|ValidationException
Issue-Key: dynamodb.awsjson-error-code-carrier-is-body-only-rejected|run#32|AssertionError|assert status == 400, f"a tampered signature gave {status!r}: {body_sig!r}"
Issue-Key: dynamodb.delete-item-key-duplicate-set-member-byte-order-rejected|expect_error#14|AssertionError|assert got_message == message, f"unexpected message {got_message!r}"
Issue-Key: dynamodb.duplicate-set-members-in-condition-key-and-transact-rejected|run#25|AssertionError|assert message == counted + 'Input collection ["b", "b"] contains duplicates.', (
Issue-Key: dynamodb.query-undefined-expression-value-precedes-resource-not-found|run#14|AssertionError|assert code == ERR_NOT_FOUND, (
Issue-Key: dynamodb.update-table-vector-index-requires-pay-per-request|_error#20|AssertionError|raise AssertionError(f'expected the call to be refused, but it succeeded: {result!r}')
Issue-Commit: 3cd41981ee722cd642660e763820306da1bf304f

# dynamodb: 9 net-new causes — UpdateItem's over-size message and byte accounting, parenthesised UpdateExpression arithmetic refused, legacy AttributeUpdates refused, duplicate-set and sigv4 refusal shapes, Query error precedence, vector-index billing-mode switch accepted

> _Note: this issue was identified against the code base at commit `3cd41981ee722cd642660e763820306da1bf304f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateItem

## Description
Nine distinct root causes in the DynamoDB data plane and control plane, all grounded in failing behavioral checks. None is covered by an open issue; the nearest open issues are named per cause below and do not carry these replay keys.

Cause 1 of 9: UpdateItem's item-size refusal uses the wrong fixed text.
AWS: an UpdateItem that would grow the item past the 400 KB (409,600-byte) item limit is refused 400 ValidationException with the fixed text `Item size to update has exceeded the maximum allowed size` (Smithy: UpdateItem declares no dedicated size error, so the refusal is the service-common ValidationException; the message is AWS's fixed text for UpdateItem's size ceiling, distinct from PutItem's `Item size has exceeded the maximum allowed size`). The whole message is fixed text — it carries no values from the request.
Emulator: UpdateItem answers 400 ValidationException with PutItem's wording, `Item size has exceeded the maximum allowed size` — the three words `to update` are missing.
Why: a client that branches on the message (or a bisection probe that only tolerates UpdateItem's wording) cannot tell an UpdateItem size refusal from a PutItem one, and the two checks that bisect the per-clause byte ceilings abort on the first probe instead of measuring.
Fix: UpdateItem's size validation, where the over-limit ValidationException message is composed (not PutItem's).
Traces: dynamodb.attribute-updates-update-item-item-size-409600-byte-boundary, dynamodb.update-item-request-shape-byte-charges-by-clause-kind, dynamodb.update-item-set-remove-fixed-action-cost-ceiling.

Cause 2 of 9: the UpdateExpression parser rejects parentheses grouping an arithmetic operand pair.
AWS: an UpdateExpression SET clause may parenthesise an operand pair — `SET n = (n - :d)`, `SET n = (:a + :b)`, `SET a[0] = (a[0] + :x)` are all accepted and the parenthesised arithmetic is evaluated before assignment (Smithy: UpdateItem.UpdateExpression is a free-form string, so the grammar is documented behavior, not a model constraint; recorded behavior shows the call succeeding with the computed value). Only REDUNDANT (doubled) parentheses around a single operand are refused, with `1 validation error detected: Invalid UpdateExpression: The expression has redundant parentheses;`.
Emulator: every such call is refused 400 ValidationException `1 validation error detected: Invalid UpdateExpression: Syntax error; token: "(", near: "= (<first token after the paren>"` — the token and `near` fragment are taken from the request, the rest is fixed text. The grouping form is never parsed.
Why: a client that groups an arithmetic operand pair — the idiomatic way to make precedence explicit in a counter update — gets its write refused outright, so the item is never updated.
Fix: the UpdateExpression grammar/parser in the expression engine (SET operand production must accept a parenthesised operand pair while still refusing redundant parentheses).
Traces: dynamodb.set-arithmetic-grouping-size-utf16-and-add-empty-string-to-set, dynamodb.update-expression-malformed-syntax-rejected, dynamodb.update-item-nested-arithmetic-mixed-clauses-and-if-not-exists-parent.

Cause 3 of 9: UpdateItem with legacy AttributeUpdates is refused as if it carried an UpdateExpression.
AWS: UpdateItem accepts the legacy `AttributeUpdates` map (Smithy: UpdateItem.AttributeUpdates targets AttributeUpdates, a map of AttributeValueUpdate{Value, Action}; Action documents PUT/DELETE/ADD, with ADD unioning a set operand into an existing set and adding a number operand to an existing number). Recorded behavior additionally shows ADD with a one-element list operand appending to an existing list. The call succeeds and ReturnValues UPDATED_NEW/UPDATED_OLD report the touched attributes.
Emulator: the call is refused 400 ValidationException `An operand in the update expression has an incorrect data type` — a message about an update expression for a request that carried no UpdateExpression at all, so the legacy path is being rewritten into an expression and then rejected by the expression type checker.
Why: every client still using the legacy AttributeUpdates parameter (older SDK code, DocumentClient `update` shims) has its writes refused, and the message points at an expression the caller never sent.
Fix: UpdateItem's legacy-parameter handling — the AttributeUpdates ADD/DELETE/PUT semantics and the operand type rules applied to them.
Traces: dynamodb.attribute-updates-add-delete-put-return-values-on-existing-item.

Cause 4 of 9: a tampered SigV4 signature is answered 403 with an unqualified error type instead of 400 with the coral-namespaced one.
AWS: a DynamoDB request whose Authorization signature does not verify is answered HTTP 400 with an awsJson1.0 body whose `__type` is `com.amazon.coral.service#InvalidSignatureException` — note the coral namespace rather than DynamoDB's own `com.amazonaws.dynamodb.v20120810#...`, and note the status is 400, not 403 (recorded behavior; Smithy models no auth error on the operation, so the shape comes from the recording).
Emulator: the same request is answered HTTP 403 with body `{"__type": "InvalidSignatureException", "message": "The request signature we calculated does not match the signature you provided. Check your AWS Secret Access Key and signing method. Consult the service documentation for details."}` — right message text, wrong status, and a bare `__type` with no namespace prefix.
Why: an SDK that keys retry/credential-refresh behaviour off the 400-with-namespaced-__type shape (and any client that distinguishes 403 authorization denial from 400 signature mismatch) mis-classifies the failure. The open issue 20261111T170613Z-dynamodb-the-http-sigv4-layer-omits-x-amz.md covers other sigv4 header defects and does not carry this check.
Fix: the HTTP SigV4 front-door's error serializer (status code and `__type` namespace for InvalidSignatureException).
Traces: dynamodb.awsjson-error-code-carrier-is-body-only-rejected.

Cause 5 of 9: the duplicate-set-member ValidationException omits AWS's `1 validation error detected: ` prefix on DeleteItem.
AWS: a duplicate member in a set value supplied outside an Item — a Key, a ConditionExpression value, a Transact*Items slot — is refused 400 ValidationException. On DeleteItem the message is prefixed with the validation-error count: `1 validation error detected: One or more parameter values were invalid: Input collection [<members>] contains duplicates.` (SS/NS/BS variants differ in whether and how members are named; the prefix and the `One or more parameter values were invalid: ` fixed text are AWS's, the member list comes from the request).
Emulator: DeleteItem answers the same code but drops the leading `1 validation error detected: `, e.g. `One or more parameter values were invalid: Input collection ["b", "b"] contains duplicates.` for a Key of `{"pk": {"SS": ["b","b"]}}`.
Why: the prefix is how a client (and AWS's own tooling) tells a per-request validation-error report from a parameter-value refusal; its absence makes the two indistinguishable and breaks exact-message assertions.
Fix: DeleteItem's key/value validation, where the duplicate-set-member ValidationException message is composed.
Traces: dynamodb.delete-item-key-duplicate-set-member-byte-order-rejected, dynamodb.duplicate-set-members-in-condition-key-and-transact-rejected.

Cause 6 of 9: the BS duplicate-member list in that message is not ordered by ascending byte value.
AWS: when the duplicate-bearing set is a BS, the message names the distinct members seen before the duplicate in ASCENDING BYTE ORDER, with the duplicate appended last — not the order the request listed them in. For a Key of `{"pk": {"BS": [0x7a, 0x01, 0x01]}}` the collection is rendered `["AQ==","eg==","AQ=="]` (0x01 then 0x7a, then the duplicate 0x01), followed by the fixed `of type BS contains duplicates.` (recorded behavior; the members are base64 of the request's bytes).
Emulator: the members are echoed in request order — `["eg==","AQ==","AQ=="]`.
Why: the ordering is the only part of the message that tells the caller which member DynamoDB considered duplicated; echoing request order reports a different collection than AWS does for the same request.
Fix: the duplicate-set-member message formatter for BS values (sort the named distinct members by byte value before rendering, append the duplicate).
Traces: dynamodb.delete-item-key-duplicate-set-member-byte-order-rejected.

Cause 7 of 9: Query runs key-condition semantic validation before the table-existence lookup.
AWS: a Query naming a table that does not exist is refused 400 ResourceNotFoundException `Requested resource not found` (Smithy: Query lists ResourceNotFoundException, httpError 400), and that existence check WINS over a wrong-typed key-condition operand: a Query against an absent table with `KeyConditionExpression="pk = :pk AND sk = :sk AND extra = :extra"` and `:pk` bound to an `L` value — every placeholder defined — is still ResourceNotFoundException. Only a defect caught during expression PARSING (an undefined placeholder) preempts the lookup, with `Invalid KeyConditionExpression: An expression attribute value used in expression is not defined; attribute value: :extra`.
Emulator: that call is refused ValidationException instead, so the semantic (operand-type) check is evaluated ahead of the resource lookup.
Why: a client probing whether a table exists gets a validation error instead of ResourceNotFoundException and cannot run its create-on-missing branch; error precedence is part of the contract.
Fix: Query's request-handling order — the table lookup must precede key-condition operand-type validation and follow only expression parsing.
Traces: dynamodb.query-undefined-expression-value-precedes-resource-not-found.

Cause 8 of 9: UpdateItem charges ExpressionAttributeValues bytes against the 400 KB item-size ceiling.
AWS: placeholder values that are only COMPARED (a ConditionExpression operand) or that resolve to a no-op (a DELETE of a member from an attribute that does not exist) never become item bytes, so they do not count toward the item-size limit. An UpdateItem whose ExpressionAttributeValues total just under the 2 MB substitution cap — split across ConditionExpression and UpdateExpression — is evaluated and applies, writing only its small SET attribute. Over 2 MB of substitution values is refused 400 ValidationException `1 validation error detected: ExpressionAttributeValues exceeds max size` (recorded behavior; Smithy constrains neither total).
Emulator: the under-cap call is refused 400 ValidationException `Item size has exceeded the maximum allowed size`, i.e. the substitution values are summed into the item size.
Why: any conditional write whose condition compares against a large value is refused even though the item it writes is tiny — and the real 2 MB ExpressionAttributeValues cap is never reached, so the modeled refusal is unreachable. The open issue 20261111T170613Z-dynamodb-item-size-batch-size-and-request-size.md covers other size-accounting checks and does not carry this one.
Fix: UpdateItem's size accounting — count only the bytes the finished item occupies plus each clause's own request shape, and add the separate ExpressionAttributeValues total cap.
Traces: dynamodb.update-item-expression-substitution-total-size-over-2mb-rejected.

Cause 9 of 9: UpdateTable accepts switching a vector-index-bearing table to PROVISIONED.
AWS: vector indexes exist only on PAY_PER_REQUEST tables, so UpdateTable with `BillingMode=PROVISIONED` (plus ProvisionedThroughput) on a table that already carries a vector index is refused 400 ValidationException `One or more parameter values were invalid: Vector indexes are only supported for PAY_PER_REQUEST tables`, leaving BillingModeSummary.BillingMode PAY_PER_REQUEST and the VectorIndexes list unchanged (Smithy: UpdateTable.BillingMode targets BillingMode, UpdateTable.VectorIndexUpdates targets VectorIndexUpdateList; the cross-member rule is AWS behavior, enforced as ValidationException). The emulator already refuses the mirror case (adding a vector index to a PROVISIONED table) correctly.
Emulator: the call succeeds 200, returning TableDescription with TableStatus UPDATING, BillingModeSummary.BillingMode PROVISIONED and the vector index still listed ACTIVE — a state AWS cannot produce.
Why: a client reaches a table that is simultaneously PROVISIONED and vector-indexed; subsequent vector reads/writes are billed and validated against a mode AWS forbids. The open issue 20261111T170613Z-dynamodb-vector-index-validation-is-not-enforced-its.md covers other vector-index validation checks and does not carry this one.
Fix: UpdateTable's validation — reject a billing-mode change to PROVISIONED while any vector index exists on the table, before any state mutation.
Traces: dynamodb.update-table-vector-index-requires-pay-per-request.

## Evidence
Cause 1 — expected ('ValidationException', 'Item size to update has exceeded the maximum allowed size'), got ('ValidationException', 'Item size has exceeded the maximum allowed size'); the two bisection checks abort with "unexpected error instead of the size refusal: ('ValidationException', 'Item size has exceeded the maximum allowed size')".

Cause 2 — `SET n = (n - :d)` / `SET n = (:a + :b)` / `SET a[0] = (a[0] + :x)` each raise: ValidationException `1 validation error detected: Invalid UpdateExpression: Syntax error; token: "(", near: "= (n"` (resp. `"= (:a"`, `"= (a"`). Expected: the call succeeds and the parenthesised arithmetic is applied (n=10, :d=3 -> 7; :a=1,:b=2 -> 3; a[0]=10,:x=5 -> 15).

Cause 3 — UpdateItem with AttributeUpdates only (no UpdateExpression member in the request) raises ValidationException `An operand in the update expression has an incorrect data type`. Expected: ADD unions `{b,c}` into the existing SS `{a,b}` as `{a,b,c}`, ADD of a one-element list appends it, DELETE with a Value removes one member, DELETE with no Value removes the attribute, PUT replaces a map wholesale, each reported through UPDATED_NEW/UPDATED_OLD.

Cause 4 — raw POST to the DynamoDB endpoint with `x-amz-target: DynamoDB_20120810.DescribeTable` and the last 4 chars of the Authorization signature replaced: emulator answers HTTP 403, body `{"__type": "InvalidSignatureException", "message": "The request signature we calculated does not match the signature you provided. Check your AWS Secret Access Key and signing method. Consult the service documentation for details."}`. Expected: HTTP 400, body `__type` == `com.amazon.coral.service#InvalidSignatureException`.

Cause 5 — DeleteItem Key `{"pk": {"SS": ["b","b"]}}`: got message `One or more parameter values were invalid: Input collection ["b", "b"] contains duplicates.`; expected `1 validation error detected: One or more parameter values were invalid: Input collection ["b", "b"] contains duplicates.`

Cause 6 — DeleteItem Key `{"pk": {"BS": [b"\x7a", b"\x01", b"\x01"]}}`: got `One or more parameter values were invalid: Input collection ["eg==","AQ==","AQ=="]of type BS contains duplicates.`; expected `1 validation error detected: One or more parameter values were invalid: Input collection ["AQ==","eg==","AQ=="]of type BS contains duplicates.`

Cause 7 — Query on an absent table, `KeyConditionExpression="pk = :pk AND sk = :sk AND extra = :extra"`, `:pk={"L":[{"S":"x"}]}`, `:sk`/`:extra` both defined: got code 'ValidationException'; expected 'ResourceNotFoundException' with message `Requested resource not found` (the same call with `:extra` left undefined is correctly ValidationException).

Cause 8 — UpdateItem `UpdateExpression="SET marker = :m DELETE tags :vu"`, `ConditionExpression="attribute_exists(pk) OR pk = :vc"` with `:vc` a ~950 KB S and `:vu` an SS holding a ~950 KB member (total under 2 MB), writing only `marker`: raised ValidationException `Item size has exceeded the maximum allowed size`. Expected: accepted, `marker` reads back as written and `tags` is not created; only a substitution total over 2 MB is refused, with `1 validation error detected: ExpressionAttributeValues exceeds max size`.

Cause 9 — UpdateTable(TableName=<table with vector index idx2>, BillingMode='PROVISIONED', ProvisionedThroughput={1,1}) returned HTTP 200 with TableDescription.TableStatus 'UPDATING', BillingModeSummary.BillingMode 'PROVISIONED' and VectorIndexes still listing idx2 ACTIVE. Expected: ValidationException `One or more parameter values were invalid: Vector indexes are only supported for PAY_PER_REQUEST tables`, table unchanged.

## Steps to Reproduce
Replay the failing checks listed in trace_ids against the emulator:
1. dynamodb.attribute-updates-update-item-item-size-409600-byte-boundary — create a PAY_PER_REQUEST table, UpdateItem (legacy AttributeUpdates PUT) growing the item to exactly 409,600 bytes, then one byte more, and compare the refusal message.
2. dynamodb.update-item-request-shape-byte-charges-by-clause-kind and dynamodb.update-item-set-remove-fixed-action-cost-ceiling — bisect the accepted SET/REMOVE byte ceiling; the probe aborts on the first refusal because the message is not UpdateItem's.
3. dynamodb.set-arithmetic-grouping-size-utf16-and-add-empty-string-to-set, dynamodb.update-expression-malformed-syntax-rejected, dynamodb.update-item-nested-arithmetic-mixed-clauses-and-if-not-exists-parent — UpdateItem with `SET n = (n - :d)`, `SET n = (:a + :b)` and `SET a[0] = (a[0] + :x)`.
4. dynamodb.attribute-updates-add-delete-put-return-values-on-existing-item — PutItem an item with a number, an SS, an L and an M, then UpdateItem with AttributeUpdates ADD/DELETE/PUT and ReturnValues UPDATED_NEW/UPDATED_OLD.
5. dynamodb.awsjson-error-code-carrier-is-body-only-rejected — sign a raw DescribeTable POST, corrupt the last 4 characters of the Authorization signature, and inspect the HTTP status and body `__type`.
6. dynamodb.delete-item-key-duplicate-set-member-byte-order-rejected and dynamodb.duplicate-set-members-in-condition-key-and-transact-rejected — DeleteItem with a Key whose SS/BS carries a duplicate member.
7. dynamodb.query-undefined-expression-value-precedes-resource-not-found — Query an absent table with a wrong-typed key-condition operand and a third ANDed clause, all placeholders defined.
8. dynamodb.update-item-expression-substitution-total-size-over-2mb-rejected — UpdateItem whose ExpressionAttributeValues total just under 2 MB across ConditionExpression and UpdateExpression while writing one small attribute.
9. dynamodb.update-table-vector-index-requires-pay-per-request — CreateTable PAY_PER_REQUEST with a VectorIndexes entry, wait ACTIVE, then UpdateTable with BillingMode PROVISIONED.
