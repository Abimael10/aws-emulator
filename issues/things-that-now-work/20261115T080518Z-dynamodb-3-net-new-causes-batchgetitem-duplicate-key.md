Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.request-validation-precedes-missing-table-lookup-rejected dynamodb.transact-write-items-nesting-violation-is-top-level-validation-not-cancellation dynamodb.describe-endpoints-returns-server-endpoints
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.request-validation-precedes-missing-table-lookup-rejected|check#18|AssertionError|assert (code, msg) == want, f"{label} on the missing table gave {code!r}: {msg!r}, expected {want!r}"
Issue-Key: dynamodb.transact-write-items-nesting-violation-is-top-level-validation-not-cancellation|run#19|AssertionError|assert message == _NESTING_MESSAGE, f'expected the exact ValidationException message, got {message!r}'
Issue-Key: dynamodb.describe-endpoints-returns-server-endpoints|run#7|AssertionError|assert matching_region, f"no endpoint address referenced region {ctx.region}: {endpoints1}"
Issue-Commit: 5d7ae394f187f060d35765f1ed4ed8891185905a

# dynamodb: 3 net-new causes — BatchGetItem duplicate-key validation loses to the table lookup, the TransactWriteItems nesting refusal is wrapped in the "1 validation error detected: " envelope, and DescribeEndpoints advertises a bare socket address instead of a service/region endpoint

