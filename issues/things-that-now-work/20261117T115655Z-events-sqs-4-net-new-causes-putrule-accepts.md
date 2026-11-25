Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__sqs.events-put-rule-invalid-wildcard-escape-rejected-and-the-live-rule-keeps-matching events__sqs.events-put-rule-numeric-cidr-operand-rejected-and-the-live-rule-keeps-matching events__sqs.events-pattern-empty-string-operands-deliver-strings-only events__sqs.events-pattern-wildcard-escapes-deliver-only-the-literal-star-and-backslash
Issue-Key: events__sqs.events-put-rule-invalid-wildcard-escape-rejected-and-the-live-rule-keeps-matching|_error#1|AssertionError|raise AssertionError("the call succeeded where AWS refuses it")
Issue-Key: events__sqs.events-put-rule-numeric-cidr-operand-rejected-and-the-live-rule-keeps-matching|run#6|AssertionError|assert msg == want, f"refusal message {msg!r} != {want!r}"
Issue-Key: events__sqs.events-pattern-empty-string-operands-deliver-strings-only|_drain_one#1|Timeout|msgs = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10,
Issue-Key: events__sqs.events-pattern-wildcard-escapes-deliver-only-the-literal-star-and-backslash|_drain_one#1|Timeout|msgs = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10,
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# events__sqs: 4 net-new causes — PutRule accepts an invalid wildcard escape, the non-string cidr operand refusal uses the wrong Reason, and two pattern-language delivery matrices never converge

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutRule

## Description
Four independent defects on the EventBridge pattern-language/SQS-delivery path. Each must be closed before this issue is submitted.

