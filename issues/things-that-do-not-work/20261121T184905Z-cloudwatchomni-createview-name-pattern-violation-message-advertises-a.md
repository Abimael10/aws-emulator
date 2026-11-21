Issue-Source: trace-sweep-divergence
Issue-Trace: cloudwatchomni-view-definition-statement-and-name-rules
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudwatchomni-view-definition-statement-and-name-rules
Issue-Shape-Id: com.amazonaws.cloudwatchomni#CloudWatchOmniFrontend
Issue-Key: cloudwatchomni|CreateView|4xx->4xx|$.message
Issue-Commit: f730cfd0671b3874b510ab2deffec753a1dc2251

# cloudwatchomni: CreateView name pattern-violation message advertises a regex narrower than AWS's

> _Note: this issue was identified against the code base at commit `f730cfd0671b3874b510ab2deffec753a1dc2251`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateView

## Description
ONE net-new root cause in the cloudwatchomni emulator: the 400 ValidationException the emulator returns when CreateView's `name` violates its pattern constraint quotes a regular expression that is not the one AWS advertises for that member — the character class for the characters after the "view." prefix is missing the dot.

Related but NOT covering this: open issue 20261111T022337Z-cloudwatchomni-conflict-and-validation-error-messages-diverge-from.md bundles five cloudwatchomni message-wording causes, including a CreateView name-conflict message, a CreateView definition message and an UpdateDomain @pattern message whose defects are the spliced "Value '<input>' at" clause and un-stripped ^/$ anchors. Neither that issue nor open issue 20261111T022337Z-cloudwatchomni-account-region-singleton-rules-unenforced-and-name.md carries this cause: here the value clause is already absent and the anchors are already stripped correctly, and the single diverging element is the body of the regex itself, which comes from the member's constraint and not from the shared formatter.

Cause 1 of 1: CreateView name pattern-violation message renders a pattern missing the '.' character.
  AWS: Smithy CreateView declares ValidationException (errorKind client, @httpError 400); CreateViewRequest.name is required and targets ViewName (@length 6..256, @pattern ^view\\.[a-z0-9][a-z0-9_-]{0,250}$, doc "User view name: must start with \"view.\" prefix followed by lowercase alphanumeric, hyphens, and underscores"). The message template is recorded behavior only: "<n> validation error detected: Value at '<memberName>' failed to satisfy constraint: Member must satisfy regular expression pattern: <pattern>", where the error count and the member name come from the request's failing members and <pattern> is the member's pattern rendered WITHOUT the ^ and $ anchors. For this member the recording shows AWS rendering that pattern as view\\.[a-z0-9][a-z0-9._-]{0,250} — i.e. the live service's ViewName constraint permits '.' among the trailing characters, so the on-disk model (and its prose) is narrower than the service it models. The rejected value itself is NOT part of the template.
  Emulator: it answers the correct 400 / ValidationException and the correct surrounding fixed text (no "Value '<input>' at" clause, anchors stripped), but renders the pattern as view\\.[a-z0-9][a-z0-9_-]{0,250} — the dot is absent from the character class, so the advertised regex differs from AWS's by exactly that one character.
  Why: a client that exact-matches the validation message fails, and — worse — a client that compiles the regex out of the message to pre-validate names locally derives a stricter rule than AWS enforces and rejects names such as "view.sales.weekly" that the real service accepts. Hypothesis (neither the model nor this recording states it): because the emulator renders the same constraint it enforces, its CreateView admission check is presumably also rejecting dotted names AWS accepts; the recorded step only proves the message text diverges.
  Fix: CreateView's input validation for `name` (and the ViewName constraint it is derived from) — the pattern used both to admit the name and to render into the constraint-violation message must be the one AWS advertises, with '.' included in the trailing character class. Rendering of the surrounding template is already correct and must not change.
  Traces: cloudwatchomni-view-definition-statement-and-name-rules#task#3.

## Evidence
Fixture cloudwatchomni-view-definition-statement-and-name-rules, step task#3, operation CreateView: status matches (400 expected, 400 actual) and $.__type matches; only $.message diverges.
  expected: "1 validation error detected: Value at 'name' failed to satisfy constraint: Member must satisfy regular expression pattern: view\\.[a-z0-9][a-z0-9._-]{0,250}"
  actual:   "1 validation error detected: Value at 'name' failed to satisfy constraint: Member must satisfy regular expression pattern: view\\.[a-z0-9][a-z0-9_-]{0,250}"
Fixed text of the template: "<n> validation error detected: Value at '", "' failed to satisfy constraint: Member must satisfy regular expression pattern: ". Variable parts: the error count (1 here, because one member failed) and the member name ('name') come from the request; the regex comes from the member's modeled constraint. The single divergence is "[a-z0-9._-]" vs "[a-z0-9_-]" inside that regex.
Smithy cited: CreateView errors include ValidationException (errorKind client, @httpError 400); CreateViewRequest.name required, target com.amazonaws.cloudwatchomni#ViewName, @length min 6 max 256, @pattern ^view\\.[a-z0-9][a-z0-9_-]{0,250}$. The model's pattern lacks the '.' that the recorded AWS response advertises, so the model is stale relative to the wire and the recording is the authority here.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the wire corpus simparity-wire-aws-v1 against it.
3. Replay fixture cloudwatchomni-view-definition-statement-and-name-rules, step task#3: call CreateView with a `name` that violates the ViewName pattern (the fixture's name fails the pattern constraint and no other member fails).
4. Expect 400 with __type com.amazonaws.cloudwatchomni#ValidationException and message "1 validation error detected: Value at 'name' failed to satisfy constraint: Member must satisfy regular expression pattern: view\\.[a-z0-9][a-z0-9._-]{0,250}" — note the '.' inside the trailing character class and the absence of ^/$ anchors.
5. Observe the emulator emits the same sentence with the regex rendered as view\\.[a-z0-9][a-z0-9_-]{0,250} (dot missing).
6. Additionally check (not covered by the recorded step) that CreateView ACCEPTS a name whose trailing characters include a dot, e.g. "view.sales.weekly", since AWS's advertised pattern permits it.
