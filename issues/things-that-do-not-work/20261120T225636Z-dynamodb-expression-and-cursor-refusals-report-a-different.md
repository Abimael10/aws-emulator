Issue-Source: trace-sweep-divergence
Issue-Trace: ddbval-update-expression-conflict-arity-and-operand-type-rejected
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ddbval-update-expression-conflict-arity-and-operand-type-rejected ddbval-updateitem-condition-expression-function-restrictions-rejected dynamodb-condition-and-update-expression-operand-type-and-path-conflict-refusals dynamodb-ddbstrip-traj-update-expression-grammar-mistakes dynamodb-document-path-into-key-or-index-key-rejected dynamodb-empty-tablename-masks-invalid-returnvalues dynamodb-legacy-attribute-updates-action-type-validation dynamodb-legacy-attribute-updates-binary-set-and-scalar-type-mismatches dynamodb-legacy-expected-and-attribute-updates-malformed-combinations dynamodb-legacy-expected-comparison-operator-arity-write-ops dynamodb-null-member-and-table-name-aggregation-on-item-ops dynamodb-update-expression-function-not-allowed-rejected dynamodb-update-item-duplicate-set-members-key-attributeupdates-condition-rejected dynamodb-update-item-table-name-and-enum-violations-aggregate-with-null-key dynamodb-update-item-table-name-pattern-violation-reported dynamodb-update-item-validation-error-aggregation-forms dynamodb-updateitem-add-delete-set-syntax-errors-rejected update-expression-list-index-syntax-errors-rejected ddbval-transact-write-items-update-function-context-ordering dynamodb-transact-write-items-binary-and-out-of-order-set-duplicates-rejected dynamodb-transact-write-items-if-not-exists-in-condition-rejected dynamodb-transact-write-items-mixed-update-delete-member-rejected dynamodb-filter-expression-condition-function-as-operand-rejected dynamodb-legacy-query-keyconditions-comparisonoperator-extended dynamodb-query-conditionaloperator-enum-outranks-select-and-table-existence dynamodb-query-duplicate-set-member-across-value-positions-rejected dynamodb-query-expression-attribute-map-and-filter-validation dynamodb-query-filter-expression-list-index-syntax-errors dynamodb-query-key-condition-value-first-function-and-value-only-rejected dynamodb-query-keyconditions-structure-validation dynamodb-query-projection-expression-alphabetic-list-index-syntax dynamodb-query-scan-exclusive-start-key-attributevalue-validation-rejected dynamodb-query-scan-index-name-length-and-pattern-rejected dynamodb-query-scan-table-name-validation-boundaries-rejected dynamodb-query-select-joined-defects dynamodb-scan-query-filter-expression-document-path-and-token-syntax-errors gsi-query-exclusive-start-key-outside-boundary-rejected lsi-query-exclusive-start-key-outside-boundary-rejected query-base-table-range-predicate-exclusive-start-key-foreign-partition-rejected query-filter-function-zero-operand-and-bare-size-rejected query-gsi-composite-index-exclusive-start-key-foreign-hash-and-range-rejected query-gsi-full-key-equality-foreign-hash-exclusive-start-key-rejected query-key-condition-expression-grammar-rejected ddb-gsi-include-projection-scan-select-all-attributes-rejected ddb-gsi-lsi-scan-esk-malformed-cursor-variants ddb-index-cursor-key-type-mismatch-scan-and-query ddbdepth-scan-pagination-boundary dynamodb-filter-expression-list-index-non-numeric-and-empty-bracket-rejected dynamodb-redundant-parentheses-expression-rejected dynamodb-scan-filter-expression-function-wrong-position-attribute-not-exists-and-in-list dynamodb-scan-filter-expression-redundant-parens-around-operands-and-function-args dynamodb-scan-filter-expression-syntax-error-catalog scan-filter-function-operand-count-and-type-rejected dynamodb-batch-execute-statement-and-execute-transaction-reject-unconvertible-n-parameter
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|UpdateItem|4xx->4xx|$.message
Issue-Key: dynamodb|TransactWriteItems|4xx->4xx|$.message
Issue-Key: dynamodb|Query|4xx->4xx|$.message
Issue-Key: dynamodb|Scan|4xx->4xx|$.message
Issue-Key: dynamodb|ExecuteTransaction|4xx->4xx|$.message
Issue-Commit: 4589a545c981676c72550ecd598553d4d546f08e

# DynamoDB expression and cursor refusals report a different defect than AWS diagnoses

