Issue-Source: trace-sweep-divergence
Issue-Trace: endusermessaging-notify-code-configuration-validation-and-idempotency
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: endusermessaging-notify-code-configuration-validation-and-idempotency
Issue-Shape-Id: com.amazonaws.endusermessaging#EndUserMessaging
Issue-Key: endusermessaging|CreateNotifyCodeConfiguration|4xx->2xx|$.message
Issue-Key: endusermessaging|CreateNotifyCodeConfiguration|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: endusermessaging|CreateNotifyCodeConfiguration|4xx->2xx|status
Issue-Key: endusermessaging|UpdateNotifyCodeConfiguration|4xx->4xx|$.fieldList
Issue-Key: endusermessaging|UpdateNotifyCodeConfiguration|4xx->4xx|$.message
Issue-Key: endusermessaging|GetNotifyCodeConfiguration|4xx->4xx|$.fieldList
Issue-Key: endusermessaging|GetNotifyCodeConfiguration|4xx->4xx|$.message
Issue-Key: endusermessaging|GetNotifyCodeConfiguration|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: endusermessaging|GetNotifyCodeConfiguration|4xx->4xx|status
Issue-Commit: 87fb877f34cb08a6e89ae1853605cffbee8a1e9e

# endusermessaging notify-code-configuration refusals diverge: template gate, id-format gate and @range envelope

