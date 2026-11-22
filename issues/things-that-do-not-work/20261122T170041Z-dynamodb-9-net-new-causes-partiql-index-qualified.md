Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.execute-transaction-read-response-shapes-and-write-consumed-capacity dynamodb.execute-transaction-read-responses-three-way-shape-and-write-consumed-capacity dynamodb.partiql-select-gsi-order-by-sort-key-requires-hash-equality dynamodb.partiql-select-lsi-order-by-sort-key-requires-hash-equality dynamodb.execute-transaction-writes-maintain-gsi-and-lsi-entries dynamodb.execute-statement-insert-parameter-nesting-depth-32-levels-accepted-33-rejected dynamodb.partiql-insert-and-update-item-size-boundary-at-409600-bytes dynamodb.partiql-or-negated-sort-key-branch-runs-and-unions dynamodb.partiql-select-lsi-unprojected-filter-scoping-unkeyed-or-between dynamodb.partiql-set-add-three-argument-arity-rejected dynamodb.partiql-update-set-literal-and-set-add-reject-mixed-type-set-members dynamodb.request-validation-precedes-missing-table-lookup-rejected
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.execute-transaction-read-response-shapes-and-write-consumed-capacity|run#10|AssertionError|assert responses == [{"Item": {"attr": {"S": "v1"}}}, {"Item": {}}, {}], (
Issue-Key: dynamodb.execute-transaction-read-responses-three-way-shape-and-write-consumed-capacity|run#13|AssertionError|assert responses[1] == {"Item": {}}, (
Issue-Key: dynamodb|ExecuteStatement|ValidationException
Issue-Key: dynamodb.request-validation-precedes-missing-table-lookup-rejected|check#9|AssertionError|assert (code, msg) == want, f"{label} on the missing table gave {code!r}: {msg!r}, expected {want!r}"
Issue-Commit: 8896a828822ada8fe26405eeb13d1bd63d2626fa

# dynamodb: 9 net-new causes — PartiQL index-qualified ORDER BY, OR/NOT sort-key branches, set literals and at-limit writes are refused, ExecuteTransaction read shape and three refusal messages diverge