> _Note: this issue was identified against the code base at commit `4589a545c981676c72550ecd598553d4d546f08e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateItem

## Description
The emulator refuses these requests with the right status (400) but its diagnostic names a different defect than the one AWS diagnoses: a path conflict becomes an invalid path, a mis-placed known function becomes an unknown function name, a function-position error becomes a generic syntax error, a typed cursor mismatch becomes a generic bad-cursor message, and a non-convertible PartiQL parameter becomes a stored-number overflow. Five independent net-new causes (all with empty stub_path and empty owned_by in the replay report).

THIS BUNDLE CONTAINS 5 CAUSES.

Cause 1 of 5 — UpdateItem: two conflicting document paths in one UpdateExpression.
AWS: refusal is 400 com.amazon.coral.validate#ValidationException with "1 validation error detected: Invalid UpdateExpression: Two document paths conflict with each other; must remove or rewrite one of these paths; path one: <path one>, path two: <path two>". Fixed text: the "<n> validation error detected: " envelope plus "Invalid UpdateExpression: Two document paths conflict with each other; must remove or rewrite one of these paths; path one: , path two: ". The two paths are taken from the request and rendered as bracketed element lists, with list indexes rendered as a bracketed ordinal (a map hop as the attribute name, an index hop as [0]). Recorded behavior supplies the message; the model supplies only the error class.
Emulator: "The document path provided in the update expression is invalid for update" — no validation-error envelope, and it reports the path as unusable instead of diagnosing the CONFLICT between two paths, so neither offending path is named.
Why: a client cannot tell which two clauses of its UpdateExpression collide, and cannot distinguish this from a genuinely malformed path.
Fix: the UpdateExpression analyzer's diagnostics (path-overlap detection and its message), not the state layer.
Traces: ddbval-update-expression-conflict-arity-and-operand-type-rejected#task#4, plus 37 further steps in the cluster (ddbval-updateitem-condition-expression-function-restrictions-rejected#task#4, dynamodb-legacy-attribute-updates-action-type-validation#task#4, ...).

Cause 2 of 5 — TransactWriteItems: a known condition function used inside an UpdateExpression is reported as an unknown function.
AWS: "Invalid UpdateExpression: The function is not allowed in an update expression; function: <function name>". The fixed text distinguishes a RECOGNISED DynamoDB function used in a context that forbids it from a name the grammar does not know; the function name is taken from the request. Recorded behavior only.
Emulator: "Invalid UpdateExpression: Invalid function name; function: <function name>" — it reports `contains` (a valid DynamoDB condition function) as an unknown name, conflating the two defect classes.
Why: a caller is told its function name is misspelled when the real rule is that the function is not permitted in an update expression, so it will not reach for the right fix.
Fix: the update-expression function table / context check in the expression evaluator — recognise the function, then reject by context.
Traces: ddbval-transact-write-items-update-function-context-ordering#task#4, plus 7 further steps (dynamodb-transact-write-items-if-not-exists-in-condition-rejected#task#5, dynamodb-transact-write-items-mixed-update-delete-member-rejected#task#5, ...).

Cause 3 of 5 — Query/Scan: a condition function used as an operand is refused as a syntax error on an unrelated token.
AWS: "Invalid FilterExpression: The function is not allowed to be used this way in an expression; function: <function name>". AWS parses the whole expression, recognises the function, and rejects its POSITION; the function name comes from the request. Recorded behavior only.
Emulator: "Invalid FilterExpression: Syntax error; token: \"<token>\", near: \"<fragment>\"" — a generic syntax error naming a token elsewhere in the expression (the recorded run points at the comparison operator, not the function), so the actual defect is never named.
Why: the caller is pointed at a well-formed part of its expression and never learns that the function cannot be used as an operand.
Fix: the Filter/Condition expression parser's function-position diagnostics (classify before falling back to the syntax-error template).
Traces: dynamodb-filter-expression-condition-function-as-operand-rejected#task#10, plus 35 further steps (dynamodb-query-key-condition-value-first-function-and-value-only-rejected#task#4, dynamodb-query-conditionaloperator-enum-outranks-select-and-table-existence#task#4, ...).

Cause 4 of 5 — Scan/Query: an ExclusiveStartKey whose index-key type does not match the index's schema.
AWS: "One or more parameter values were invalid: Type mismatch for Index Key <key name> Expected: <modeled attribute type> Actual: <submitted attribute type> IndexName: <index name>". Fixed text is "One or more parameter values were invalid: Type mismatch for Index Key  Expected:  Actual:  IndexName: "; the key name, the expected type (from the index's KeySchema plus AttributeDefinitions) and the actual type (from the submitted AttributeValue) and the index name all come from the request and the table's state. Recorded behavior only.
Emulator: "The provided starting key is invalid" — a single generic cursor refusal that names neither the key, the expected and actual types, nor the index.
Why: a paginating client given a cursor from the wrong index cannot tell which key or type is wrong, and a suite matching AWS's wording fails.
Fix: ExclusiveStartKey validation in the Scan/Query read path — validate each key element against the resolved index schema and raise the typed message.
Traces: ddb-gsi-include-projection-scan-select-all-attributes-rejected#task#10, plus 31 further steps (ddb-index-cursor-key-type-mismatch-scan-and-query#task#8, dynamodb-query-scan-exclusive-start-key-attributevalue-validation-rejected#task#10, ...).

Cause 5 of 5 — ExecuteTransaction: a PartiQL parameter that cannot be converted to a number is reported as a stored-number overflow.
AWS: 400 with "The parameter cannot be converted to a numeric value: <literal>" — the literal is the submitted parameter value. Recorded behavior only; the model types the parameter as an AttributeValue and says nothing about the message.
Emulator: "Number overflow. Attempting to store a number with magnitude larger than supported range" — it classifies an un-coercible PARAMETER as an overflow on a value being STORED, which is a different error class in AWS's vocabulary and never echoes the offending literal.
Why: the caller is told its number is too large to store when the real defect is that the parameter is not a number at all, and the offending literal is not identified.
Fix: the PartiQL parameter-coercion error path for ExecuteTransaction. The BatchExecuteStatement sibling of the same fixture is already owned by 20261112T231149Z-dynamodb-reads-and-partiql-refusals-diverge-valid-projections.md; the ExecuteTransaction cluster is NOT and must be closed here.
Traces: dynamodb-batch-execute-statement-and-execute-transaction-reject-unconvertible-n-parameter#task#8.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout; all five are `verdict: mismatch` clusters with empty stub_path and empty owned_by.

Cause 1 (UpdateItem, ddbval-update-expression-conflict-arity-and-operand-type-rejected#task#4, 38 affected, status 400 -> 400):
  expected $.message = "1 validation error detected: Invalid UpdateExpression: Two document paths conflict with each other; must remove or rewrite one of these paths; path one: [a, b], path two: [a, [0]]"
  actual   $.message = "The document path provided in the update expression is invalid for update"

Cause 2 (TransactWriteItems, ddbval-transact-write-items-update-function-context-ordering#task#4, 8 affected, status 400 -> 400):
  expected $.message = "Invalid UpdateExpression: The function is not allowed in an update expression; function: contains"
  actual   $.message = "Invalid UpdateExpression: Invalid function name; function: contains"

Cause 3 (Query, dynamodb-filter-expression-condition-function-as-operand-rejected#task#10, 36 affected, status 400 -> 400):
  expected $.message = "Invalid FilterExpression: The function is not allowed to be used this way in an expression; function: attribute_exists"
  actual   $.message = "Invalid FilterExpression: Syntax error; token: \"BETWEEN\", near: \") BETWEEN :b\""

Cause 4 (Scan, ddb-gsi-include-projection-scan-select-all-attributes-rejected#task#10, 32 affected, status 400 -> 400):
  expected $.message = "One or more parameter values were invalid: Type mismatch for Index Key gsk2 Expected: N Actual: S IndexName: gsi2"
  actual   $.message = "The provided starting key is invalid"

Cause 5 (ExecuteTransaction, dynamodb-batch-execute-statement-and-execute-transaction-reject-unconvertible-n-parameter#task#8, 1 affected, status 400 -> 400):
  expected $.message = "The parameter cannot be converted to a numeric value: 1e99999999999"
  actual   $.message = "Number overflow. Attempting to store a number with magnitude larger than supported range"

(The key name, index name, function name and literal above are the recorded run's fill-ins for the message templates described in the issue; the templates, not these values, are the requirement.)

## Steps to Reproduce
1. Boot the emulator built from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1: replay ddbval-update-expression-conflict-arity-and-operand-type-rejected step task#4 — UpdateItem whose UpdateExpression references two overlapping document paths (a.b and a[0]) — and compare $.message.
3. Cause 2: replay ddbval-transact-write-items-update-function-context-ordering step task#4 — TransactWriteItems whose Update.UpdateExpression calls contains() — and compare $.message.
4. Cause 3: replay dynamodb-filter-expression-condition-function-as-operand-rejected step task#10 — Query whose FilterExpression uses attribute_exists() as an operand of a comparison — and compare $.message.
5. Cause 4: replay ddb-gsi-include-projection-scan-select-all-attributes-rejected step task#10 — Scan on an index with an ExclusiveStartKey whose index key is the wrong AttributeValue type — and compare $.message.
6. Cause 5: replay dynamodb-batch-execute-statement-and-execute-transaction-reject-unconvertible-n-parameter step task#8 — ExecuteTransaction with an N parameter that cannot be coerced — and compare $.message.
7. All five must pass before closing.
