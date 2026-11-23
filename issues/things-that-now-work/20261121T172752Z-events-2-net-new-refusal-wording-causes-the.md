Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events.list-event-buses-paginates-with-limit-and-next-token events.list-targets-by-rule-paginates-with-limit-and-next-token events.update-connection-malformed-kms-key-identifier-rejected
Issue-Shape-Id: com.amazonaws.eventbridge#AWSEvents
Issue-Key: events.list-event-buses-paginates-with-limit-and-next-token|run#14|AssertionError|assert (code, message) == ("ValidationException", "Parameter NextToken is not valid."), (
Issue-Key: events.list-targets-by-rule-paginates-with-limit-and-next-token|run#25|AssertionError|assert message == "Parameter NextToken is not valid.", (
Issue-Key: events.update-connection-malformed-kms-key-identifier-rejected|run#11|AssertionError|assert (code, message) == (
Issue-Commit: 75e93429d3ea083e11be0578db4814848b1cd3ff

# events: 2 net-new refusal-wording causes — the invalid-NextToken message carries an extra "Reason: The request is malformed." clause, and a @pattern violation quotes the pattern with its ^/$ anchors stripped

> _Note: this issue was identified against the code base at commit `75e93429d3ea083e11be0578db4814848b1cd3ff`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListEventBuses / ListTargetsByRule / UpdateConnection

## Description
TWO (2) net-new root causes, both grounded in `events` behavioral checks that the replay report marks neither ALREADY FILED nor OWNED. Open issues cover neighbouring ground but none carries these checks' replay keys (e.g. 20261116T135019Z-events-describeeventbus-does-not-refuse-an-aws-namespaced.md for event-bus name admission, 20261104T120007Z-events-updateconnection-s-authparameters-without-authorizationtype-refusal-uses.md for a DIFFERENT UpdateConnection refusal — its member is AuthParameters/AuthorizationType, not KmsKeyIdentifier, and 20261109T171600Z-events-eventbridge-7-net-new-causes-global-endpoint-putevents.md for global endpoints/PutTargets), so none of them covers these failures.

