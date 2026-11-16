Issue-Source: trace-sweep-divergence
Issue-Trace: iot-scheduled-audit-invalid-daymonth-error
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-scheduled-audit-invalid-daymonth-error
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|CreateScheduledAudit|4xx->4xx|$.message
Issue-Commit: ec5051b363c0c99e67e4b7e135f7cf1248204a73

# iot CreateScheduledAudit @pattern refusal drops the offending value and strips anchors from the reported regex

> _Note: this issue was identified against the code base at commit `ec5051b363c0c99e67e4b7e135f7cf1248204a73`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateScheduledAudit

## Description
One cause.

Cause 1 of 1: CreateScheduledAudit rejects an out-of-pattern `dayOfMonth` with the right status (400) and the right error type, but the ValidationException *message* is built from a broken template: it omits the quoted offending value, and it prints the member's regular expression with its leading `^` and trailing `$` removed.

AWS: Smithy model services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json - CreateScheduledAudit input member `dayOfMonth` targets com.amazonaws.iot#DayOfMonth, a string with @pattern `^([1-9]|[12][0-9]|3[01])$|^LAST$`. When a request violates a modeled @pattern, AWS answers HTTP 400 with the standard counted constraint-violation envelope, whose message is exactly:
`<N> validation error detected: Value '<value>' at '<memberName>' failed to satisfy constraint: Member must satisfy regular expression pattern: <pattern>`
Fixed text of that template: `validation error detected: Value `, ` at `, ` failed to satisfy constraint: Member must satisfy regular expression pattern: `. Variable parts, all taken from the request and the model: `<N>` the number of violations in this request; `<value>` the rejected value as the client sent it, single-quoted; `<memberName>` the input member's wire name (`dayOfMonth`); `<pattern>` the member's @pattern trait VERBATIM, anchors included. The recorded response for this step confirms the form: status 400, message `1 validation error detected: Value '99' at 'dayOfMonth' failed to satisfy constraint: Member must satisfy regular expression pattern: ^([1-9]|[12][0-9]|3[01])$|^LAST$` (here `99` is this run's request value and is NOT part of the requirement - the requirement is that the client's value appear, single-quoted, in that slot).

Emulator: answers 400 with message `1 validation error detected: Value at 'dayOfMonth' failed to satisfy constraint: Member must satisfy regular expression pattern: ([1-9]|[12][0-9]|3[01])$|^LAST`. Two defects in one template: (a) the `'<value>' ` slot is absent entirely - the text reads `Value at 'dayOfMonth'`, so the rejected value is never echoed; (b) the pattern is emitted after stripping the first character `^` and the last character `$` of the trait string, which both mangles the regex (`([1-9]|[12][0-9]|3[01])$|^LAST` is a different, unanchored alternation) and makes it not the model's pattern.

Why: clients and SDK test suites parse this message to learn WHICH value was rejected and which rule it broke - the AWS ValidationException body carries no structured field for either, so the message is the only channel. With the value omitted, a caller validating a batch of scheduled-audit definitions cannot tell which input was at fault, and repair logic that quotes the bad value back to the user has nothing to quote. With the anchors stripped, a client that compiles the reported regex to pre-validate subsequent requests gets a pattern that accepts values AWS rejects (anything merely ending in a 1-31 run, or anything containing `LAST`), so it keeps sending requests the real service refuses.

Fix: the shared constraint-violation message formatter used by IoT's input validation - it must interpolate the request's rejected value into the `Value '<value>'` slot, and must emit the @pattern trait string unmodified rather than trimming its anchor characters. The anchor-trimming looks like one shared helper, so the same wrong text will appear for every other @pattern-constrained member in this service; correcting the formatter fixes the class, not just `dayOfMonth`.

Traces: iot-scheduled-audit-invalid-daymonth-error#task#1 (CreateScheduledAudit, expected 400 / actual 400, `$.message` value_mismatch).

## Evidence
Fixture iot-scheduled-audit-invalid-daymonth-error, step task#1, operation CreateScheduledAudit.

Status: expected 400, actual 400 (status and error type agree; only the message diverges).

Expected ($.message), as recorded from AWS:
  1 validation error detected: Value '99' at 'dayOfMonth' failed to satisfy constraint: Member must satisfy regular expression pattern: ^([1-9]|[12][0-9]|3[01])$|^LAST$

Actual ($.message), from the emulator:
  1 validation error detected: Value at 'dayOfMonth' failed to satisfy constraint: Member must satisfy regular expression pattern: ([1-9]|[12][0-9]|3[01])$|^LAST

Diff, split into fixed text vs values:
  - FIXED text that must match exactly: "1 validation error detected: Value " / " at " / " failed to satisfy constraint: Member must satisfy regular expression pattern: ". The emulator drops the `'<value>' ` token that belongs between "Value " and " at ".
  - VALUE from the request: the rejected `dayOfMonth` (in this recording `'99'`) - must be echoed single-quoted; the emulator omits it.
  - VALUE from the request: the member name `dayOfMonth` - correct in the emulator.
  - VALUE from the model: the @pattern of com.amazonaws.iot#DayOfMonth, which smithy_query reports as `^([1-9]|[12][0-9]|3[01])$|^LAST$`. The emulator prints `([1-9]|[12][0-9]|3[01])$|^LAST`, i.e. the leading `^` and trailing `$` have been stripped.

Model citation: smithy_query(query="shape", shape="DayOfMonth", model_path="services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json") -> {"shape_id": "com.amazonaws.iot#DayOfMonth", "type": "string", "pattern": "^([1-9]|[12][0-9]|3[01])$|^LAST$"}.

No open issue covers this: the filed "1 validation error detected" issues are for other services (pca-connector-ad CreateDirectoryRegistration/CreateConnector, bedrock-agentcore-control CreateCapacityProvider, dynamodb, route53 CreateHealthCheck) and none concerns IoT or the anchor-stripping / omitted-value defects in this template.

## Steps to Reproduce
1. Boot the emulator from this checkout and point an IoT client at its data listener.
2. Call CreateScheduledAudit with a `dayOfMonth` that violates com.amazonaws.iot#DayOfMonth's @pattern `^([1-9]|[12][0-9]|3[01])$|^LAST$` - e.g. frequency MONTHLY, dayOfMonth "99", one enabled targetCheckName, and a scheduledAuditName.
3. Observe HTTP 400 with a ValidationException whose message is `1 validation error detected: Value at 'dayOfMonth' failed to satisfy constraint: Member must satisfy regular expression pattern: ([1-9]|[12][0-9]|3[01])$|^LAST`.
4. AWS instead returns `1 validation error detected: Value '<the value you sent>' at 'dayOfMonth' failed to satisfy constraint: Member must satisfy regular expression pattern: ^([1-9]|[12][0-9]|3[01])$|^LAST$` - the rejected value echoed single-quoted, and the model's @pattern reproduced with its anchors.
5. Equivalently, replay fixture iot-scheduled-audit-invalid-daymonth-error and inspect step task#1.
