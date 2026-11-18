Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.contributor-insights-disable-while-enabling-rejected dynamodb.gsi-multi-attribute-hash-key-query-without-every-attribute-rejected dynamodb.partiql-select-index-qualified-capacity-table-and-index-arms dynamodb.partiql-select-keys-only-gsi-and-projected-attribute-list-lsi-charge-no-table-arm dynamodb.partiql-write-through-index-qualifier-rejected dynamodb.scan-filter-contains-document-typed-operand-rejected dynamodb.set-arithmetic-grouping-size-utf16-and-add-empty-string-to-set dynamodb.update-expression-malformed-syntax-rejected dynamodb.update-item-addition-and-if-not-exists-overhead-ignores-growth-and-path-absence dynamodb.vvd-key-schema-attribute-without-definition-rejected
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.contributor-insights-disable-while-enabling-rejected|run#9|AssertionError|assert refused, f"DISABLE was never refused while ENABLING in {_ATTEMPTS} attempts: {tried!r}"
Issue-Key: dynamodb.gsi-multi-attribute-hash-key-query-without-every-attribute-rejected|run#29|AssertionError|assert msg == "HASH key attributes region must have equality conditions specified in the query", (
Issue-Key: dynamodb.partiql-select-index-qualified-capacity-table-and-index-arms|run#11|AssertionError|assert cc['GlobalSecondaryIndexes'][_GSI]['CapacityUnits'] == _INDEX_RCU_EVENTUAL, (
Issue-Key: dynamodb.partiql-select-keys-only-gsi-and-projected-attribute-list-lsi-charge-no-table-arm|run#13|AssertionError|assert cc['GlobalSecondaryIndexes'][_GSI]['CapacityUnits'] == _INDEX_RCU_EVENTUAL, (
Issue-Key: dynamodb.partiql-write-through-index-qualifier-rejected|run#22|AssertionError|assert err.get('Message') == expected, f"expected message {expected!r}, got {err.get('Message')!r}"
Issue-Key: dynamodb|Scan|ValidationException
Issue-Key: dynamodb.set-arithmetic-grouping-size-utf16-and-add-empty-string-to-set|run#13|AssertionError|assert matched_sks == {'one-astral-char', 'two-ascii-chars'}, (
Issue-Key: dynamodb.update-expression-malformed-syntax-rejected|_error#21|AssertionError|raise AssertionError(f"real AWS rejects this UpdateExpression, but it SUCCEEDED and returned {result!r}")
Issue-Key: dynamodb.update-item-addition-and-if-not-exists-overhead-ignores-growth-and-path-absence|run#11|AssertionError|assert lo_add_no_growth == lo_add_growth, (
Issue-Key: dynamodb.vvd-key-schema-attribute-without-definition-rejected|run#8|AssertionError|assert message == expected_msg, f"expected message {expected_msg!r}, got {message!r}"
Issue-Commit: 2d1d2911b5095265487653cd392f1f2d36a8c5ca

# dynamodb: 9 net-new causes — a DISABLE accepted mid-ENABLING, multi-HASH GSI/PartiQL/CreateTable refusal wording, a PartiQL index arm rounded per row, a CONTAINS operand wrongly refused, size() counting code points, redundant parentheses accepted and an arithmetic size ceiling that charges digit growth

> _Note: this issue was identified against the code base at commit `2d1d2911b5095265487653cd392f1f2d36a8c5ca`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Query, ExecuteStatement, Scan, UpdateItem, CreateTable, UpdateContributorInsights

## Description
Nine net-new root causes in the dynamodb emulator, each grounded in the failing behavioral checks named under its own Traces line. Nine causes, numbered below; all nine must be closed before this issue is submitted.

