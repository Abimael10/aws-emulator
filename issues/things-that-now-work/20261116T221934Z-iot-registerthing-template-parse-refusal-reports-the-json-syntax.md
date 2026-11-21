Issue-Source: trace-sweep-divergence
Issue-Trace: iot-register-thing-invalid-template
Issue-Step: task#1
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-register-thing-invalid-template iot-register-thing-missing-parameter-error
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|RegisterThing|4xx->4xx|$.message
Issue-Commit: 7e98a57978f7e128a72280f725fba6f61b82c0f8

# RegisterThing template-parse refusal reports the JSON syntax-error column one position too far right

> _Note: this issue was identified against the code base at commit `7e98a57978f7e128a72280f725fba6f61b82c0f8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterThing

## Description
Cause 1 of 1: RegisterThing's provisioning-template parse failure produces the right status, error type and sentence, but the character position it embeds in that sentence is off by one.

AWS: RegisterThing (Smithy: iot RegisterThing, input member templateBody required, target TemplateBody @length 0..10240, @pattern ^[\s\S]*$; modeled error InvalidRequestException, errorKind client, httpError 400) refuses a templateBody that is not well-formed JSON with HTTP 400, x-amzn-errortype InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/, and the message template "Invalid registration template. Template format error: JSON syntax error at line <line>, column <column>". The Smithy model does not state the message text, so this is recorded behavior only: the recording shows the fixed text "Invalid registration template. Template format error: JSON syntax error at line ", ", column " and the two variable parts <line> and <column>, which are the 1-based line and 1-based column of the FIRST offending character of the submitted templateBody. In the recordings the offending character is the very first character of the body, so AWS reports line 1, column 1.

Emulator: same status (400) and same fixed text, but the column value is one greater than AWS's for the same body — it reports "... JSON syntax error at line 1, column 2" where AWS reports "... JSON syntax error at line 1, column 1". Hypothesis: the parse-error position is taken AFTER the offending byte has been consumed (or a 0-based offset is rendered with +1 applied twice), so every reported column is shifted right by one.

Why: the line/column pair is the only machine-readable hint a client or an operator has for locating the defect in a provisioning template. A client that highlights, slices or logs templateBody at the reported offset points at the character after the real fault; for a single-character or first-character fault the reported column can even run past the body. Template-authoring tooling that asserts on the reported position disagrees with AWS for every malformed template, not just these two.

Fix: the templateBody JSON-parse error path of the RegisterThing handler's validation — map the parser's error position to a 1-based column of the first offending character before formatting the InvalidRequestException message, rather than using the parser's post-consumption cursor. The fixed text and the line value already match; only the column mapping is wrong.

Traces: iot-register-thing-invalid-template#task#1 (representative) and iot-register-thing-missing-parameter-error#task#1 — both expect "... column 1" and both get "... column 2".

## Evidence
Replay cluster service=iot|RegisterThing|status=400->400|keys=$.message (verdict: mismatch, expected_status 400, actual_status 400, affected_count 2).

$.message: value_mismatch
  expected: 'Invalid registration template. Template format error: JSON syntax error at line 1, column 1'
  actual:   'Invalid registration template. Template format error: JSON syntax error at line 1, column 2'

Status (400), x-amzn-errortype and all other response members matched; the sole diverging key is $.message, and within it only the column number.

Fixed text (requirement): 'Invalid registration template. Template format error: JSON syntax error at line ' + <line> + ', column ' + <column>.
Variable parts (taken from the request): <line> and <column> = 1-based position of the first offending character of the submitted templateBody. '1'/'1' in these recordings is the value for a fault at the first character, not a literal requirement.

Affected steps: iot-register-thing-invalid-template#task#1, iot-register-thing-missing-parameter-error#task#1.

## Steps to Reproduce
1. Point an AWS SDK / CLI at the emulator's iot endpoint.
2. Call RegisterThing with a templateBody that is not well-formed JSON from its first character (e.g. a body whose first character cannot start a JSON value), as fixture iot-register-thing-invalid-template step task#1 does.
3. Observe HTTP 400 with x-amzn-errortype InvalidRequestException and message '... JSON syntax error at line 1, column 2'.
4. AWS answers the same status and error type with '... JSON syntax error at line 1, column 1' — the 1-based column of the first offending character.
5. Replay fixtures iot-register-thing-invalid-template (task#1) and iot-register-thing-missing-parameter-error (task#1) to confirm both clear.
