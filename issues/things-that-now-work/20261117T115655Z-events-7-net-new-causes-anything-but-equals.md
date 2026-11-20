Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events.put-rule-anything-but-ignore-case-list-with-a-number-rejected-and-no-rule-created events.create-archive-anything-but-ignore-case-list-with-a-number-rejected-and-no-archive-created events.list-connections-paginates-with-limit-and-next-token events.put-rule-refuses-a-numeric-operand-no-double-represents-and-keeps-the-stored-pattern events.test-event-pattern-cidr-operand-masks-its-host-bits events.test-event-pattern-numeric-skips-an-event-number-no-double-represents events.update-connection-malformed-kms-key-identifier-rejected events.put-events-entries-over-ten-rejected-whole-and-exactly-ten-delivers
Issue-Shape-Id: com.amazonaws.eventbridge#AWSEvents
Issue-Key: events.put-rule-anything-but-ignore-case-list-with-a-number-rejected-and-no-rule-created|run#14|AssertionError|assert msg == want, f"refusal message {msg!r} != {want!r}"
Issue-Key: events.create-archive-anything-but-ignore-case-list-with-a-number-rejected-and-no-archive-created|run#14|AssertionError|assert msg == want, f"refusal message {msg!r} != {want!r}"
Issue-Key: events.list-connections-paginates-with-limit-and-next-token|run#14|AssertionError|assert (code, message) == (
Issue-Key: events.put-rule-refuses-a-numeric-operand-no-double-represents-and-keeps-the-stored-pattern|_refusal#10|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {outcome!r}")
Issue-Key: events|TestEventPattern|InvalidEventPatternException
Issue-Key: events.test-event-pattern-numeric-skips-an-event-number-no-double-represents|_expect#5|AssertionError|assert got is want, (
Issue-Key: events.update-connection-malformed-kms-key-identifier-rejected|run#11|AssertionError|assert (code, message) == (
Issue-Key: events.put-events-entries-over-ten-rejected-whole-and-exactly-ten-delivers|_drain#39|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# events: 7 net-new causes — anything-but/equals-ignore-case refusal wording, ListConnections NextToken message, unvalidated non-double numeric operand, cidr host-bit masking, numeric matcher double precision, kmsKeyIdentifier pattern anchors, and the 10-entry PutEvents path

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutRule

## Description
Seven independent EventBridge defects found by the behavioral replay. Each must be closed before this issue is submitted.

Cause 1 of 7 — the `anything-but`/`equals-ignore-case` non-string element refusal carries the wrong Reason and only one source locator.
AWS: PutRule and CreateArchive both validate EventPattern (Smithy: PutRule.EventPattern @length 0..4096, CreateArchive.EventPattern @length 0..4096; both model error InvalidEventPatternException, errorKind client, httpError 400). For a pattern whose `anything-but`/`equals-ignore-case` list holds a non-string element, AWS answers HTTP 400 InvalidEventPatternException. The message is fixed text plus a source locator. PutRule's fixed text is `Event pattern is not valid. Reason: Inside anything-but/ignore-case list, number|start|null|boolean is not supported.`; CreateArchive's fixed text is `Event pattern is not valid. Reason: Inside anything-but/equals-ignore-case list, number|start|null|boolean is not supported.` Each is followed by a newline and the locator ` at [Source: (String)"<the EventPattern exactly as the request sent it>"; line: 1, column: <1-based column just past the offending list>]`, and that whole locator line is emitted TWICE (separated by a newline). The only variable parts are the echoed pattern text (from the request) and the column (derived from where the offending element sits in it).
Emulator: HTTP 400 InvalidEventPatternException, but the message is `Event pattern is not valid. Reason: equals-ignore-case match pattern must be a string` and the locator line is emitted only ONCE.
Why: a client that keys on the reason sentence to tell "a non-string inside an anything-but list" apart from "the whole equals-ignore-case operand is not a string" cannot, and a client that parses the duplicated locator block sees a shorter body than AWS produces.
Fix: the event-pattern validator's anything-but/equals-ignore-case branch and the InvalidEventPatternException message builder shared by PutRule and CreateArchive.
Traces: events.put-rule-anything-but-ignore-case-list-with-a-number-rejected-and-no-rule-created, events.create-archive-anything-but-ignore-case-list-with-a-number-rejected-and-no-archive-created.

Cause 2 of 7 — ListConnections' unissued-NextToken refusal is truncated.
AWS: Smithy ListConnections.NextToken @length 1..2048, ListConnections.Limit targets LimitMax100 @range 1..100. A NextToken the service never issued is refused with ValidationException whose message is the fixed text `Parameter NextToken is not valid. Reason: The request is malformed.` (no variable part).
Emulator: ValidationException with `Parameter NextToken is not valid.` — the `Reason: The request is malformed.` sentence is missing.
Why: the Limit=101 refusal in the same operation is already correct, so a client comparing the two refusals sees an EventBridge-shaped message for one and a truncated one for the other; token-recovery logic keyed on the reason clause does not fire.
Fix: ListConnections' pagination-token validation and its ValidationException message.
Traces: events.list-connections-paginates-with-limit-and-next-token.