> _Note: this issue was identified against the code base at commit `5d7ae394f187f060d35765f1ed4ed8891185905a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchGetItem, TransactWriteItems, DescribeEndpoints

## Description
Three net-new root causes in the dynamodb emulator, each grounded in one failing behavioral check. They are independent and must each be closed.

Cause 1 of 3 — BatchGetItem's duplicate-key request validation does not precede the table lookup.
AWS: DynamoDB validates the request shape before it resolves the named table, so a malformed request against a table that does not exist is refused as a ValidationException, and only a well-formed request against a missing table reaches ResourceNotFoundException (AWS docs, Error handling: "Programming.Errors" — validation errors are raised before the resource is resolved). Smithy: BatchGetItem.RequestItems is required, BatchGetRequestMap @length min 1 max 100, and the operation models ResourceNotFoundException (400) as its missing-resource error; the duplicate-key rule is not modelled, so the wording is the whole contract — AWS serves HTTP 400, code ValidationException, message "Provided list of item keys contains duplicates" (entirely fixed text, no request-derived parts). The same check confirms AWS's ordering on PutItem/DeleteItem Expected, GetItem ProjectionExpression, UpdateItem UpdateExpression and Scan FilterExpression, and the emulator already gets those right; BatchGetItem is the one arm that diverges.
Emulator: for RequestItems={<absent table>: {Keys: [k, k]}} it answers ResourceNotFoundException with message "Requested resource not found" — the table lookup runs first and the duplicate-key check never runs.
Why: a client gets the wrong error class for a programming defect in its own request. ResourceNotFoundException reads as "create the table / wait for ACTIVE" and is commonly retried or routed to provisioning logic, while ValidationException is terminal and must be fixed in the caller. The same defect also hides duplicate keys on tables that DO exist from any caller that only exercises this path, since the duplicate check is evidently downstream of resolution.
Fix: BatchGetItem's handler/validation ordering — the per-table duplicate-key check (and the request-shape checks generally) must run before the table is resolved, and must emit AWS's fixed sentence.
Traces: dynamodb.request-validation-precedes-missing-table-lookup-rejected.

Cause 2 of 3 — the nesting-depth refusal for TransactWriteItems Put items and Update SET values carries the "1 validation error detected: " envelope.
AWS: an item (or an UpdateExpression value) nested 33 levels deep is a top-level request validation failure for the whole TransactWriteItems call: HTTP 400, code ValidationException, message "Nesting Levels have exceeded supported limits: Attributes in the item have nested levels beyond supported limit" — all fixed text, with no leading envelope, and never a per-action TransactionCanceledException. Smithy: TransactWriteItems serves ValidationException/TransactionCanceledException and carries no message text, so the wording is the contract; AWS uses the "N validation error(s) detected: Value '<v>' at '<member>' failed to satisfy constraint: ..." envelope only for a modelled member-constraint violation, not for a service-level rule such as nesting depth.
Emulator: the code and the top-level classification are right (ValidationException for the whole call, neither action's effect lands), but the message is served as "1 validation error detected: Nesting Levels have exceeded supported limits: Attributes in the item have nested levels beyond supported limit".
Why: callers, CLIs and test suites surface this string verbatim and some branch on it; a message that is not AWS's text is a behavioural difference even when status and code match.
Fix: the nesting-depth check's message assembly on the TransactWriteItems path (Put Item and Update SET values) — emit the bare sentence, not the constraint-envelope builder's output. NOTE: open issue 20261111T170613Z-dynamodb-non-constraint-validationexception-messages-are-wrapped-in.md describes the same envelope defect for BatchWriteItem PutRequest and for a TransactWriteItems Put *condition value*, but it does not carry this check's replay key and its causes do not cover the Put-item / Update-SET arms, so this cause is filed separately; a fixer should expect the two to share a code path.
Traces: dynamodb.transact-write-items-nesting-violation-is-top-level-validation-not-cancellation.

Cause 3 of 3 — DescribeEndpoints advertises a bare host:port that names neither the service nor the caller's region.
AWS: DescribeEndpoints is a server-side, no-input operation. Smithy: output Endpoints is required, each Endpoint has required Address (String, documented "IP address of the endpoint") and required CachePeriodInMinutes (Long). Beyond that the model states no format, so this is behavioral-expectation only: AWS answers with the regional DynamoDB endpoint for the region the call was made in — an address of the form dynamodb.<region>.amazonaws.com (service token plus the caller's region) with a positive CachePeriodInMinutes — and the set is stable across an immediate repeat call.
Emulator: it answers one endpoint whose Address is a bare loopback socket address carrying no service token and no region (this run: '127.0.0.1:46813', CachePeriodInMinutes 1440), so no returned address references the caller's region. The specific host and port are environment values and are NOT the requirement; the requirement is that the advertised address identifies the dynamodb service and the region the call was addressed to.
Why: SDK endpoint discovery (BatchGetItem and friends carry clientDiscoveredEndpoint) caches the Address and re-targets subsequent calls at it; an address that does not identify the service or region cannot be region-validated by a caller, and a multi-region client cannot tell two regions' discovered endpoints apart.
Fix: the DescribeEndpoints handler's response construction — derive the advertised Address from the service's endpoint identity for the request's region rather than echoing the raw listening socket.
Traces: dynamodb.describe-endpoints-returns-server-endpoints.

## Evidence
Verbatim from the replay:

Cause 1 — dynamodb.request-validation-precedes-missing-table-lookup-rejected:
  AssertionError: BatchGetItem with a duplicated key on the missing table gave 'ResourceNotFoundException': 'Requested resource not found', expected ('ValidationException', 'Provided list of item keys contains duplicates')
  (the earlier arms of the same check — Expected EQ with no value, ProjectionExpression ''/'a..b'/'a, a.b', UpdateExpression ''/'SET'/reserved/undefined-name, FilterExpression 'a +' — all passed, as did every well-formed arm expecting ResourceNotFoundException, so only the BatchGetItem arm diverges.)

Cause 2 — dynamodb.transact-write-items-nesting-violation-is-top-level-validation-not-cancellation:
  AssertionError: expected the exact ValidationException message, got '1 validation error detected: Nesting Levels have exceeded supported limits: Attributes in the item have nested levels beyond supported limit'
  Expected message (fixed text, no variable parts): 'Nesting Levels have exceeded supported limits: Attributes in the item have nested levels beyond supported limit'. Code ValidationException and the top-level (non-cancellation) classification already matched.

Cause 3 — dynamodb.describe-endpoints-returns-server-endpoints:
  AssertionError: no endpoint address referenced region us-west-2: [{'Address': '127.0.0.1:46813', 'CachePeriodInMinutes': 1440}]
  Expected: at least one Endpoint whose Address names the dynamodb service and the request's region (form dynamodb.<region>.amazonaws.com). The literal host/port and the port number are this run's environment values, not requirements.

## Steps to Reproduce
Cause 1:
1. Pick a table name that does not exist (do not create it).
2. Call BatchGetItem with RequestItems={<that name>: {"Keys": [{"pk": {"S": "k"}}, {"pk": {"S": "k"}}]}} (the same key twice).
3. Expect HTTP 400 ValidationException 'Provided list of item keys contains duplicates'; the emulator answers ResourceNotFoundException 'Requested resource not found'.
4. Repeat with two distinct keys and confirm that arm is ResourceNotFoundException, and that DescribeTable on the name still answers 'Requested resource not found: Table: <name> not found'.

Cause 2:
1. CreateTable with KeySchema pk HASH (S), BillingMode PAY_PER_REQUEST; wait for ACTIVE.
2. PutItem {'pk': 'update-target', 'ctr': 1}.
3. Build a value nested 33 levels ({'M': {'n': ...}} wrapped 32 times around {'S': 'leaf'}).
4. TransactWriteItems with [Put of a new item carrying that value] + [a valid Update of 'update-target']; then again with [a valid Put of a new item] + [Update whose SET value is that deep value].
5. Both calls must be ValidationException with exactly 'Nesting Levels have exceeded supported limits: Attributes in the item have nested levels beyond supported limit' (no '1 validation error detected: ' prefix), and neither sibling action may land.

Cause 3:
1. Call DescribeEndpoints (no input) against the emulator with a region configured (e.g. us-west-2).
2. Expect at least one Endpoint whose Address names the service and that region (dynamodb.us-west-2.amazonaws.com) with a positive CachePeriodInMinutes; the emulator returns a bare host:port.
3. Call it again and confirm the address set is unchanged.