Cause 1 of 9: UpdateContributorInsights accepts DISABLE while the insights are still ENABLING.
AWS: Smithy: UpdateContributorInsights takes a required TableName and a required ContributorInsightsAction enum and answers a ContributorInsightsStatus enum; the model binds only InternalServerError and ResourceNotFoundException to the operation, so the refusal class here is recorded behavior only — HTTP 400 ValidationException with the message "Table or Index is not in a valid state to update Key Access Insights: ContributorInsights Status must be ENABLED or FAILED to perform DISABLE operation." That message is entirely fixed text; it carries no value from the request or from state. A DISABLE sent while DescribeContributorInsights still reports ENABLING is refused with it, the request has no effect, and the insights go on to settle ENABLED.
Emulator: the DISABLE is accepted in that window on every attempt (the check observed the ENABLING status and then an accepted DISABLE five times out of five) and the insights settle DISABLED.
Why: a client that enables insights and then (on a retry, or from a second actor) disables them reaches a terminal state AWS never reaches, and code that relies on the refusal to serialise insight changes never sees it, so the divergence is silent.
Fix: the UpdateContributorInsights handler's control-plane state guard — DISABLE is admissible only from ENABLED or FAILED, and the refusal must carry the message above. The open issue 20261115T010844Z-cloudwatch-dynamodb-updatecontributorinsights-with-contributorinsightsaction-disable-answers-contributorinsightsstatus.md is filed under a different service heading and is about the status a DISABLE answers, not about this state guard.
Traces: dynamodb.contributor-insights-disable-while-enabling-rejected

Cause 2 of 9: a Query through a GSI keyed on two HASH attributes reports the single-key refusal instead of the multi-attribute one.
AWS: Smithy: Query takes IndexName and KeyConditionExpression; a GSI whose KeySchema carries more than one HASH element answers a Query only when EVERY HASH attribute carries an equality condition. Omitting one is HTTP 400 ValidationException with the message "HASH key attributes <names> must have equality conditions specified in the query": the fixed text is "HASH key attributes " ... " must have equality conditions specified in the query", and <names> are the index key-schema HASH attributes the request's KeyConditionExpression left without an equality term (so the same index yields the other attribute's name when the other half is supplied).
Emulator: answers the generic single-key wording "Query condition missed key schema element: region".
Why: a client cannot tell a multi-attribute key schema's rule from a plain missing-key-element error; the refusal names the wrong rule and the message text a client matches on differs.
Fix: Query's key-condition validation, in the branch that resolves an index key schema (multiple HASH elements) before it falls back to the single-element message.
Traces: dynamodb.gsi-multi-attribute-hash-key-query-without-every-attribute-rejected

Cause 3 of 9: a PartiQL SELECT served from a secondary index rounds the index arm per row instead of once over the page.
AWS: recorded behavior only for the PartiQL path (the model carries ConsumedCapacity on ExecuteStatementOutput but no sizing rule); the DynamoDB capacity rule is that a read served from an index rounds the SUMMED size of the rows it read up to the 4 KB read unit ONCE — 1.0 RCU strongly consistent, 0.5 RCU eventually consistent. For a page of three items each far below 4 KB, the index arm is 0.5 (eventual) or 1.0 (strong), and the aggregate CapacityUnits equals it when nothing is fetched back from the base table.
Emulator: the GSI arm reports 1.5 ({'ReadCapacityUnits': 1.5, 'CapacityUnits': 1.5}) for the three-row page — 0.5 charged per row — on both an ALL-projection GSI and a KEYS_ONLY GSI.
Why: capacity figures are the contract a client uses to size on-demand cost and provisioned throughput; a per-row rounding inflates every index-served SELECT in proportion to the page size.
Fix: ExecuteStatement's SELECT capacity accounting — the index arm must round once over the page, the way the native Query reader does. The open issue 20261118T031500Z-dynamodb-a-secondary-index-table-never-reaches-active-and-updateitem-clause-overhead.md states this as its cause 4, but names only one of the two checks and its recorded replay key no longer matches either failure (both now fail at the GlobalSecondaryIndexes arm assertion), so the pair is re-grounded here.
Traces: dynamodb.partiql-select-index-qualified-capacity-table-and-index-arms, dynamodb.partiql-select-keys-only-gsi-and-projected-attribute-list-lsi-charge-no-table-arm

