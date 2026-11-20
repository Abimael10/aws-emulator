Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodb-transact-get-items-expression-and-key-defects-outrank-cancellation-and-duplicates
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodb-transact-get-items-expression-and-key-defects-outrank-cancellation-and-duplicates transact-write-items-update-grows-item-past-size-limit dynamodb-update-table-gsi-update-nested-member-type-mismatch-rejected incremental-export-invalid-windows-rejected dynamodb-bulk-operation-management-refusals
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|ExportTableToPointInTime|4xx->4xx|$.message
Issue-Key: dynamodb|ListBulkOperations|4xx->2xx|$.__type
Issue-Key: dynamodb|ListBulkOperations|4xx->2xx|$.message
Issue-Key: dynamodb|ListBulkOperations|4xx->2xx|status
Issue-Related: 20261104T061030Z-dynamodb-serves-the-wrong-error-class-for-malformed.md
Issue-Related: 20261111T221143Z-dynamodb-condition-key-and-filter-expression-defects-are.md
Issue-Related: 20261117T221515Z-dynamodb-refusal-wording-and-validation-precedence-diverge-across.md
Issue-Related: 20261120T225636Z-dynamodb-expression-and-cursor-refusals-report-a-different.md
Issue-Related: 20261120T225636Z-dynamodb-validationexception-envelope-diverges-member-paths-value-rendering.md
Issue-Commit: 4589a545c981676c72550ecd598553d4d546f08e

# DynamoDB refusal precedence picks the wrong error class, and two request limits are not enforced at all

