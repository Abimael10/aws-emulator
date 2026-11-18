Issue-Source: trace-sweep-divergence
Issue-Trace: events-numeric-matching-bounds
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: events-numeric-matching-bounds events-test-event-pattern-number-representability-per-matcher events-test-event-pattern-numeric-operand-double-precision events-test-event-pattern-wildcard-escapes events-test-event-pattern-anything-but-list-forms events-test-event-pattern-matcher-operand-type-refusals events-test-event-pattern-escaped-star-between-wildcards
Issue-Shape-Id: com.amazonaws.eventbridge#AWSEvents
Issue-Key: events|TestEventPattern|4xx->2xx|$.__type
Issue-Key: events|TestEventPattern|4xx->2xx|$.message
Issue-Key: events|TestEventPattern|4xx->2xx|status
Issue-Key: events|TestEventPattern|4xx->4xx|$.message
Issue-Key: events|PutRule|4xx->2xx|$.__type
Issue-Key: events|PutRule|4xx->2xx|$.message
Issue-Key: events|PutRule|4xx->2xx|status
Issue-Key: events|DescribeRule|4xx->2xx|$.__type
Issue-Key: events|DescribeRule|4xx->2xx|$.message
Issue-Key: events|DescribeRule|4xx->2xx|status
Issue-Key: events|TestEventPattern|2xx->4xx|$.Result
Issue-Key: events|TestEventPattern|2xx->4xx|status
Issue-Key: events|PutRule|2xx->4xx|$.RuleArn
Issue-Key: events|PutRule|2xx->4xx|status
Issue-Key: events|DescribeRule|2xx->4xx|$.Arn
Issue-Key: events|DescribeRule|2xx->4xx|$.CreatedBy
Issue-Key: events|DescribeRule|2xx->4xx|$.EventBusName
Issue-Key: events|DescribeRule|2xx->4xx|$.EventPattern
Issue-Key: events|DescribeRule|2xx->4xx|$.Name
Issue-Key: events|DescribeRule|2xx->4xx|$.State
Issue-Key: events|DescribeRule|2xx->4xx|status
Issue-Key: events|TestEventPattern|2xx->2xx|$.Result
Issue-Commit: eab59dd3d13731329d6515ebc512ab3051ad49e1

# events: 8 net-new causes — the event-pattern validator and matcher diverge on numeric and wildcard operands