> _Note: this issue was identified against the code base at commit `8896a828822ada8fe26405eeb13d1bd63d2626fa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteStatement

## Description
Nine distinct net-new root causes in the DynamoDB PartiQL data plane (ExecuteStatement / ExecuteTransaction) and in legacy-Expected request validation. Each is numbered with its own failing check ids; all nine must be closed before this issue is submitted. None of them is covered by the open PartiQL issues 20261103T022510Z-dynamodb-data-plane-validation-gaps-partiql-semantics-number.md, 20261106T140930Z-dynamodb-partiql-malformed-statement-refusal-appends-an-emulator.md, 20261111T170613Z-dynamodb-partiql-batch-transaction-consumedcapacity-is-not-aggregated.md, 20261112T231149Z-dynamodb-reads-and-partiql-refusals-diverge-valid-projections.md, 20261113T061255Z-dynamodb-the-partiql-parser-s-refusal-detail-for.md or 20261118T105911Z-dynamodb-9-net-new-causes-a-disable-accepted.md, which cover other PartiQL behaviours and do not carry these replay keys.

=== Cause 1 of 9: ExecuteTransaction read Responses drop the `Item` key for an existing item that lacks the projected attribute ===
AWS: Smithy: ExecuteTransaction.output.Responses -> ItemResponseList of ItemResponse; ItemResponse has exactly one OPTIONAL member `Item` (AttributeMap). The presence or absence of that member is therefore the wire distinction between "the key exists" and "the key does not exist". AWS answers one Responses entry per TransactStatement, in order: `{"Item": {<projected attributes>}}` when the key exists and the projection matched, `{"Item": {}}` when the key exists but carries none of the projected attributes, and `{}` (no `Item` member at all) when the key does not exist. Recorded behavior of the suite's reference run confirms the three-way shape.
Emulator: collapses the middle case into the miss case. For a SELECT of one projected attribute over an item that exists without that attribute it returns `{}` instead of `{"Item": {}}`, so an existence probe cannot distinguish a sparse item from an absent key. Replay: `expected [hit, exists-without-attr, absent] shapes, got [{'Item': {'attr': {'S': 'v1'}}}, {}, {}]`.
Why: a client that uses a projecting transactional SELECT to test key existence (the standard read-transaction idiom) reads an existing row as missing, and will go on to INSERT over it or report it absent.
Fix: the ExecuteTransaction handler's read path / its ItemResponse serializer — emit the `Item` member whenever the key resolved, even when the projected attribute map is empty, and omit it only on a true miss.
Traces: dynamodb.execute-transaction-read-response-shapes-and-write-consumed-capacity, dynamodb.execute-transaction-read-responses-three-way-shape-and-write-consumed-capacity.

=== Cause 2 of 9: an index-qualified PartiQL SELECT with ORDER BY is refused outright, with emulator-authored wording ===
AWS: a SELECT whose FROM names an index (`SELECT * FROM "<table>"."<index>" WHERE <index hash key> = <v> ORDER BY <index sort key> ASC|DESC`) is accepted and returns the index partition in sort-key order. ORDER BY is refused only in two shapes, each with its own fixed text: with a WHERE that states no hash-key equality — ValidationException, HTTP 400, message `Must have at least one non-optional hash key condition in WHERE clause when using ORDER BY clause.` — and with no WHERE clause at all — ValidationException, HTTP 400, message `Must have WHERE clause in the statement when using ORDER BY clause.` Both messages are entirely fixed text with no variable parts.
Emulator: refuses the accepted form too. `SELECT * FROM "<table>"."gsi1" WHERE gpk = 'g1' ORDER BY gsk ASC` (gpk is the GSI's hash key) and `SELECT * FROM "<table>"."lsi1" WHERE pk = 'a' ORDER BY lsk ASC` (pk is both the table's and the LSI's hash key) are both answered ValidationException with the emulator-authored text `ORDER BY is supported only for a statement whose WHERE clause states an equality on the table's partition key`. That text is not AWS's for either refusal class, and in the LSI case the stated condition is actually satisfied by the request, so the refusal contradicts its own message. The same refusal blocks index-qualified ORDER BY reads used to verify transactional writes.
Why: every ordered index read through PartiQL fails. A client paginating a GSI or LSI in sort-key order gets a 400 for a statement AWS serves, and a client matching on the refusal text for the genuinely invalid forms never matches.
Fix: the PartiQL SELECT planner/validator — resolve ORDER BY against the key schema of the index named in the FROM qualifier (not only the base table's), and emit AWS's two fixed messages for the no-hash-key and no-WHERE classes.
Traces: dynamodb.partiql-select-gsi-order-by-sort-key-requires-hash-equality, dynamodb.partiql-select-lsi-order-by-sort-key-requires-hash-equality, dynamodb.execute-transaction-writes-maintain-gsi-and-lsi-entries.

=== Cause 3 of 9: a 32-level nested Map bound as a PartiQL INSERT parameter is refused, and the depth message carries an extra emulator detail ===
AWS: the documented item limit (https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/Limits.html) is 32 levels of attribute nesting, enforced identically on PutItem and on a PartiQL INSERT's Parameters. A document of 31 wrapping Maps around one scalar leaf is 32 levels and is STORED and round-trips whole. 33 levels is refused with ValidationException, HTTP 400, message exactly `Nesting Levels have exceeded supported limits` — fixed text, no variable part.
Emulator: refuses the 32-level document, i.e. the limit is enforced one level too low, and the message it returns is `Nesting Levels have exceeded supported limits: Attributes in the item have nested levels beyond supported limit` — AWS's fixed text with an emulator-authored `: ...` detail appended.
Why: documents at the documented maximum depth cannot be written through PartiQL at all, and a client comparing the refusal message against AWS's text never matches.
Fix: the PartiQL parameter-binding validation that counts nesting depth (off-by-one in the comparison) and the ValidationException message it raises.
Traces: dynamodb.execute-statement-insert-parameter-nesting-depth-32-levels-accepted-33-rejected.

=== Cause 4 of 9: a PartiQL UPDATE that grows an item to exactly the item-size limit is refused as over-size ===
AWS: the item-size quota (https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/ServiceQuotas.html#limits-items) is 400 KB = 409,600 bytes INCLUSIVE. A PartiQL UPDATE that grows an item to exactly 409,600 encoded bytes succeeds and the value reads back whole; only one byte past the limit is refused, with ValidationException, HTTP 400, message exactly `Item size to update has exceeded the maximum allowed size` (fixed text; the sibling INSERT path's wording is `Item size has exceeded the maximum allowed size`).
Emulator: refuses the exactly-at-limit UPDATE, returning that same over-limit ValidationException for an item of exactly 409,600 bytes — the boundary is treated as exclusive instead of inclusive.
Why: a client writing items sized to the documented maximum is refused for a request AWS accepts; the largest legal item is unreachable through PartiQL UPDATE.
Fix: the PartiQL UPDATE handler's item-size check (use `>` against 409600, not `>=`). This is a distinct path from the PutItem boundary already filed as 20261118T105911Z-dynamodb-a-putitem-sized-exactly-to-the-409.md, which does not cover the PartiQL UPDATE path.
Traces: dynamodb.partiql-insert-and-update-item-size-boundary-at-409600-bytes.

=== Cause 5 of 9: a WHERE ORing a negated sort-key branch beside another sort-key branch is refused as "overlapping" ===
AWS: a PartiQL WHERE clause may OR two branches that each constrain the same partition key and the same sort key, including negated sort-key forms (`NOT sk = <v>`, `NOT (sk = <v>)`, `sk <> <v>`, `NOT begins_with(sk, <v>)`, `NOT sk IN [...]`, `NOT sk > <v>`). The statement runs and returns the UNION of the branches, scoped to the partition keys the branches state (https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/ql-reference.select.html). Recorded behavior: `pk = 'p' AND NOT sk = '1' OR pk = 'p' AND sk = '2'` over sort keys 1, 2, 3 returns {2, 3}.
Emulator: refuses the statement with ValidationException, HTTP 400, message `Overlapping conditions with range keys are not supported in where clause` — a refusal AWS does not issue for this shape. The plain `pk = 'p' AND sk = '2'` positive control in the same run is served, so the parser works and only the OR-of-sort-key-branches shape is rejected.
Why: any client expressing a union or an exclusion over a sort key in one statement gets a 400 and must be rewritten into several round trips.
Fix: the PartiQL WHERE-clause key-condition analyser — treat OR branches as separate key conditions to be unioned rather than as one overlapping range condition, and keep the negated comparison forms.
Traces: dynamodb.partiql-or-negated-sort-key-branch-runs-and-unions.

=== Cause 6 of 9: an unkeyed PartiQL filter on an attribute an index does not project is refused instead of matching nothing ===
AWS: on a SELECT through a secondary index, a filter on an attribute the index does not project is refused ONLY when the index's own key condition sits in a top-level AND; with no key condition at all the filter is ACCEPTED and, because the index never carries that attribute, it reads as absent and matches nothing (an empty Items list). The refusal, where it applies, is ValidationException, HTTP 400, message `One or more parameter values were invalid: Secondary index <index-name> does not project one or more filter attributes: [<attribute names>]` — the fixed text around the index name taken from the FROM qualifier and the bracketed list taken from the request's unprojected filter attributes.
Emulator: refuses the unkeyed form as well. `SELECT * FROM "<table>"."lsi1" WHERE not_proj_attr = 'np-1'`, with no key condition anywhere, is answered that ValidationException, where AWS answers 200 with an empty Items list. The projected-attribute control in the same run is served, so the scan path itself works.
Why: a client scanning an index with an incidental filter on a base-table-only attribute gets a 400 instead of an empty result, and the refusal fires for a request shape AWS admits.
Fix: the PartiQL SELECT validator's unprojected-filter-attribute scoping — raise it only when the index's key condition is in a top-level AND, and otherwise evaluate the unprojected attribute as absent.
Traces: dynamodb.partiql-select-lsi-unprojected-filter-scoping-unkeyed-or-between.

=== Cause 7 of 9: set_add's wrong-arity refusal uses emulator-authored text instead of AWS's positional message ===
AWS: `set_add` takes exactly two arguments (https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/ql-reference.update.html). Called with three it is refused with ValidationException, HTTP 400, message `SET_ADD must have exactly two arguments at <line>:<column>:<length>` — the fixed text `SET_ADD must have exactly two arguments at ` followed by the offending token's position inside the submitted statement (line, column and token length; in the recorded run that position was 1:49:7, a value of that one statement and not part of the template).
Emulator: returns ValidationException with `Incorrect number of operands for operator or function; operator or function: set_add, number of operands: 3` — different fixed text, lower-cased function name, an operand count in place of the arity claim, and no statement position at all. The refusal itself is correct and the set is left unchanged; only the message diverges.
Why: a client that surfaces or parses the refusal to point at the offending token in the statement gets neither AWS's wording nor any position, so statement-level error reporting breaks.
Fix: the PartiQL function-arity validation in the UPDATE path and the ValidationException message it formats — emit AWS's text with the token's line:column:length.
Traces: dynamodb.partiql-set-add-three-argument-arity-rejected.

=== Cause 8 of 9: a set (bag) literal inside an INSERT ... VALUE is rejected as a malformed statement ===
AWS: the `<<...>>` bag literal is a PartiQL set literal and is valid inside an INSERT's VALUE map, e.g. `INSERT INTO "<table>" VALUE {'pk': ?, 'goodset': <<'x', 'y'>>}`, which stores a String Set attribute (https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/ql-reference.update.html). Later refusals in this area are about the bag's CONTENTS, not its syntax: a mixed-type bag literal is ValidationException `Unsupported data type in Bag. DynamoDB only supports either numbers or strings in bags`, and set_add mixing a member of another type is ValidationException `An operand in the update expression has an incorrect data type`.
Emulator: cannot parse the bag literal in that position at all — the seeding INSERT is answered ValidationException, HTTP 400, `Statement wasn't well formed, can't be processed: Unexpected token after expression`. The same `<<...>>` literal parses as a set_add argument in a sibling check, so the gap is specific to the bag literal inside an INSERT ... VALUE map.
Why: sets cannot be created through PartiQL INSERT at all, and the two content-type refusals above are unreachable, so a client validating set typing sees a parse error instead of the modelled refusal.
Fix: the PartiQL statement parser's VALUE/map-literal production — accept the `<<...>>` bag literal as a value, and let the set-member type rules produce the two ValidationException messages above.
Traces: dynamodb.partiql-update-set-literal-and-set-add-reject-mixed-type-set-members.

=== Cause 9 of 9: DeleteItem's legacy-Expected validation message omits the "1 validation error detected: " prefix PutItem emits ===
AWS: a legacy `Expected` entry naming `ComparisonOperator: EQ` with no `Value`/`AttributeValueList` is refused — before the table is even looked up, so it is ValidationException and not ResourceNotFoundException — with HTTP 400 and message `1 validation error detected: One or more parameter values were invalid: Value or AttributeValueList must be used with ComparisonOperator: <operator> for Attribute: <attribute name>`. The `1 validation error detected: ` prefix and the sentence are fixed text; the operator and the attribute name come from the request's Expected map. AWS uses the identical message for PutItem and DeleteItem.
Emulator: emits the prefixed message for PutItem but the unprefixed `One or more parameter values were invalid: Value or AttributeValueList must be used with ComparisonOperator: EQ for Attribute: a` for DeleteItem — the same defect on the same validation rule, inconsistent between the two operations.
Why: a client with one matcher for this validation failure matches PutItem and not DeleteItem, so the same malformed Expected is classified two different ways.
Fix: DeleteItem's legacy-Expected validation message formatting — route it through the same prefixed ValidationException builder PutItem uses.
Traces: dynamodb.request-validation-precedes-missing-table-lookup-rejected.

## Evidence
Replay output, verbatim per cause.

Cause 1 — dynamodb.execute-transaction-read-response-shapes-and-write-consumed-capacity:
  AssertionError: expected [hit, exists-without-attr, absent] shapes, got [{'Item': {'attr': {'S': 'v1'}}}, {}, {}]
  expected: [{'Item': {'attr': {'S': 'v1'}}}, {'Item': {}}, {}]
Cause 1 — dynamodb.execute-transaction-read-responses-three-way-shape-and-write-consumed-capacity:
  AssertionError: expected an existing item lacking the projected attribute to answer an empty Item, got {}

Cause 2 — dynamodb.partiql-select-gsi-order-by-sort-key-requires-hash-equality, on `SELECT * FROM "<table>"."gsi1" WHERE gpk = 'g1' ORDER BY gsk ASC`:
  ClientError: An error occurred (ValidationException) when calling the ExecuteStatement operation: ORDER BY is supported only for a statement whose WHERE clause states an equality on the table's partition key
  expected: 200 with items ordered gsk 10, 20, 30; the ONLY refusals AWS issues here are ValidationException 'Must have at least one non-optional hash key condition in WHERE clause when using ORDER BY clause.' (no hash-key equality) and ValidationException 'Must have WHERE clause in the statement when using ORDER BY clause.' (no WHERE).
Cause 2 — dynamodb.partiql-select-lsi-order-by-sort-key-requires-hash-equality, on `SELECT * FROM "<table>"."lsi1" WHERE pk = 'a' ORDER BY lsk ASC` (pk IS the table's partition key): same ValidationException text.
Cause 2 — dynamodb.execute-transaction-writes-maintain-gsi-and-lsi-entries, on `SELECT * FROM "<table>"."by-l" WHERE pk = ? ORDER BY lsk`: same ValidationException text.

Cause 3 — dynamodb.execute-statement-insert-parameter-nesting-depth-32-levels-accepted-33-rejected, on an INSERT whose parameter is 31 wrapping Maps around one scalar leaf (32 levels):
  ClientError: An error occurred (ValidationException) when calling the ExecuteStatement operation: Nesting Levels have exceeded supported limits: Attributes in the item have nested levels beyond supported limit
  expected: the 32-level INSERT succeeds and round-trips; only 33 levels is refused, with message exactly 'Nesting Levels have exceeded supported limits'.

Cause 4 — dynamodb.partiql-insert-and-update-item-size-boundary-at-409600-bytes, on `UPDATE "<table>" SET bigattr = ? WHERE pk = ?` growing the item to exactly 409600 bytes:
  ClientError: An error occurred (ValidationException) when calling the ExecuteStatement operation: Item size to update has exceeded the maximum allowed size
  expected: 200, the value reads back whole; that ValidationException belongs to 409601 bytes.

Cause 5 — dynamodb.partiql-or-negated-sort-key-branch-runs-and-unions, on `SELECT * FROM "<table>" WHERE pk = 'p' AND NOT sk = '1' OR pk = 'p' AND sk = '2'`:
  ClientError: An error occurred (ValidationException) when calling the ExecuteStatement operation: Overlapping conditions with range keys are not supported in where clause
  expected: 200 with the union of the branches (sort keys 2 and 3). The plain `pk = 'p' AND sk = '2'` control in the same run was served.

Cause 6 — dynamodb.partiql-select-lsi-unprojected-filter-scoping-unkeyed-or-between, on `SELECT * FROM "<table>"."lsi1" WHERE not_proj_attr = 'np-1'` with no key condition:
  ClientError: An error occurred (ValidationException) when calling the ExecuteStatement operation: One or more parameter values were invalid: Secondary index lsi1 does not project one or more filter attributes: [not_proj_attr]
  expected: 200 with an empty Items list; that refusal belongs only to the form where the index key condition sits in a top-level AND.

Cause 7 — dynamodb.partiql-set-add-three-argument-arity-rejected, on `UPDATE "<table>" SET tags = set_add(tags, <<'b'>>, <<'c'>>) WHERE pk = ?`:
  AssertionError: expected ('ValidationException', 'SET_ADD must have exactly two arguments at 1:49:7'), got ('ValidationException', 'Incorrect number of operands for operator or function; operator or function: set_add, number of operands: 3')
  (1:49:7 is this statement's token position, a variable part of the template.)

Cause 8 — dynamodb.partiql-update-set-literal-and-set-add-reject-mixed-type-set-members, on `INSERT INTO "<table>" VALUE {'pk': ?, 'goodset': <<'x', 'y'>>}`:
  ClientError: An error occurred (ValidationException) when calling the ExecuteStatement operation: Statement wasn't well formed, can't be processed: Unexpected token after expression
  expected: 200, a String Set {'x','y'} stored.

Cause 9 — dynamodb.request-validation-precedes-missing-table-lookup-rejected:
  AssertionError: DeleteItem with Expected EQ and no value on the missing table gave 'ValidationException': 'One or more parameter values were invalid: Value or AttributeValueList must be used with ComparisonOperator: EQ for Attribute: a', expected ('ValidationException', '1 validation error detected: One or more parameter values were invalid: Value or AttributeValueList must be used with ComparisonOperator: EQ for Attribute: a')
  The identical PutItem assertion earlier in the same check passed, so only DeleteItem loses the prefix.

## Steps to Reproduce
Boot the emulator and run the behavioral suite; the ids below are the failing checks.

Cause 1: create a table with a HASH+RANGE key schema; PutItem one item with an extra attribute `attr` and one item without it; call ExecuteTransaction with three SELECTs projecting `attr` — one for the item with it, one for the item without it, one for a key that does not exist. Inspect `Responses`: entry 2 must be `{"Item": {}}`, entry 3 must be `{}`.
  Replay: dynamodb.execute-transaction-read-response-shapes-and-write-consumed-capacity, dynamodb.execute-transaction-read-responses-three-way-shape-and-write-consumed-capacity

Cause 2: create a table with a GSI (hash gpk, range gsk) and an LSI (hash pk, range lsk), write a few items, then ExecuteStatement `SELECT * FROM "<table>"."<index>" WHERE <index hash key> = <v> ORDER BY <index sort key> ASC`. Then repeat with a non-key filter instead of the hash equality, and with no WHERE at all, and compare the refusal messages.
  Replay: dynamodb.partiql-select-gsi-order-by-sort-key-requires-hash-equality, dynamodb.partiql-select-lsi-order-by-sort-key-requires-hash-equality, dynamodb.execute-transaction-writes-maintain-gsi-and-lsi-entries

Cause 3: ExecuteStatement `INSERT INTO "<table>" VALUE {'pk':?, 'v':?}` binding a parameter built as 31 nested `{"M": {"k": ...}}` wrappers around `{"S": "leaf"}` (32 levels) — must succeed; then 32 wrappers (33 levels) — must be refused with exactly `Nesting Levels have exceeded supported limits`.
  Replay: dynamodb.execute-statement-insert-parameter-nesting-depth-32-levels-accepted-33-rejected

Cause 4: PutItem a bare key, then ExecuteStatement `UPDATE "<table>" SET bigattr = ? WHERE pk = ?` with a filler string sized so the encoded item is exactly 409600 bytes — must succeed; one byte more must be refused.
  Replay: dynamodb.partiql-insert-and-update-item-size-boundary-at-409600-bytes

Cause 5: write sort keys '1','2','3' under one partition key, then ExecuteStatement `SELECT * FROM "<table>" WHERE pk = 'p' AND NOT sk = '1' OR pk = 'p' AND sk = '2'` (and the `NOT (sk = ...)`, `sk <> ...`, `NOT begins_with(...)`, `NOT sk IN [...]`, `NOT sk > ...` variants) — each must run and return the union.
  Replay: dynamodb.partiql-or-negated-sort-key-branch-runs-and-unions

Cause 6: create a table with an LSI whose Projection is INCLUDE of one attribute only; write rows carrying both that attribute and an unprojected one; ExecuteStatement `SELECT * FROM "<table>"."<lsi>" WHERE <unprojected attr> = '<v>'` with no key condition — must return an empty Items list, not a ValidationException.
  Replay: dynamodb.partiql-select-lsi-unprojected-filter-scoping-unkeyed-or-between

Cause 7: PutItem an item with a String Set attribute, then ExecuteStatement `UPDATE "<table>" SET tags = set_add(tags, <<'b'>>, <<'c'>>) WHERE pk = ?` and compare the ValidationException message against `SET_ADD must have exactly two arguments at <line>:<col>:<len>`.
  Replay: dynamodb.partiql-set-add-three-argument-arity-rejected

Cause 8: ExecuteStatement `INSERT INTO "<table>" VALUE {'pk': ?, 'goodset': <<'x', 'y'>>}` — must store a String Set; then `UPDATE "<table>" SET goodset = <<1, 'z'>> WHERE pk = ?` and `UPDATE "<table>" SET goodset = set_add(goodset, <<5>>) WHERE pk = ?` for the two content-type refusals.
  Replay: dynamodb.partiql-update-set-literal-and-set-add-reject-mixed-type-set-members

Cause 9: against a table name that does not exist, call DeleteItem with Key and `Expected={"a": {"ComparisonOperator": "EQ"}}` (no Value) and compare the ValidationException message with the one PutItem returns for the same Expected map — both must carry the `1 validation error detected: ` prefix.
  Replay: dynamodb.request-validation-precedes-missing-table-lookup-rejected