> _Note: this issue was identified against the code base at commit `4589a545c981676c72550ecd598553d4d546f08e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TransactGetItems

## Description
Each of these requests is refusable, but the emulator picks a different error class than AWS (resource-existence vs request-validation vs transaction-cancellation), words the refusal itself, or does not refuse at all. Five independent net-new causes (all with empty stub_path and empty owned_by in the replay report).

THIS BUNDLE CONTAINS 5 CAUSES.

Cause 1 of 5 — TransactGetItems: a nonexistent table must outrank the duplicate-item check.
AWS: Smithy: TransactGetItems models ResourceNotFoundException (client, HTTP 400). When the transaction names a table that does not exist, AWS answers 400 com.amazonaws.dynamodb.v20120810#ResourceNotFoundException with message "Requested resource not found" — i.e. table resolution happens BEFORE the per-transaction structural check that forbids two operations on one item. The message here is the fixed short form with no interpolated values.
Emulator: 400 com.amazon.coral.validate#ValidationException with "Transaction request cannot include multiple operations on one item" — it runs the duplicate-item validation first and so reports a request defect for a request whose real problem is a missing table.
Why: a client's retry logic branches on the error class — ResourceNotFoundException is a provisioning problem, ValidationException is a caller bug — so it takes the wrong recovery path and never learns the table is absent.
Fix: ordering in the TransactGetItems pre-flight — resolve every named table before the structural duplicate check.
Traces: dynamodb-transact-get-items-expression-and-key-defects-outrank-cancellation-and-duplicates#task#8.

Cause 2 of 5 — TransactWriteItems: an Update that grows an item past the size limit is a validation refusal, not a cancellation.
AWS: 400 com.amazon.coral.validate#ValidationException with "Item size to update has exceeded the maximum allowed size" — fixed text, no interpolated values. AWS rejects the transaction outright rather than cancelling it, so there is no CancellationReasons list.
Emulator: 400 com.amazonaws.dynamodb.v20120810#TransactionCanceledException with "Transaction cancelled, please refer cancellation reasons for specific reasons [ValidationError]" — the over-size update is reported as a per-item cancellation.
Why: a caller that inspects CancellationReasons to find the offending action finds a generic ValidationError and never the size message; code that treats TransactionCanceledException as retryable will retry a request that can never succeed.
Fix: the item-size check in the TransactWriteItems update path must raise the validation refusal before the transaction is built and cancelled. The open issue 20261120T124018Z-dynamodb-6-net-new-causes-transactwriteitems-cancellation-reason.md describes a related over-size cancellation but does not carry this replay key, so this cause must be closed here.
Traces: transact-write-items-update-grows-item-past-size-limit#task#6.

Cause 3 of 5 — UpdateTable: a GSI update missing AttributeDefinitions is answered as a missing table.
AWS: 400 com.amazon.coral.validate#ValidationException with "One or more parameter values were invalid: AttributeDefinitions is not specified for index: <index name>" — fixed text "One or more parameter values were invalid: AttributeDefinitions is not specified for index: " plus the index name taken from the GlobalSecondaryIndexUpdates entry. Smithy: UpdateTable.AttributeDefinitions is optional at the shape level and UpdateTable models both ResourceNotFoundException and the validation class, so the request-shape rule is enforced before the table lookup.
Emulator: 400 com.amazonaws.dynamodb.v20120810#ResourceNotFoundException with "Requested resource not found: Table: <table name> not found" — it resolves the table first and reports its absence instead of the missing AttributeDefinitions.
Why: the caller is told to create a table when the real defect is that the GSI update is incomplete; the error class is wrong for its retry logic.
Fix: validation precedence in the UpdateTable handler — validate GlobalSecondaryIndexUpdates against AttributeDefinitions before resolving the table.
Traces: dynamodb-update-table-gsi-update-nested-member-type-mismatch-rejected#task#7.

Cause 4 of 5 — ExportTableToPointInTime: the incremental-export window refusal is emulator-authored.
AWS: 400 with "Incremental export period to time cannot be greater than the current time : <ExportToTime>" — fixed text "Incremental export period to time cannot be greater than the current time : " plus the submitted IncrementalExportSpecification.ExportToTime rendered as an ISO-8601 UTC instant. Smithy: ExportTableToPointInTime.IncrementalExportSpecification carries ExportFromTime/ExportToTime timestamps and the operation models InvalidExportTimeException; the message is recorded behavior.
Emulator: 400 with "Export Time is invalid" — a generic refusal that names neither the rule (the window end may not be in the future) nor the offending timestamp.
Why: a caller with a bad export window cannot tell which bound is wrong or why, and anything matching AWS's wording fails.
Fix: the ExportTableToPointInTime incremental-window validation message.
Traces: incremental-export-invalid-windows-rejected#task#9, #task#10, #task#11.

Cause 5 of 5 — ListBulkOperations: MaxResults above the cap is accepted instead of refused.
AWS: 400 com.amazon.coral.validate#ValidationException with "1 validation error detected: Value '<submitted value>' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to <max>". Smithy types ListBulkOperations.MaxResults as ListBulkOperationsMaxResults @range 1..100, but the recorded refusal shows the enforced ceiling is 25, and the recorded response is the requirement: a MaxResults of 26 is refused with that message naming the submitted value, the member path 'maxResults' and the limit.
Emulator: 200 with a normal listing body, no __type and no message — the range is not enforced at all.
Why: a caller that exceeds the page-size cap gets a silent success and may assume a larger page size is supported, and the refusal it must handle in AWS never appears.
Fix: range validation on ListBulkOperations.MaxResults in that operation's input validation. The open issue 20261117T221515Z-dynamodb-bulk-operation-control-plane-is-unrouted-five.md owns other steps of this same fixture but not task#4, which is unclaimed and must be closed here.
Traces: dynamodb-bulk-operation-management-refusals#task#4.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout; all five are `verdict: mismatch` clusters with empty stub_path and empty owned_by.

Cause 1 (TransactGetItems, dynamodb-transact-get-items-expression-and-key-defects-outrank-cancellation-and-duplicates#task#8, 1 affected, status 400 -> 400):
  expected $.__type = 'com.amazonaws.dynamodb.v20120810#ResourceNotFoundException', $.message = 'Requested resource not found'
  actual   $.__type = 'com.amazon.coral.validate#ValidationException', $.message = 'Transaction request cannot include multiple operations on one item'

Cause 2 (TransactWriteItems, transact-write-items-update-grows-item-past-size-limit#task#6, 1 affected, status 400 -> 400):
  expected $.__type = 'com.amazon.coral.validate#ValidationException', $.message = 'Item size to update has exceeded the maximum allowed size'
  actual   $.__type = 'com.amazonaws.dynamodb.v20120810#TransactionCanceledException', $.message = 'Transaction cancelled, please refer cancellation reasons for specific reasons [ValidationError]'

Cause 3 (UpdateTable, dynamodb-update-table-gsi-update-nested-member-type-mismatch-rejected#task#7, 1 affected, status 400 -> 400):
  expected $.__type = 'com.amazon.coral.validate#ValidationException', $.message = 'One or more parameter values were invalid: AttributeDefinitions is not specified for index: <index name>'
  actual   $.__type = 'com.amazonaws.dynamodb.v20120810#ResourceNotFoundException', $.message = 'Requested resource not found: Table: <table name> not found'

Cause 4 (ExportTableToPointInTime, incremental-export-invalid-windows-rejected#task#9, 3 affected, status 400 -> 400):
  expected $.message = 'Incremental export period to time cannot be greater than the current time : <ExportToTime>' (recorded fill-in: 2099-01-01T02:00:00Z)
  actual   $.message = 'Export Time is invalid'

Cause 5 (ListBulkOperations, dynamodb-bulk-operation-management-refusals#task#4, 1 affected, status 400 -> 200):
  expected status 400, $.__type = 'com.amazon.coral.validate#ValidationException', $.message = "1 validation error detected: Value '26' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 25"
  actual   status 200, no $.__type, no $.message

Model citations: Smithy TransactGetItems errors include ResourceNotFoundException (client, 400); UpdateTable errors include ResourceNotFoundException and the common validation class, AttributeDefinitions optional on the input shape; ExportTableToPointInTime.IncrementalExportSpecification{ExportFromTime, ExportToTime: timestamp}, errors include InvalidExportTimeException; ListBulkOperations.MaxResults -> ListBulkOperationsMaxResults @range 1..100 (recorded enforced ceiling 25).

## Steps to Reproduce
1. Boot the emulator built from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1: replay dynamodb-transact-get-items-expression-and-key-defects-outrank-cancellation-and-duplicates step task#8 — TransactGetItems naming a table that does not exist together with two actions on one item — and check $.__type/$.message.
3. Cause 2: replay transact-write-items-update-grows-item-past-size-limit step task#6 — TransactWriteItems whose Update grows the item past the item-size ceiling — and check that it is refused as a ValidationException, not cancelled.
4. Cause 3: replay dynamodb-update-table-gsi-update-nested-member-type-mismatch-rejected step task#7 — UpdateTable with a GlobalSecondaryIndexUpdates entry and no matching AttributeDefinitions, against an absent table — and check $.__type/$.message.
5. Cause 4: replay incremental-export-invalid-windows-rejected steps task#9-task#11 — ExportTableToPointInTime with IncrementalExportSpecification.ExportToTime in the future — and compare $.message.
6. Cause 5: replay dynamodb-bulk-operation-management-refusals step task#4 — ListBulkOperations with MaxResults above the cap — and check it is refused 400 with the maxResults range message.
7. All five must pass before closing.