> _Note: this issue was identified against the code base at commit `eab59dd3d13731329d6515ebc512ab3051ad49e1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TestEventPattern

## Description
Eight independent defects in the single EventBridge event-pattern validator/matcher that TestEventPattern, PutRule and CreateArchive all share. Each must be closed before this issue is submitted. Two open BEHAVIORAL issues touch the same component but do NOT cover these wire steps (they carry no wire replay key): 20261117T115655Z-events-7-net-new-causes-anything-but-equals.md (its cause 3 is the PutRule numeric-operand acceptance, its cause 5 the numeric matcher precision) and 20261117T115655Z-events-sqs-4-net-new-causes-putrule-accepts.md (wildcard escapes). Fixing the component must close these wire causes too, and they must be re-replayed.

Cause 1 of 8 — TestEventPattern accepts a numeric operand no IEEE-754 binary64 double represents exactly.
AWS: Smithy TestEventPattern.EventPattern is @required, targets EventPattern with @length 0..4096, and the operation models InvalidEventPatternException (errorKind client, httpError 400). A `numeric` operand that does not round-trip through a binary64 double is refused with HTTP 400 InvalidEventPatternException whose message is the fixed text `Event pattern is not valid. Reason: Cannot compare number : ` followed by the operand as the request wrote it, then a newline and the locator ` at [Source: (String)"<the EventPattern exactly as sent>"; line: 1, column: <1-based column of the brace that closes the matcher object holding the operand>]`. Fixed text: `Event pattern is not valid. Reason: Cannot compare number : `; variable parts: the operand, the echoed pattern and the column, all derived from the request.
Emulator: HTTP 200 with a TestEventPattern result body and no error at all — `$.__type` and `$.message` are absent where AWS sends InvalidEventPatternException.
Why: a client cannot discover that its operand exceeds the matcher's numeric domain; it believes a pattern AWS would reject is testable, and later gets silently wrong match verdicts (cause 8) instead of an error.
Fix: the numeric-matcher operand validation in the shared event-pattern validator, and the InvalidEventPatternException it must raise on the TestEventPattern path.
Traces: events-numeric-matching-bounds#task#2 (cluster of 11 steps, also events-test-event-pattern-number-representability-per-matcher#task#3/#4/#5, events-test-event-pattern-numeric-operand-double-precision#task#2/#4/#5/#6, events-test-event-pattern-wildcard-escapes#task#9/#11).

Cause 2 of 8 — the non-numeric JSON token refusal uses Jackson's 'Unrecognized token' wording and points at the token's first column.
AWS: an EventPattern containing a non-standard JSON numeric token (`Infinity`, `-Infinity`, `NaN`) is refused HTTP 400 InvalidEventPatternException with the fixed text `Event pattern is not valid. Reason: Non-standard token '<token>': enable `JsonReadFeature.ALLOW_NON_NUMERIC_NUMBERS` to allow` plus a newline and ` at [Source: (String)"<the EventPattern as sent>"; line: 1, column: <1-based column just PAST the offending token>]`. Variable parts: the token text, the echoed pattern and the column, all from the request.
Emulator: the same code and status, but the reason reads `Unrecognized token '<token>': was expecting (JSON String, Number, Array, Object or token 'null', 'true' or 'false')` and the locator column is the column at which the token STARTS, not the one just past its last character (recorded step: 37 instead of 45 for a token occupying columns 37..44).
Why: a client keying on the reason sentence cannot tell a non-standard numeric token from arbitrary JSON garbage, and a client that uses the locator column to highlight the offending span highlights the wrong position.
Fix: the event-pattern JSON parse-error mapping in the validator — the non-numeric-token branch must produce AWS's `Non-standard token` sentence and advance the reported column past the token.
Traces: events-numeric-matching-bounds#task#5 (cluster of 9 steps, also events-test-event-pattern-anything-but-list-forms#task#11..#14 and events-test-event-pattern-matcher-operand-type-refusals#task#7/#8).

Cause 3 of 8 — PutRule accepts and STORES a pattern whose numeric operand no double represents.
AWS: PutRule models InvalidEventPatternException (client, 400) and validates EventPattern (@length 0..4096) with the same validator as TestEventPattern, so the operand of cause 1 is refused with the identical `Event pattern is not valid. Reason: Cannot compare number : <operand>` message and locator. The refusal mints no rule and leaves any existing rule's stored pattern untouched.
Emulator: HTTP 200 with a RuleArn — no `$.__type`/`$.message` — and the rule is created with a pattern AWS would never store.
Why: a rule that cannot exist on AWS exists in the emulator; the client's deployment appears to succeed and the rule then matches by emulator-only semantics.
Fix: PutRule's EventPattern validation must call the same numeric-operand check as the validator above, before the rule is written to state.
Traces: events-numeric-matching-bounds#task#10.

Cause 4 of 8 — DescribeRule answers 200 for a rule name that does not exist on the bus.
AWS: DescribeRule models ResourceNotFoundException (errorKind client, httpError 400). A name not present on the named bus is refused HTTP 400 ResourceNotFoundException with the fixed text `Rule <the Name from the request> does not exist on EventBus <the resolved EventBusName, `default` when the request omits it>.` — the rule name and bus name are the only variable parts.
Emulator: HTTP 200 with a full rule body, `$.__type` and `$.message` absent. In this fixture the name is reachable only because cause 3 let the preceding PutRule store a pattern AWS rejects; but the divergence is DescribeRule's own: it reports a rule for a name AWS reports as absent, so the not-found path is unreachable for the client either way.
Why: a client that probes for a rule's existence (or expects 400 before a create) reads a phantom rule and skips the create, or treats a failed deployment as live.
Fix: DescribeRule's state lookup and its ResourceNotFoundException path (it must not resolve names the bus does not hold), together with cause 3 so no rule is minted by a refused PutRule.
Traces: events-numeric-matching-bounds#task#11.

Cause 5 of 8 — TestEventPattern REFUSES a valid wildcard operand that escapes a `*` between two wildcards.
AWS: inside a `wildcard` operand, `\*` is a legal escape standing for a literal asterisk and `\\` for a literal backslash, so an operand that places an escaped star between unescaped wildcards is a valid pattern. TestEventPattern answers HTTP 200 with `Result` (Boolean, default false) set from the match — true for an event whose value contains a literal `*` in that position. Recorded behavior only: the model constrains EventPattern by @length alone, so the escape grammar comes from the recording and AWS's wildcard documentation.
Emulator: HTTP 400 — the pattern is rejected as invalid and `$.Result` is absent.
Why: a pattern AWS accepts cannot be tested at all, so matching on a literal asterisk is unusable; every client that pre-validates its pattern with TestEventPattern is blocked.
Fix: the wildcard matcher's escape parsing in the event-pattern validator — `\*` and `\\` must be accepted anywhere in the operand, including adjacent to unescaped wildcards.
Traces: events-test-event-pattern-escaped-star-between-wildcards#task#1 (cluster of 3 steps, also #task#2 and events-test-event-pattern-matcher-operand-type-refusals#task#9).

Cause 6 of 8 — PutRule refuses the same valid escaped-wildcard pattern, so the rule is never created.
AWS: PutRule stores that pattern and answers HTTP 200 with `RuleArn` = `arn:aws:events:<region>:<account-id>:rule/<Name>` (region, account and name come from the request/credentials).
Emulator: HTTP 400 and no `$.RuleArn`, because PutRule shares the validator of cause 5.
Why: a rule matching a literal asterisk cannot be deployed at all — the client's create fails with a validation error for a pattern AWS accepts.
Fix: same validator fix as cause 5, on PutRule's validation path.
Traces: events-test-event-pattern-escaped-star-between-wildcards#task#4.

Cause 7 of 8 — DescribeRule then reports nothing for that rule.
AWS: after the PutRule of cause 6, DescribeRule answers HTTP 200 echoing `Name` (the request's name), `Arn` (the rule ARN), `EventPattern` (the stored pattern byte-for-byte), `EventBusName` (`default` when unspecified), `State` `ENABLED` when PutRule did not disable it, and `CreatedBy` (the caller's 12-digit account id).
Emulator: HTTP 400 with none of those members, because the rule was never created (cause 6).
Why: the client cannot read back the pattern it deployed; the read path reports a hard error rather than the rule.
Fix: follows from cause 6 — the rule must exist; DescribeRule's response assembly for `EventPattern`/`State`/`CreatedBy` must then be verified against this step.
Traces: events-test-event-pattern-escaped-star-between-wildcards#task#5.

Cause 8 of 8 — the matcher OVER-matches: Result true where AWS answers false, for escaped wildcards and for an unrepresentable event number.
AWS: TestEventPattern answers HTTP 200 with `Result` false when the pattern does not match. An escaped `\*`/`\\` inside a `wildcard` operand matches ONLY the literal character, so an event whose value would match the operand with the escape read as a wildcard is withheld; and when the EVENT's number is not exactly representable as a binary64 double the numeric matcher does not consider it, giving false even for a comparison the exact integer would satisfy. Recorded behavior only (output is just `Result`, Boolean, default false).
Emulator: HTTP 200 with `Result` true on all of these rows.
Why: the emulator admits events AWS withholds, so a rule built on an escaped wildcard or a numeric bound over-delivers on the emulator and the client's filter looks correct until it runs on AWS.
Fix: the wildcard matcher's escape handling at MATCH time (an escaped metacharacter must compare literally) and the numeric comparison's representability check in the event-pattern matcher.
Traces: events-test-event-pattern-numeric-operand-double-precision#task#7 (cluster of 6 steps, also events-test-event-pattern-wildcard-escapes#task#1/#3/#4/#5/#6).

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout: 686 steps, 40 mismatched, score 94.2%.

Cause 1 — events-numeric-matching-bounds#task#2 (TestEventPattern): expected 400 with $.__type 'InvalidEventPatternException' and $.message 'Event pattern is not valid. Reason: Cannot compare number : 9007199254740993\n at [Source: (String)"{\"detail\": {\"n\": [{\"numeric\": [\"=\", 9007199254740993]}]}}"; line: 1, column: 54]'; got 200 with neither member (11 steps in the cluster).
Cause 2 — events-numeric-matching-bounds#task#5 (TestEventPattern): both sides 400; expected $.message 'Event pattern is not valid. Reason: Non-standard token \'Infinity\': enable `JsonReadFeature.ALLOW_NON_NUMERIC_NUMBERS` to allow\n at [Source: (String)"{\"detail\": {\"n\": [{\"numeric\": [\"=\", Infinity]}]}}"; line: 1, column: 45]'; got 'Event pattern is not valid. Reason: Unrecognized token \'Infinity\': was expecting (JSON String, Number, Array, Object or token \'null\', \'true\' or \'false\')\n at [Source: (String)"..."; line: 1, column: 37]' (9 steps in the cluster).
Cause 3 — events-numeric-matching-bounds#task#10 (PutRule): expected 400 InvalidEventPatternException with the cause-1 message; got 200.
Cause 4 — events-numeric-matching-bounds#task#11 (DescribeRule): expected 400 with $.__type 'ResourceNotFoundException' and $.message 'Rule <name> does not exist on EventBus default.'; got 200 with a rule body.
Cause 5 — events-test-event-pattern-escaped-star-between-wildcards#task#1 (TestEventPattern): expected 200 with $.Result true; got 400 and no $.Result (3 steps in the cluster).
Cause 6 — events-test-event-pattern-escaped-star-between-wildcards#task#4 (PutRule): expected 200 with $.RuleArn 'arn:aws:events:<region>:<account>:rule/<name>'; got 400 and no $.RuleArn.
Cause 7 — events-test-event-pattern-escaped-star-between-wildcards#task#5 (DescribeRule): expected 200 with $.Arn, $.Name, $.EventBusName 'default', $.State 'ENABLED', $.CreatedBy '<account-id>' and $.EventPattern '{"detail": {"s": [{"wildcard": "*\\\\**"}]}}'; got 400 with none of them.
Cause 8 — events-test-event-pattern-numeric-operand-double-precision#task#7 and events-test-event-pattern-wildcard-escapes#task#1/#3/#4/#5/#6 (TestEventPattern): expected 200 $.Result false; got 200 $.Result true.

## Steps to Reproduce
1. Clone the repo, boot the emulator and replay the wire corpus simparity-wire-aws-v1; the failing steps are the fixtures/steps listed under each cause.
2. Cause 1: TestEventPattern with EventPattern '{"detail": {"n": [{"numeric": ["=", 9007199254740993]}]}}' and any event — must be 400 InvalidEventPatternException with the 'Cannot compare number : <operand>' message and the locator column of the matcher object's closing brace.
3. Cause 2: TestEventPattern with EventPattern '{"detail": {"n": [{"numeric": ["=", Infinity]}]}}' — compare the Reason sentence and the locator column against AWS's 'Non-standard token' message with the column just past the token.
4. Cause 3: PutRule with the cause-1 pattern — must be refused and must create no rule; then DescribeRule that name (cause 4) and expect 400 ResourceNotFoundException 'Rule <name> does not exist on EventBus default.'.
5. Cause 5/6/7: TestEventPattern, then PutRule, then DescribeRule with a wildcard operand that escapes a star between two wildcards ('*\\**' as written in JSON) — the test must answer 200 Result true, the rule must be created and read back with the stored pattern.
6. Cause 8: TestEventPattern with escaped-wildcard operands against events that only match if the escape is read as a wildcard, and with a numeric comparison against an event number above 2^53 that no double represents — both must answer 200 Result false.