Cause 4 of 9: a PartiQL UPDATE or DELETE addressed at "table"."index" is refused with the parser's FROM-clause message instead of the index refusal.
AWS: recorded behavior only. UPDATE and DELETE whose target is an index qualifier are HTTP 400 ValidationException "This operation is not supported on an index" — wholly fixed text, no request values. INSERT INTO "table"."index" is refused differently, with the parse-position message "FROM clause may only contain a single table name in data manipulation statements at 1:13:<column>", whose only variable part is the statement column at which the qualifier ends.
Emulator: the UPDATE is refused with the INSERT/parser wording — "FROM clause may only contain a single table name in data manipulation statements at 1:13:29" (the 29 is that run's table-name length, not part of the requirement).
Why: the two refusals mean different things — one says the statement did not parse, the other says the operation is unsupported on an index. A client (or an ORM) that distinguishes a syntax defect from an unsupported target mis-classifies the failure.
Fix: ExecuteStatement's statement analysis — an index qualifier on a DML UPDATE/DELETE must be refused by the semantic layer with the index message, leaving the parser's FROM-clause message to INSERT. The open issue 20261111T170613Z-dynamodb-partiql-semantic-validation-index-qualified-select-update.md covers index-qualified SELECT/UPDATE semantics but not this refusal's class or text.
Traces: dynamodb.partiql-write-through-index-qualifier-rejected

Cause 5 of 9: legacy ScanFilter CONTAINS refuses a BOOL or NULL operand AWS accepts.
AWS: Smithy: Scan.ScanFilter is a FilterConditionMap of Condition (ComparisonOperator + AttributeValueList). CONTAINS refuses a document-typed operand — an M or an L entry — with HTTP 400 ValidationException "One or more parameter values were invalid: ComparisonOperator CONTAINS is not valid for <type> AttributeValue type", where the fixed text surrounds the operand's own type tag (M or L). A BOOL or NULL operand is ACCEPTED: the Scan answers HTTP 200 and matches nothing against a string-set attribute, and a matching S operand still selects its item.
Emulator: the BOOL operand is refused with the same document-type wording, "One or more parameter values were invalid: ComparisonOperator CONTAINS is not valid for BOOL AttributeValue type", so a 200 becomes a 400.
Why: a legacy client filtering on a scalar operand gets a hard 400 where AWS returns an empty page; the refusal is over-broad and breaks reads that work against the service.
Fix: Scan's legacy ScanFilter operand-type validation — scope the CONTAINS document-type refusal to M and L only.
Traces: dynamodb.scan-filter-contains-document-typed-operand-rejected

Cause 6 of 9: size() counts Unicode code points instead of UTF-16 code units.
AWS: size() applied to a String in a FilterExpression counts UTF-16 code units, so one astral-plane character (a surrogate pair) counts 2 — the same as two ASCII characters — and a three-character ASCII string does not match size(attr) = 2.
Emulator: the Query with FilterExpression 'size(txt) = :n', :n = 2, matched only the two-ASCII-character row; the single astral-plane character was counted as 1.
Why: any filter or condition expression gating on string length silently selects a different row set for non-BMP text, so correctness depends on the characters stored.
Fix: the size() implementation in the expression evaluator's string-length rule (shared by FilterExpression and ConditionExpression). The owned issue 20261117T172826Z-dynamodb-expression-evaluation-10-net-new-causes-transaction.md carries the bytes-vs-UTF-16 variant (dynamodb.scan-filter-size-counts-utf16-code-units-not-bytes); the surrogate-pair half is net-new here.
Traces: dynamodb.set-arithmetic-grouping-size-utf16-and-add-empty-string-to-set

Cause 7 of 9: an UpdateExpression with redundant parentheses is accepted and applied.
AWS: an UpdateExpression may parenthesise an operand pair ("SET n = (:a + :b)" is accepted), but doubled parentheses around a single operand are a grammar defect: "SET n = ((:v))" is refused HTTP 400 ValidationException "1 validation error detected: Invalid UpdateExpression: The expression has redundant parentheses;" — fixed text, including the trailing semicolon, with no request values — and the item is left untouched.
Emulator: UpdateItem answers HTTP 200 and performs the update (the check observed the 200 with an empty body).
Why: an emulator accepting an expression AWS refuses lets a defective expression pass in development and fail only in production, and the mutation it performs is one AWS never performs.
Fix: the UpdateExpression parser/validator in UpdateItem's expression validation — reject redundant parentheses before any mutation.
Traces: dynamodb.update-expression-malformed-syntax-rejected

Cause 8 of 9: UpdateItem's item-size ceiling charges an arithmetic clause for its RESULT's digit growth.
AWS: the size an UpdateItem is checked against (409,600 bytes) charges an arithmetic SET clause for its OPERANDS' own size, not for the width of the result, so "SET v = :v, n = :x + :y" admits the same maximum :v length whether the two 2-digit operands sum to a 2-digit result (49 + 49) or to a 3-digit one (99 + 99). One byte over is refused HTTP 400 ValidationException "Item size to update has exceeded the maximum allowed size" and nothing is written.
Emulator: the two ceilings differ by one byte — the growing addition accepts one byte less (this run bisected 409505 for 49 + 49 against 409504 for 99 + 99; the figures are this run's, the equality of the two ceilings is the requirement).
Why: a client sizing a write against the documented item ceiling has a write refused (or accepted) depending on the arithmetic result rather than on what it sent, which is not reproducible from its own request.
Fix: UpdateItem's size-gate accounting for arithmetic SET clauses. The open issue 20261118T031500Z-dynamodb-a-secondary-index-table-never-reaches-active-and-updateitem-clause-overhead.md states this as its cause 2 and quotes the same pair of ceilings from an earlier run, but the check's recorded replay key no longer matches the current failure point, so it is re-grounded here.
Traces: dynamodb.update-item-addition-and-if-not-exists-overhead-ignores-growth-and-path-absence

Cause 9 of 9: CreateTable's key-schema-without-definition refusal uses emulator-authored text that names neither set.
AWS: Smithy: CreateTable takes AttributeDefinitions and KeySchema; a KeySchema element with no matching AttributeDefinitions entry is HTTP 400 ValidationException "One or more parameter values were invalid: Some index key attributes are not defined in AttributeDefinitions. Keys: [<key schema attribute names>], AttributeDefinitions: [<attribute definition names>]". The fixed text is "One or more parameter values were invalid: Some index key attributes are not defined in AttributeDefinitions. Keys: [...], AttributeDefinitions: [...]"; both bracketed lists are taken from the request (in this recording, KeySchema PK/HASH + SK/RANGE and definitions PK/EXTRA render as "Keys: [SK, PK], AttributeDefinitions: [EXTRA, PK]"), and nothing is created — DescribeTable then answers ResourceNotFoundException "Requested resource not found: Table: <name> not found".
Emulator: answers ValidationException with "Invalid KeySchema: Some index key attribute have no definition" — neither list, and different fixed text, so the refusal still cannot tell a caller which name is unmatched. The refusal itself and the absence of the table are correct.
Why: the message is the only place AWS reports WHICH key attribute lacks a definition; a tool or template renderer that surfaces it loses the diagnosis, and a client matching the text does not match.
Fix: CreateTable's key-schema validation message (the same validator the fixed 20261108T051530Z-dynamodb-8-net-new-createtable-putitem-validation-causes.md touched; this same-length-but-mismatched-sets shape still answers emulator-authored text).
Traces: dynamodb.vvd-key-schema-attribute-without-definition-rejected

## Evidence
Expected vs actual, verbatim from this replay (names, ids, byte figures and statement columns below are this run's; the rules and the fixed message text are the requirement):
- Cause 1: 'AssertionError: DISABLE was never refused while ENABLING in 5 attempts: ["DISABLE accepted after the status read \'ENABLING\'; settled \'DISABLED\'", ... x5]'. Expected: ValidationException 'Table or Index is not in a valid state to update Key Access Insights: ContributorInsights Status must be ENABLED or FAILED to perform DISABLE operation.' and the insights settling ENABLED.
- Cause 2: "AssertionError: querying with only 'tenant': message 'Query condition missed key schema element: region'". Expected: 'HASH key attributes region must have equality conditions specified in the query' (and symmetrically '... tenant ...' when only region is supplied).
- Cause 3: "AssertionError: the GSI arm must carry the 0.5 RCU read, got {'gsi1': {'ReadCapacityUnits': 1.5, 'CapacityUnits': 1.5}}" and "... got {'gsi-keys-only': {'ReadCapacityUnits': 1.5, 'CapacityUnits': 1.5}}". Expected 0.5 eventually consistent / 1.0 strongly consistent, once over the page.
- Cause 4: "AssertionError: expected message 'This operation is not supported on an index', got 'FROM clause may only contain a single table name in data manipulation statements at 1:13:29'" for UPDATE "<table>"."gsi1" SET ....
- Cause 5: 'botocore.exceptions.ClientError: An error occurred (ValidationException) when calling the Scan operation: One or more parameter values were invalid: ComparisonOperator CONTAINS is not valid for BOOL AttributeValue type' — the call must be ACCEPTED (HTTP 200, Count 0). The M and L refusals are already correct.
- Cause 6: "AssertionError: size(txt) = 2 matched {'two-ascii-chars'}, expected {'one-astral-char', 'two-ascii-chars'}".
- Cause 7: 'AssertionError: real AWS rejects this UpdateExpression, but it SUCCEEDED and returned {... "HTTPStatusCode": 200 ...}' for UpdateExpression 'SET n = ((:v))'. Expected ValidationException '1 validation error detected: Invalid UpdateExpression: The expression has redundant parentheses;' with the item unchanged.
- Cause 8: 'AssertionError: "49 + 49" (no digit growth) accepted up to 409505 bytes but "99 + 99" (one extra digit in the result) accepted up to 409504'. The two ceilings must be equal.
- Cause 9: "AssertionError: expected message 'One or more parameter values were invalid: Some index key attributes are not defined in AttributeDefinitions. Keys: [SK, PK], AttributeDefinitions: [EXTRA, PK]', got 'Invalid KeySchema: Some index key attribute have no definition'".

## Steps to Reproduce
1. Build and boot the emulator, then replay the behavioral suite (run_behavioral_suite).
2. Re-run the check ids listed in trace_ids; each cause's Traces line names the checks that reproduce it.
3. Cause 1: CreateTable (PAY_PER_REQUEST), wait ACTIVE, UpdateContributorInsights ENABLE, read DescribeContributorInsights until it reports ENABLING, then UpdateContributorInsights DISABLE — AWS refuses with ValidationException and the insights settle ENABLED.
4. Cause 2: CreateTable with a GSI whose KeySchema has two HASH elements (tenant, region) plus a RANGE element, then Query that index with an equality on only one of them.
5. Cause 3: ExecuteStatement 'SELECT * FROM "<table>"."<gsi>" WHERE <gsi hash> = ?' with ReturnConsumedCapacity='INDEXES' over three sub-4 KB rows and compare the GlobalSecondaryIndexes arm.
6. Cause 4: ExecuteStatement 'UPDATE "<table>"."<gsi>" SET extra=...' and 'DELETE FROM "<table>"."<gsi>" WHERE ...' and compare the message with the INSERT form's parser message.
7. Cause 5: Scan with ScanFilter {attr: {ComparisonOperator: CONTAINS, AttributeValueList: [{'BOOL': true}]}} (and {'NULL': true}) against a string-set attribute — both are accepted by AWS and match nothing.
8. Cause 6: Query with FilterExpression 'size(txt) = :n', :n = 2, over rows holding a single astral-plane character, 'ab' and 'abc'.
9. Cause 7: UpdateItem with UpdateExpression 'SET n = ((:v))'.
10. Cause 8: bisect the largest accepted filler length for 'SET v = :v, n = :x + :y' with (49, 49) and with (99, 99) and compare the two boundaries.
11. Cause 9: CreateTable with AttributeDefinitions [PK, EXTRA] and KeySchema [PK HASH, SK RANGE] and compare the ValidationException message.