> _Note: this issue was identified against the code base at commit `87fb877f34cb08a6e89ae1853605cffbee8a1e9e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateNotifyCodeConfiguration

## Description
Three net-new root causes on the endusermessaging notify-code-configuration REFUSAL paths, all observed replaying the wire corpus simparity-wire-aws-v1 against the running, bootstrapped emulator (the service answers: other steps of the same fixtures pass). 3 causes must be closed before this issue is submitted.

Related but NOT covering issues, named so a fixer can share the renderer/gate work instead of duplicating it: the open BEHAVIORAL issue '20261115T011048Z-endusermessaging-notify-code-configurations-4-net-new-causes.md' describes the same three classes of defect from the behavioral suite, but its Issue-Source is the behavioral corpus and it carries NO wire Issue-Trace/Issue-Step, so none of the wire steps below are gated by it (the replay report reports all three clusters unowned, with no stub blocking them). '20261104T183251Z-endusermessaging-input-validation-refusals-omit-fieldlist-and-diverge.md' and '20261111T021035Z-endusermessaging-brand-profile-validation-fieldlist-omitted-length-range.md' record the same ValidationException-envelope defect for OTHER operations (CreateNotifyCodeConfiguration's and the brand-profile operations' refusals), not for the UpdateNotifyCodeConfiguration or GetNotifyCodeConfiguration paths below.

=== Cause 1 of 3: a channel template body without the {{code}} placeholder is accepted with 200 instead of refused with 400 ValidationException ===
AWS: Smithy model com.amazonaws.endusermessaging#EndUserMessaging (restJson1). CreateNotifyCodeConfiguration.channelParameters targets ChannelParameters, whose text/voice members target TextParameters/VoiceParameters with inlineTemplateBody (InlineTemplateBody, @length 1..6000, @pattern ^[\s\S]*\S[\s\S]*$, @sensitive); the member is documented as the freeform template used to render the one-time passcode and states the template must contain the code placeholder. The operation models ValidationException (HTTP 400, httpError defaulted). A create whose supplied text or voice inlineTemplateBody omits the {{code}} placeholder is refused: HTTP 400, x-amzn-errortype ValidationException, Error.Message 'template body must contain {{code}} placeholder' — that sentence is entirely FIXED text, with no part taken from the request. Nothing is created by the refused call.
Emulator: no placeholder gate exists. services/service-com-amazonaws-endusermessaging-endusermessaging/src/ops/create_notify_code_configuration.rs stores channel_parameters verbatim (lines 221-222, 'Both parameter structures are stored exactly as supplied') and answers HTTP 200 with the placeholder-less body echoed back; the replay reports status expected 400, got 200, with $.message and $headers.x-amzn-errortype both missing from the response.
Why: a client is told its OTP configuration was created when the configuration can never render a passcode — the breakage only surfaces later, at send time, as a message with no code in it. A caller whose negative test expects AWS's 400 at configuration time sees a success, and ships a silently broken configuration.
Fix: shared channel-parameter validation for CreateNotifyCodeConfiguration (and the same gate on the update path), run BEFORE the state write in the operation's handler/validation layer — refuse any supplied text/voice inlineTemplateBody that lacks the {{code}} placeholder with ValidationException and the fixed message above.
Traces: endusermessaging-notify-code-configuration-validation-and-idempotency#task#1, clustered with #task#4 (same divergence_key service=endusermessaging|CreateNotifyCodeConfiguration|status=400->200).

=== Cause 2 of 3: the UpdateNotifyCodeConfiguration @range refusal omits fieldList and re-words AWS's validation sentence ===
AWS: UpdateNotifyCodeConfiguration.codeConfigurationParameters carries codeLength with @range min 4 max 8 and validityPeriodMinutes with @range min 1 max 60 (CodeLength/ValidityPeriodMinutes in the model); the operation's ValidationException (HTTP 400) models BOTH message (@required) and fieldList (ValidationExceptionFieldList of ValidationExceptionField{path, message}). For a constraint violation AWS answers 400 with Error.Message '<N> validation error detected. Value at '<json-pointer>' failed to satisfy constraint: Member must be between <min> and <max>, inclusive' AND a fieldList entry whose path is that same JSON pointer and whose message is the per-member sentence 'Value at '<json-pointer>' failed to satisfy constraint: Member must be between <min> and <max>, inclusive'. Fixed text: '<N> validation error detected. Value at ', ''' failed to satisfy constraint: Member must be between ', ' and ', ', inclusive'. Variable parts: the failure count, the offending member's JSON POINTER path (e.g. /codeConfigurationParameters/codeLength) and the model's own @range bounds (4 and 8 for codeLength, 1 and 60 for validityPeriodMinutes). The submitted value is NOT echoed.
Emulator: answers the right status and error code but renders a different envelope — $.fieldList is absent entirely, and $.message reads '1 validation error detected: Value '<v>' at 'codeConfigurationParameters.codeLength' failed to satisfy constraint: Member must have value less than or equal to 8': a colon instead of a period after 'detected', the rejected value interpolated, a DOTTED member path instead of a JSON pointer, and a one-sided 'less than or equal to <max>' sentence where AWS states the two-sided inclusive range. The '9' in the recorded message is one request's submitted value, not part of the requirement.
Why: fieldList is how an SDK/client maps a refusal back to the specific input member, so a client that walks it sees an empty list and cannot tell which field was rejected; a client that classifies refusals by constraint sentence or surfaces the valid range to a user never sees AWS's documented inclusive 4..8 / 1..60 wording, and cannot translate the dotted path back to a request member.
Fix: the service's shared input-constraint validation / ValidationException renderer as reached from UpdateNotifyCodeConfiguration — populate fieldList with one {path, message} entry per violated member, render AWS's two-sided inclusive sentence with the member's JSON-pointer path, drop the echoed value and use a period after 'detected'. The refusal must stay ahead of any state write.
Traces: endusermessaging-notify-code-configuration-validation-and-idempotency#task#6, clustered with #task#7.

=== Cause 3 of 3: a malformed notifyCodeConfigurationId is looked up instead of format-checked, so GetNotifyCodeConfiguration answers 404 ResourceNotFoundException instead of 400 ValidationException ===
AWS: GetNotifyCodeConfiguration.notifyCodeConfigurationId is an @httpLabel on /v1/notify-code-configurations/{notifyCodeConfigurationId+} targeting the id-or-ARN shape (documented as accepting either a bare id or the resource's full ARN); the operation models ResourceNotFoundException (404) and, service-wide, ValidationException (400). A value that is pattern-legal but is not a well-formed notify-code-configuration id or ARN is rejected by the service's id-format gate BEFORE any store lookup: HTTP 400, errortype ValidationException, Error.Message 'invalid notify code configuration ID format: <value>' and a fieldList entry {path: 'notifyCodeConfigurationId', message: 'invalid notify code configuration ID format: <value>'} — fixed text 'invalid notify code configuration ID format: ' plus the identifier taken verbatim from the request path. 404 ResourceNotFoundException with 'Resource not found - ResourceType="notify-code-configuration" ResourceId="<id>"' is reserved for a WELL-FORMED id that resolves to nothing.
Emulator: services/service-com-amazonaws-endusermessaging-endusermessaging/src/ops/get_notify_code_configuration.rs lines 160-172 pass the raw label straight to resolve_notify_code_configuration_id (src/state.rs:244-252) and, on a miss, return crate::state::notify_code_configuration_not_found — so a malformed identifier comes back as HTTP 404 with x-amzn-errortype ResourceNotFoundException and message 'Resource not found - ResourceType="notify-code-configuration" ResourceId="<value>"', with no fieldList. The identifier string in the recorded step is the probe's value and only fills the message template.
Why: the client cannot distinguish 'your request is malformed, fix it' (400, non-retryable, no resource implied) from 'the resource does not exist' (404, recreate or stop). SDK error-class branching and retry policy differ between the two, so a caller that passed a name or a truncated id where an id belongs concludes the resource was deleted and recreates it instead of correcting its request.
Fix: GetNotifyCodeConfiguration's validation, ahead of the state lookup — apply an id/ARN format gate wherever the id-or-ARN shape is accepted and raise ValidationException (message + fieldList) before resolving against the store, leaving ResourceNotFoundException for well-formed identifiers that miss.
Traces: endusermessaging-notify-code-configuration-validation-and-idempotency#task#9.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200): 19 divergence clusters, 3 of them the causes below (stub_path empty, owned_by empty for all three).

Cause 1 — fixture endusermessaging-notify-code-configuration-validation-and-idempotency step task#1, operation CreateNotifyCodeConfiguration, verdict mismatch:
  status: expected 400, got 200
  $.message: missing (expected 'template body must contain {{code}} placeholder', got None)
  $headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>' i.e. ValidationException, got None)
  affected_count 2: also endusermessaging-notify-code-configuration-validation-and-idempotency#task#4.
  Source: src/ops/create_notify_code_configuration.rs:212-238 — the record is minted and inserted with channel_parameters taken verbatim; no placeholder gate anywhere in the handler.

Cause 2 — fixture endusermessaging-notify-code-configuration-validation-and-idempotency step task#6, operation UpdateNotifyCodeConfiguration, 400 vs 400:
  $.fieldList: missing (expected [{'path': '/codeConfigurationParameters/codeLength', 'message': "Value at '/codeConfigurationParameters/codeLength' failed to satisfy constraint: Member must be between 4 and 8, inclusive"}], got None)
  $.message: value_mismatch (expected "1 validation error detected. Value at '/codeConfigurationParameters/codeLength' failed to satisfy constraint: Member must be between 4 and 8, inclusive", got "1 validation error detected: Value '9' at 'codeConfigurationParameters.codeLength' failed to satisfy constraint: Member must have value less than or equal to 8")
  affected_count 2: also #task#7.
  Fixed vs variable: '<N> validation error detected. Value at ' / "' failed to satisfy constraint: Member must be between " / ', inclusive' are fixed text; the JSON pointer comes from the request member and 4/8 (1/60 for validityPeriodMinutes) are the model's @range bounds. '9' is this request's submitted value, which AWS does not echo.

Cause 3 — fixture endusermessaging-notify-code-configuration-validation-and-idempotency step task#9, operation GetNotifyCodeConfiguration:
  status: expected 400, got 404
  $.fieldList: missing (expected [{'path': 'notifyCodeConfigurationId', 'message': 'invalid notify code configuration ID format: <value>'}], got None)
  $.message: value_mismatch (expected 'invalid notify code configuration ID format: <value>', got 'Resource not found - ResourceType="notify-code-configuration" ResourceId="<value>"')
  $headers.x-amzn-errortype: shape_mismatch (expected ValidationException, got 'ResourceNotFoundException')
  Source: src/ops/get_notify_code_configuration.rs:160-172 with src/state.rs:244-252 and src/state.rs:411-415.

## Steps to Reproduce
Cause 1:
1. POST /v1/notify-code-configurations {"notifyCodeConfigurationName": "<name matching ^[A-Za-z0-9_ -]*[A-Za-z0-9_-][A-Za-z0-9_ -]*$>", "channelParameters": {"text": {"inlineTemplateBody": "Your sign-in code"}}} — a body with NO {{code}} placeholder.
2. Emulator: HTTP 200, the placeholder-less template persisted and echoed. AWS: HTTP 400, x-amzn-errortype ValidationException, message 'template body must contain {{code}} placeholder', and no configuration created.

Cause 2:
1. Create a configuration with codeConfigurationParameters {"codeType": "NUMERIC", "codeLength": 6, "validityPeriodMinutes": 10, "maxAttempts": 3}.
2. PUT /v1/notify-code-configurations/<id> {"codeConfigurationParameters": {"codeLength": 9}} (and again with {"validityPeriodMinutes": 61}).
3. Compare the 400 body to AWS's: fieldList must carry {path '/codeConfigurationParameters/codeLength', message "Value at '/codeConfigurationParameters/codeLength' failed to satisfy constraint: Member must be between 4 and 8, inclusive"} and message must be "1 validation error detected. Value at '/codeConfigurationParameters/codeLength' failed to satisfy constraint: Member must be between 4 and 8, inclusive" (resp. 'between 1 and 60, inclusive'). The emulator omits fieldList and re-words the message.

Cause 3:
1. GET /v1/notify-code-configurations/<a pattern-legal but non-id string, e.g. a configuration name or an arbitrary probe token>.
2. Emulator: 404 ResourceNotFoundException 'Resource not found - ResourceType="notify-code-configuration" ResourceId="<value>"'. AWS: 400 ValidationException 'invalid notify code configuration ID format: <value>' plus the matching fieldList entry on path notifyCodeConfigurationId.
3. A WELL-FORMED id that resolves to nothing must still answer 404 ResourceNotFoundException.

Replay: replay the wire corpus simparity-wire-aws-v1; fixture endusermessaging-notify-code-configuration-validation-and-idempotency, steps task#1 (and task#4), task#6 (and task#7), task#9.