=== Cause 1 of 2: a NextToken the service never issued is refused with AWS's sentence plus an emulator-authored "Reason:" clause ===
AWS: Smithy (eventbridge, com.amazonaws.eventbridge#AWSEvents): ListEventBuses input NextToken -> NextToken (string, @length min 1 max 2048), Limit -> LimitMax100 (@range 1..100); ListTargetsByRule input is the same NextToken/Limit pair. Neither operation models ValidationException (ListEventBuses models only InternalException; ListTargetsByRule InternalException + ResourceNotFoundException), so the refusal class is recorded behavior only: a syntactically acceptable NextToken that the service never issued is refused with HTTP 400, Error.Code "ValidationException" and Error.Message "Parameter NextToken is not valid." The whole message is FIXED text — AWS interpolates nothing into it, neither the member name nor the submitted token — so no part of it is a per-run or per-request value.
Emulator: the status and the error code are right, but the message is "Parameter NextToken is not valid. Reason: The request is malformed." — AWS's sentence with an extra emulator-authored clause appended. The same appended clause appears on both operations, so it comes from one shared refusal path, not from either handler separately.
Why: a client (or an operator's log-based classification) that recognises an expired/forged pagination token by its exact refusal sentence no longer recognises it, and the added clause asserts the wrong diagnosis: it tells the caller the request was malformed, when the token is well formed and merely unknown to the service, so the caller "fixes" its serialization instead of restarting the pagination. Any parity test that pins EventBridge's pagination-token refusal fails.
Fix: the shared invalid-pagination-token refusal used by the events list handlers — the NextToken decode/validation step that constructs the ValidationException must emit AWS's sentence alone, with no "Reason: ..." suffix. The same path serves ListEventBuses and ListTargetsByRule (and should be checked for the other Limit/NextToken list operations).
Traces: events.list-event-buses-paginates-with-limit-and-next-token, events.list-targets-by-rule-paginates-with-limit-and-next-token.

=== Cause 2 of 2: UpdateConnection's KmsKeyIdentifier pattern-violation message quotes the modeled regex with its anchors removed ===
AWS: Smithy UpdateConnection input member KmsKeyIdentifier -> KmsKeyIdentifier (@length min 0 max 2048, @pattern "^[a-zA-Z0-9_\\-/:]*$"). A value that violates that pattern is refused with HTTP 400 and Error.Code "ValidationException", using AWS's standard single-constraint framing: "1 validation error detected: Value '<submitted value>' at '<wire member name>' failed to satisfy constraint: Member must satisfy regular expression pattern: <the modeled pattern>". The fixed text is "1 validation error detected: Value '", "' at '", "' failed to satisfy constraint: Member must satisfy regular expression pattern: "; the variable parts are the value taken from the request, the member's wire name (kmsKeyIdentifier) and the pattern quoted VERBATIM from the model — i.e. including the leading ^ and trailing $. The refused update must also leave the connection untouched (a later description-only update still succeeds and changes Description).
Emulator: the status, the code, the framing, the quoted value and the member name all match, and the only divergence is the quoted pattern: the emulator renders "Member must satisfy regular expression pattern: [a-zA-Z0-9_\\-/:]*" — the ^ and $ anchors are stripped from the pattern it echoes.
Why: the pattern in this message is the machine-readable part of it — SDK users, code generators and operators copy it out to validate the value locally or to build their own guard. Unanchored, that regex matches ANY string (it can match the empty substring anywhere), so a client that trusts the echoed pattern accepts values the service will keep refusing, and the refusal becomes unreproducible client-side. Message-pinned parity assertions on this refusal also fail.
Fix: the @pattern constraint-violation message renderer in the events input-validation layer — it must quote the trait's pattern string exactly as the model carries it (anchors included) rather than a stripped/normalised form. Because the renderer is shared, every @pattern-constrained member of the service is affected, not only KmsKeyIdentifier.
Traces: events.update-connection-malformed-kms-key-identifier-rejected.

## Evidence
Cause 1 —
- events.list-event-buses-paginates-with-limit-and-next-token: ListEventBuses(NamePrefix=<prefix>, NextToken="sim2real-not-a-token"). Expected ('ValidationException', 'Parameter NextToken is not valid.'); actual, verbatim from the replay tail: "AssertionError: a NextToken the service never issued must be refused, got 'ValidationException' 'Parameter NextToken is not valid. Reason: The request is malformed.'" (the earlier Limit=1 paging and the Limit=101 range refusal in the same check both passed, so only the message diverges).
- events.list-targets-by-rule-paginates-with-limit-and-next-token: same probe on ListTargetsByRule. Actual: "AssertionError: a NextToken the service never issued was refused with message 'Parameter NextToken is not valid. Reason: The request is malformed.'" The token string is the check's own literal and is not part of the expected message at all — AWS's message is 100% fixed text.

Cause 2 —
- events.update-connection-malformed-kms-key-identifier-rejected: CreateConnection(API_KEY) settles AUTHORIZED, then UpdateConnection(Name=<conn>, KmsKeyIdentifier="!!!not-a-kms-identifier!!!").
  Expected: ValidationException, "1 validation error detected: Value '!!!not-a-kms-identifier!!!' at 'kmsKeyIdentifier' failed to satisfy constraint: Member must satisfy regular expression pattern: ^[a-zA-Z0-9_\-/:]*$".
  Actual (replay tail): ValidationException, "1 validation error detected: Value '!!!not-a-kms-identifier!!!' at 'kmsKeyIdentifier' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9_\-/:]*".
  The connection name is a per-run value and is not part of the message; the quoted value ('!!!not-a-kms-identifier!!!') is the request's own input, and the pattern is the model's literal. The single divergence is the missing ^ and $.

## Steps to Reproduce
Cause 1:
1. events CreateEventBus three times with a common unique NamePrefix.
2. events ListEventBuses(NamePrefix=<prefix>, Limit=1) and page with the returned NextToken until exhausted (this part already matches AWS).
3. events ListEventBuses(NamePrefix=<prefix>, NextToken="sim2real-not-a-token"). Expect HTTP 400 ValidationException with Message exactly "Parameter NextToken is not valid."; observe "Parameter NextToken is not valid. Reason: The request is malformed."
4. Repeat against ListTargetsByRule(Rule=<a rule with targets>, NextToken="sim2real-not-a-token") — same divergence.

Cause 2:
1. events CreateConnection(Name=<unique>, Description="sim2real original description", AuthorizationType="API_KEY", AuthParameters.ApiKeyAuthParameters={ApiKeyName:"x-api-key", ApiKeyValue:"secret-value"}); poll DescribeConnection until ConnectionState=AUTHORIZED.
2. events UpdateConnection(Name=<that connection>, KmsKeyIdentifier="!!!not-a-kms-identifier!!!"). Expect ValidationException whose message ends "Member must satisfy regular expression pattern: ^[a-zA-Z0-9_\-/:]*$"; observe the same message with the anchors stripped.
3. Confirm the modeled pattern with smithy_query(query="operation", operation="UpdateConnection", model_path="services/service-com-amazonaws-eventbridge-awsevents/smithy-model/com-amazonaws-eventbridge-awsevents.json") — KmsKeyIdentifier pattern "^[a-zA-Z0-9_\-/:]*$".
4. DescribeConnection must still report Description "sim2real original description", and a following UpdateConnection(Description="sim2real updated description") must succeed with ConnectionState AUTHORIZED (both unreached in this run).