Cause 3 of 7 — PutRule accepts a numeric operand no IEEE-754 binary64 double represents exactly.
AWS: a `numeric` operand that does not survive a round trip through a binary64 double is refused with HTTP 400 InvalidEventPatternException (modeled on PutRule), message `Event pattern is not valid. Reason: Cannot compare number : <operand>` followed by a newline and ` at [Source: (String)"<the EventPattern as sent>"; line: 1, column: <1-based column of the brace closing the matcher object holding the operand>]`. Fixed text is `Event pattern is not valid. Reason: Cannot compare number : `; the operand, the echoed pattern and the column come from the request. The refusal leaves an existing rule's stored EventPattern untouched and mints no rule for a new name.
Emulator: PutRule returns HTTP 200 with a RuleArn and stores the pattern (observed: 200, `{'RuleArn': 'arn:aws:events:<region>:<account>:rule/<name>'}`), i.e. the operand is accepted on both the update and the create path.
Why: a pattern AWS rejects outright is persisted here, so a rule that cannot exist on AWS silently exists in the emulator and clients never see the operand-precision error.
Fix: the numeric-matcher operand validation inside the event-pattern validator used by PutRule (and the same validator's reuse by CreateArchive/TestEventPattern).
Traces: events.put-rule-refuses-a-numeric-operand-no-double-represents-and-keeps-the-stored-pattern.

Cause 4 of 7 — a `cidr` operand carrying host bits is refused instead of masked.
AWS: a `cidr` operand whose address has host bits set (e.g. an address/prefix pair where the address is not the block's network address) is a VALID operand: AWS masks the host bits and the operand stands for the containing block, so TestEventPattern answers HTTP 200 with Result true for every address in that block and false outside it, and agrees with the same block written canonically. Recorded behavior only (the Smithy model constrains EventPattern only by length); the operand form is the documented `cidr` matcher.
Emulator: HTTP 400 InvalidEventPatternException `Event pattern is not valid. Reason: cidr match pattern must be an IPv4 or IPv6 CIDR block` plus the ` at [Source: (String)"<pattern>"; line: 1, column: <col>]` locator, for both IPv4 and IPv6 operands with host bits set.
Why: patterns AWS accepts are unusable — PutRule/TestEventPattern reject a legal rule outright, so a client cannot deploy or test a cidr rule written with any address inside the block.
Fix: the `cidr` matcher's operand parsing in the event-pattern validator (parse non-strictly and mask host bits rather than requiring the canonical network address).
Traces: events.test-event-pattern-cidr-operand-masks-its-host-bits.

Cause 5 of 7 — the numeric matcher compares an event value no double represents.
AWS: when the EVENT's number is not exactly representable as a binary64 double, the numeric matcher does not consider it: TestEventPattern answers HTTP 200 Result false even for a comparison the exact integer would satisfy. Recorded behavior only (TestEventPattern models only Result: Boolean, default false).
Emulator: Result true for pattern `{"detail": {"n": [{"numeric": [">", 1]}]}}` against an event whose `detail.n` is an integer above 2^53 that no double represents.
Why: the emulator's matcher admits events AWS withholds, so a rule built on a numeric bound delivers records on the emulator that would never be delivered on AWS — a silent over-delivery.
Fix: the numeric comparison in the event-pattern matcher (reject event values that do not round-trip through binary64 before comparing).
Traces: events.test-event-pattern-numeric-skips-an-event-number-no-double-represents.

Cause 6 of 7 — the kmsKeyIdentifier @pattern ValidationException drops the regex anchors.
AWS: Smithy UpdateConnection.KmsKeyIdentifier targets KmsKeyIdentifier with @length 0..2048 and @pattern `^[a-zA-Z0-9_\-/:]*$`. A value violating it is refused with ValidationException (HTTP 400) whose message is `1 validation error detected: Value '<the rejected value from the request>' at 'kmsKeyIdentifier' failed to satisfy constraint: Member must satisfy regular expression pattern: ^[a-zA-Z0-9_\-/:]*$` — the quoted regex is the modeled pattern verbatim, anchors INCLUDED. The refusal leaves the connection's Description and state unchanged.
Emulator: same code, status and member name, but the regex is rendered with the anchors stripped: `... pattern: [a-zA-Z0-9_\-/:]*`.
Why: a client that compares the reported regex against the modeled one (or re-uses it to pre-validate) gets a regex that matches values AWS refuses, because an unanchored pattern matches any substring.
Fix: the constraint-violation message formatter used by the events service's input validation — it must echo the modeled @pattern unaltered for this service rather than going through the shared unanchored-pattern renderer (see the related open issue 20261028T050716Z-rbin-getrule-maintainability.md, which names that shared renderer but carries no events check).
Traces: events.update-connection-malformed-kms-key-identifier-rejected.

Cause 7 of 7 — the 10-entry PutEvents limit path never completes.
AWS: Smithy PutEvents.Entries is @required with @length min 1 max 10. A request of 11 entries is refused WHOLE with ValidationException `1 validation error detected: Value '[<every rejected entry rendered in AWS's Java object form, PutEventsRequestEntry(time=..., source=..., resources=..., detailType=..., eventGroupId=..., detail=..., eventBusName=..., traceHeader=..., kmsKeyIdentifier=..., internalMetadata=...), comma-separated>]' at 'entries' failed to satisfy constraint: Member must have length less than or equal to 10` — fixed text around a value list rendered from the request's own entries — and not one of the 11 reaches a rule's target, while a request of exactly 10 entries is accepted and all ten are delivered to the rule's SQS target.
Emulator: the check never reaches its assertions — the replay reports `Check timed out after 60.0s`. Hypothesis: the liveness/drain phase that must first observe a delivered entry on the custom bus's rule target never converges, so the entry-count refusal is never exercised.
Why: a client cannot establish either half of the limit contract — neither that 11 entries are refused whole nor that 10 are delivered.
Fix: PutEvents' entry-count validation plus the custom-bus rule → SQS target delivery path it depends on.
Traces: events.put-events-entries-over-ten-rejected-whole-and-exactly-ten-delivers.

## Evidence
Cause 1: expected `Event pattern is not valid. Reason: Inside anything-but/ignore-case list, number|start|null|boolean is not supported.\n at [Source: (String)"<pattern>"; line: 1, column: 66]\n at [Source: (String)"<pattern>"; line: 1, column: 66]` (PutRule) / `...anything-but/equals-ignore-case list...` (CreateArchive); got `Event pattern is not valid. Reason: equals-ignore-case match pattern must be a string\n at [Source: (String)"<pattern>"; line: 1, column: 66]` (locator once).
Cause 2: expected ('ValidationException', 'Parameter NextToken is not valid. Reason: The request is malformed.'); got ('ValidationException', 'Parameter NextToken is not valid.').
Cause 3: expected InvalidEventPatternException `Event pattern is not valid. Reason: Cannot compare number : 9007199254740993\n at [Source: (String)"{\"detail\": {\"n\": [{\"numeric\": [\"=\", 9007199254740993]}]}}"; line: 1, column: <col>]`; got HTTP 200 with a RuleArn ('expected the call to be refused, but it succeeded').
Cause 4: expected HTTP 200 Result verdicts for operands like `198.51.100.130/25`, `203.0.113.200/26`, `192.0.2.130/25`, `2001:db8::5/64`; got HTTP 400 InvalidEventPatternException `Event pattern is not valid. Reason: cidr match pattern must be an IPv4 or IPv6 CIDR block\n at [Source: (String)"{\"detail\": {\"ip\": [{\"cidr\": \"198.51.100.130/25\"}]}}"; line: 1, column: 30]`.
Cause 5: pattern `{"detail": {"n": [{"numeric": [">", 1]}]}}` against an event with `detail.n = 9007199254740993` — expected Result=False, got Result=True.
Cause 6: expected `1 validation error detected: Value '!!!not-a-kms-identifier!!!' at 'kmsKeyIdentifier' failed to satisfy constraint: Member must satisfy regular expression pattern: ^[a-zA-Z0-9_\-/:]*$`; got the same sentence ending `pattern: [a-zA-Z0-9_\-/:]*` (anchors stripped).
Cause 7: `Check timed out after 60.0s` — no assertion reached.

## Steps to Reproduce
1. Replay the behavioral suite and re-run the checks listed in trace_ids.
2. Cause 1: `events put-rule --name r --event-pattern '{"detail":{"k":[{"anything-but":{"equals-ignore-case":["a",1]}}]},"source":["s"]}'` and the same pattern via `create-archive`; compare Error.Message against AWS's two-locator message.
3. Cause 2: `events list-connections --name-prefix p --next-token c2ltMnJlYWw=`; compare the message.
4. Cause 3: `events put-rule --name r --event-pattern '{"detail": {"n": [{"numeric": ["=", 9007199254740993]}]}}'` — must be refused; then describe-rule to confirm the prior pattern survived.
5. Cause 4: `events test-event-pattern --event-pattern '{"detail":{"ip":[{"cidr":"198.51.100.130/25"}]}}' --event <envelope with detail.ip in 198.51.100.128/25>` — must return Result=true, not refuse.
6. Cause 5: `events test-event-pattern --event-pattern '{"detail":{"n":[{"numeric":[">",1]}]}}' --event <envelope with detail.n=9007199254740993>` — must return Result=false.
7. Cause 6: create an API_KEY connection, then `events update-connection --name <n> --kms-key-identifier '!!!not-a-kms-identifier!!!'`; compare the reported regex with the modeled `^[a-zA-Z0-9_\-/:]*$`.
8. Cause 7: create a custom bus, a rule on a source and an SQS target with a queue policy allowing events.amazonaws.com, then PutEvents with 11 entries and with exactly 10.