Cause 1 of 4 — PutRule accepts a wildcard operand with an unknown escape.
AWS: PutRule validates EventPattern and models InvalidEventPatternException (errorKind client, httpError 400; Smithy: PutRule.EventPattern @length 0..4096). A `wildcard` operand containing an escape sequence that is not a legal wildcard escape is refused with HTTP 400 InvalidEventPatternException whose message is the fixed text `Event pattern is not valid. Reason: Invalid escape character at pos <position of the offending escape inside the operand>` followed by a newline and ` at [Source: (String)"<the EventPattern exactly as sent>"; line: 1, column: <1-based column just past the offending operand>]`. The refusal must leave the rule's stored EventPattern unchanged, and the live rule must keep delivering what its original wildcard matches.
Emulator: PutRule SUCCEEDS on the malformed pattern (the check's `_error` helper reports "the call succeeded where AWS refuses it"), so the update is applied and the rule's stored pattern is replaced by one AWS would never store.
Why: a client's malformed pattern is silently accepted and the live rule's matching behavior changes underneath it, with no error to act on.
Fix: the wildcard matcher's escape parsing in the event-pattern validator invoked by PutRule, and the InvalidEventPatternException it must raise.
Traces: events__sqs.events-put-rule-invalid-wildcard-escape-rejected-and-the-live-rule-keeps-matching.

Cause 2 of 4 — the non-string `cidr` operand refusal reports the wrong Reason sentence.
AWS: a `cidr` operand that is a number rather than a string is refused with HTTP 400 InvalidEventPatternException whose message is `Event pattern is not valid. Reason: prefix match pattern must be a string` followed by a newline and the ` at [Source: (String)"<the EventPattern as sent>"; line: 1, column: <1-based column just past the operand>]` locator — AWS reports the generic string-operand reason here, not a cidr-specific one. Fixed text: `Event pattern is not valid. Reason: prefix match pattern must be a string`; variable parts: the echoed pattern and the column.
Emulator: correct code (InvalidEventPatternException), correct locator, but the reason reads `cidr match pattern must be an IPv4 or IPv6 CIDR block`.
Why: a client that matches on the refusal sentence to classify operand-type errors mis-classifies this one; the message is a requirement, not a cosmetic detail.
Fix: the cidr matcher's operand type check in the event-pattern validator — a non-string operand must fall through to the generic string-operand message.
Traces: events__sqs.events-put-rule-numeric-cidr-operand-rejected-and-the-live-rule-keeps-matching.

Cause 3 of 4 — the empty-string-operand delivery matrix never completes.
AWS: an empty `prefix` or `suffix` operand matches every STRING value (the empty string included) and no number, boolean, null or absent field; an empty `wildcard` or `equals-ignore-case` operand matches only the empty string. Each rule's SQS target receives exactly its matching events and nothing else. Recorded behavior only (the model constrains EventPattern by length alone).
Emulator: the check never reaches its assertions — the replay reports `Check timed out after 60.0s`. Hypothesis: the per-rule custom-bus → SQS target deliveries that the matrix's warm-up rounds wait for never arrive, so the matrix never converges.
Why: a client cannot establish which values an empty string operand admits; and if nothing is delivered at all, no rule in the matrix works.
Fix: the custom-bus rule → SQS target delivery path, plus the empty-operand branches of the prefix/suffix/wildcard/equals-ignore-case matchers.
Traces: events__sqs.events-pattern-empty-string-operands-deliver-strings-only.

Cause 4 of 4 — the wildcard-escape delivery matrix never completes.
AWS: inside a `wildcard` operand, `\\*` matches a literal `*` and `\\\\` matches a literal backslash, so such a rule delivers only the events carrying those literal characters and withholds the rest. Recorded behavior only.
Emulator: the check never reaches its assertions — `Check timed out after 60.0s`. Hypothesis: as in cause 3, the matrix's per-rule SQS deliveries never arrive.
Why: a client cannot establish that an escaped wildcard matches the literal character rather than acting as a wildcard.
Fix: the wildcard matcher's escape handling at match time and the same custom-bus → SQS delivery path.
Traces: events__sqs.events-pattern-wildcard-escapes-deliver-only-the-literal-star-and-backslash.

## Evidence
Cause 1: PutRule with `{"detail": {"k": [{"wildcard": "a\\\\b"}]}, ...}` — expected InvalidEventPatternException `Event pattern is not valid. Reason: Invalid escape character at pos 2\n at [Source: (String)"<pattern>"; line: 1, column: <col>]`; got a successful PutRule (AssertionError: "the call succeeded where AWS refuses it").
Cause 2: expected `Event pattern is not valid. Reason: prefix match pattern must be a string\n at [Source: (String)"{\"detail\": {\"ip\": [{\"cidr\": 5}]}, \"source\": [...], \"detail-type\": [...]}"; line: 1, column: 30]`; got the same envelope with `Reason: cidr match pattern must be an IPv4 or IPv6 CIDR block`.
Causes 3 and 4: `Check timed out after 60.0s` — no assertion reached; the checks' warm-up rounds require at least one delivery per rule queue before any absence is judged.

## Steps to Reproduce
1. Replay the behavioral suite and re-run the checks in trace_ids.
2. Cause 1: create a rule with `{"detail":{"k":[{"wildcard":"a*b"}]}}`, then PutRule the same name with `{"detail":{"k":[{"wildcard":"a\\b"}]}}` — must be refused with InvalidEventPatternException and leave the stored pattern unchanged.
3. Cause 2: PutRule with `{"detail":{"ip":[{"cidr":5}]}}` and compare Error.Message's Reason clause.
4. Causes 3/4: create a custom bus, one rule per operand row with its own SQS queue (queue policy allowing events.amazonaws.com for the rule ARN), PutEvents the row events, and observe which queues receive which events.

## Resolution

All four causes are closed; what is pinned where, and the one residual outside this
service crate, are recorded here rather than left implied.

Causes 1, 2 and 4 are the shared event-pattern validator and matcher
(`services/service-com-amazonaws-eventbridge-awsevents/src/state/event_pattern.rs`),
which `PutRule`, `CreateArchive`/`UpdateArchive` and `TestEventPattern` all read a
pattern through and which routing matches an event with. They were closed by the fix
for the wire-corpus issue `20261117T140447Z-events-8-net-new-causes-the-event-pattern.md`,
which landed after this report was filed: `wildcard_violation` now reads the operand's
escape grammar (only the wildcard and the escape itself may be escaped, so an escape of
anything else is refused with `Invalid escape character at pos <position past the escape
marker>`), `wildcard_segments` reads an escaped metacharacter as one character of
literal text at match time, and `validate_operator`'s `cidr` arm refuses an operand that
is not text at all with the plain string-operand requirement every text-comparing match
type shares, which the service words for the first of them (`prefix match pattern must
be a string`). Their wording and locators are pinned by
`tests/all/pattern_numeric_and_wildcard_operands.rs` and
`tests/all/pattern_refusal_location.rs`; the half those suites did not cover — that a
refused `PutRule` leaves the live rule's stored pattern and its deliveries untouched —
is pinned by `tests/all/pattern_empty_operand_delivery.rs`.

Cause 3's rule is that an operand is compared as the TEXT it is, so an empty operand is
the empty text and not a wildcard: `prefix`/`suffix` compare one END of the leaf and
hold for every string leaf, the empty string included, while `wildcard`/
`equals-ignore-case` compare the WHOLE leaf and hold only for the empty string; no
text-comparing match type holds for a leaf that is not text or for a field the event
does not carry. The matchers' empty-operand branches implement exactly that, and the
custom-bus rule → SQS delivery path the report names was repaired by the fan-out fix
recorded in `20261111T221227Z-events-sqs-a-1-000-event-custom-bus.md`. Both halves are
now pinned end to end, over a table of six operand forms against ten leaf kinds, by
`tests/all/pattern_empty_operand_delivery.rs`: one rule per operand row on a custom bus,
each with its own queue whose policy admits that rule's deliveries, each queue receiving
exactly the events its operand admits — and the same table read back through
`TestEventPattern`, so the reporting surface and the live rule answer one verdict.

What is left, and where: replaying
`events__sqs.events-pattern-empty-string-operands-deliver-strings-only` still ends in
`Check timed out after 60.0s` inside the check's first `_drain_one`, while its three
sibling units pass. The report's hypothesis for it — that the per-rule custom-bus → SQS
deliveries never arrive — does not hold: every delivery that rule set requires arrives,
synchronously with the `PutEvents` that decided it, and nothing else does. Probed
against this checkout and all correct: the four operands and their nested and literal
forms against string, empty-string, blank, upper-case, number, boolean, null, absent,
object and list leaves; patterns naming only `detail`, naming a nested or dotted path,
naming the envelope's own string members, and relating branches with `$or`; a rule whose
bus is named by name or by ARN, with and without `State`; queue policies written
unconditionally, scoped by `aws:SourceArn` to the rule, scoped by `ArnLike`, with
list-valued `Principal`/`Action`, with `Resource` `*`, and installed either at
`CreateQueue` or by `SetQueueAttributes`; and receive-delete-receive until empty. The
remaining distance for this family of checks is the per-dispatch cost recorded in the
fan-out issue above — every request the emulator serves pays the claim scan in
`vendor/smithy-server-runtime/src/multi_service_router.rs` — and no change inside a
service crate reaches it.
