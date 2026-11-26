Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__sqs.events-pattern-empty-string-operands-deliver-strings-only
Issue-Key: events__sqs.events-pattern-empty-string-operands-deliver-strings-only|_drain_one#1|Timeout|msgs = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10,
Issue-Commit: 043cf7bbddd41f9e9269741e32e8c49c8ecdcab2

# events__sqs: the empty-string operand pattern matrix reaches no verdict — a prefix/suffix/wildcard/equals-ignore-case rule with an empty operand never delivers its matching events inside the 60 s cap

> _Note: this issue was identified against the code base at commit `043cf7bbddd41f9e9269741e32e8c49c8ecdcab2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TestEventPattern

## Description
Cause 1 of 1: EventBridge string-comparison operands given the empty string do not match the values AWS matches, so a rule built on one of them never delivers and the whole pattern-language matrix is killed before any assertion runs.

AWS:      the content-filtering string operands take an operand of any length, the empty string included. {"prefix": ""} and {"suffix": ""} are satisfied by EVERY string value of that field — the empty string "" and any non-empty string such as "x" — and by no number, boolean, null or absent field, because the string comparators only apply to string values. {"wildcard": ""} and {"equals-ignore-case": ""} are satisfied only by the empty string "". The same verdict must come back from TestEventPattern as from live rule matching: Smithy: events TestEventPattern input EventPattern (@length 0..4096, required) + Event (required), output Result (boolean); its only client refusal is InvalidEventPatternException (400), so an empty operand is a VALID pattern and must be accepted by PutRule (Smithy: events PutRule, EventPattern @length 0..4096, InvalidEventPatternException 400) rather than refused. The exact match semantics of the empty operand are recorded behavior only — the Smithy model does not describe the pattern language.

Emulator: the check never produces a verdict. The replay reports only "Error: Check timed out after 60.0s": the entire harness budget is consumed and not one of the check's assertions (put_failures, warm, control, body_errors, TestEventPattern verdicts, live delivery, leaks) is reached. The check's warm phase sends every case in a PutEvents round and declares a rule "live" only once that rule has delivered ALL of its expected cases within a single round, retrying up to 16 rounds; a rule that never delivers its matching cases keeps that loop spinning until the cap. Every call the check makes is accepted (a refused PutRule or PutEvents would surface as a ClientError/FailedEntries assertion, not a timeout), so the emulator accepts the empty-operand patterns and then routes nothing through them. Hypothesis: the matcher treats an empty operand as unsatisfiable (or short-circuits it) instead of applying the AWS rule, so at least one of empty-prefix, empty-suffix, empty-wildcard and empty-ignore-case never goes live.

Why:      a client that relies on AWS's behavior has a rule it can create but that silently routes nothing. {"prefix": ""} / {"suffix": ""} is the idiomatic "this field is present and is a string" matcher and is what a generated or templated pattern degenerates to when the prefix value is empty; on AWS it fans every string-valued event out to the target, on the emulator the target is never invoked and no error ever says why. Conversely, if the emulator instead over-matches, an empty string operand would deliver numbers, booleans, nulls and events with the field absent, which AWS withholds. Either way TestEventPattern and live delivery can disagree with AWS without any refusal to warn the caller.

Fix:      the events crate's event-pattern matcher for the string comparison operands prefix, suffix, wildcard and equals-ignore-case in the zero-length-operand case, on the single code path shared by rule evaluation at PutEvents time and by TestEventPattern, so both answer identically. The comparators must also stay string-only: a number, boolean, null or absent field must not satisfy them.

Traces:   events__sqs.events-pattern-empty-string-operands-deliver-strings-only (the only failing events__sqs check that is neither ALREADY FILED nor OWNED by an open issue). Related but NOT covering: open issue 20261125T162859Z-events-sqs-custom-bus-fan-out-to-four.md records a similar "no observable progress" in events__sqs for the custom-bus fan-out check, and 20261124T132051Z-events-sqs-5-causes-puttargets-refusal-messages-for.md owns several other events__sqs checks — neither carries this check's replay key or the empty-operand semantics.

## Evidence
Expected (AWS), with detail field k and the six cases text {"k":"x"}, empty {"k":""}, number {"k":5}, null {"k":null}, boolean {"k":true}, absent {}:
  - rule {"detail":{"k":[{"prefix":""}]}}            -> matches text and empty only
  - rule {"detail":{"k":[{"suffix":""}]}}            -> matches text and empty only
  - rule {"detail":{"k":[{"wildcard":""}]}}          -> matches empty only
  - rule {"detail":{"k":[{"equals-ignore-case":""}]}} -> matches empty only
  - no rule matches number, null, boolean or absent
  - TestEventPattern returns Result true for exactly those pairs and false for the rest, and live SQS delivery through a PutTargets target agrees with it
  - the control rule (source only) receives all six cases

Actual (emulator): no assertion is reached. Replay verdict for events__sqs.events-pattern-empty-string-operands-deliver-strings-only is [mismatch] with detail "Error: Check timed out after 60.0s". The bus, the five queues, the five rules and the targets are all created without error and PutEvents reports no FailedEntries, so the empty-operand patterns are ACCEPTED (no InvalidEventPatternException, no non-2xx); the warm-up loop then never sees a round in which an empty-operand rule delivered its expected cases, retries up to 16 PutEvents rounds, and the 60 s replay cap expires first — so neither the TestEventPattern verdict matrix nor the live delivery matrix is ever compared.

## Steps to Reproduce
1. events CreateEventBus <bus>; pick a unique source S and detail-type "sim2real.pattern-language".
2. For each of the four fragments {"detail":{"k":[{"prefix":""}]}}, {"detail":{"k":[{"suffix":""}]}}, {"detail":{"k":[{"wildcard":""}]}}, {"detail":{"k":[{"equals-ignore-case":""}]}}, scope it with {"source":[S],"detail-type":["sim2real.pattern-language"]}, call events PutRule on <bus> with that EventPattern (observe it is accepted, no InvalidEventPatternException), create an SQS queue, set a queue Policy allowing events.amazonaws.com sqs:SendMessage scoped to the rule ARN, and PutTargets the queue ARN. Add a fifth control rule scoped to the source alone.
3. events PutEvents on <bus> with six entries whose Detail is {"k":"x"}, {"k":""}, {"k":5}, {"k":null}, {"k":true} and {} respectively (each tagged with a case name). Observe FailedEntryCount 0.
4. ReceiveMessage on each of the five queues. Observe that the control queue gets all six cases while at least one empty-operand queue never receives its expected case(s) — {"k":"x"} and {"k":""} for empty prefix/suffix, {"k":""} for empty wildcard/equals-ignore-case.
5. Separately, call events TestEventPattern with each of the four patterns against each of the six events and compare Result with the AWS verdicts listed in the evidence.
6. Because the matching rounds never go live, the check retries PutEvents up to 16 times and is killed with "Check timed out after 60.0s" without evaluating any assertion.
