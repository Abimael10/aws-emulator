Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: endusermessaging.create-notify-code-configuration-client-token-replay-and-mismatch endusermessaging.notify-code-template-must-contain-code-placeholder endusermessaging.notify-code-configuration-lookup-by-arn-and-rejects-malformed-id endusermessaging.update-notify-code-configuration-out-of-range-parameters-leave-config-unchanged
Issue-Shape-Id: com.amazonaws.endusermessaging#EndUserMessaging
Issue-Key: endusermessaging.create-notify-code-configuration-client-token-replay-and-mismatch|_error#10|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: endusermessaging.notify-code-template-must-contain-code-placeholder|_error#3|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: endusermessaging.notify-code-configuration-lookup-by-arn-and-rejects-malformed-id|run#11|AssertionError|assert code == "ValidationException", f"expected ValidationException, got {code}: {message!r}"
Issue-Key: endusermessaging.update-notify-code-configuration-out-of-range-parameters-leave-config-unchanged|run#5|AssertionError|assert message == (
Issue-Commit: 2f00469c6b1c115e8270b5c2778d03323e9ec131

# endusermessaging notify-code-configurations: 4 net-new causes — clientToken replay with different parameters accepted, missing {{code}} template gate, id-format gate missing on lookup, and re-worded @range refusal

> _Note: this issue was identified against the code base at commit `2f00469c6b1c115e8270b5c2778d03323e9ec131`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateNotifyCodeConfiguration

## Description
Four net-new root causes in the endusermessaging notify-code-configuration surface, all observed replaying the behavioral suite simparity-behavioral-aws-v1 against the running (bootstrapped) emulator. 4 causes must be closed before this issue is submitted.

Related but NOT covering issues (named here so a fixer can share the renderer/gate work rather than duplicate it): the open wire issue '20261104T183251Z-endusermessaging-input-validation-refusals-omit-fieldlist-and-diverge.md' describes the same CLASSES of defect for OTHER operations — its cause 3 is the id-format gate on GetBrandProfile (not GetNotifyCodeConfiguration), its cause 4 is the idempotency-mismatch check on CreateBrandProfile (not CreateNotifyCodeConfiguration), and its cause 2 is the @range envelope observed on CreateNotifyCodeConfiguration (not the UpdateNotifyCodeConfiguration path below); '20261111T021035Z-endusermessaging-brand-profile-validation-fieldlist-omitted-length-range.md' is brand-profile only. The replay report marks none of the four checks below as OWNED or ALREADY FILED.

=== Cause 1 of 4: a replayed clientToken whose parameters differ is served the original configuration (200) instead of the idempotency-mismatch refusal ===
AWS: Smithy: CreateNotifyCodeConfiguration.clientToken targets ClientToken with @idempotencyToken (@length 1..128, @pattern ^[\x21-\x7E]+$, @sensitive), documented as 'A unique, case-sensitive identifier that you provide to ensure the idempotency of the request.' The operation models ValidationException (HTTP 400). A replay carrying the SAME clientToken with parameters that differ from the request the token first served is REFUSED with 400 ValidationException and the message 'The idempotency token and parameters presented do not match' — that sentence is entirely fixed text, with no variable part. Only a replay whose parameters match is served the original resource (that part of the check passes today). Nothing is created by the refused call.
Emulator: services/.../src/ops/create_notify_code_configuration.rs lines 182-192 look the token up in the store and, when a configuration was minted by it, return that configuration's view verbatim without comparing the incoming notifyCodeConfigurationName / codeConfigurationParameters / channelParameters / deletionProtectionEnabled. The mismatched replay answers HTTP 200 with the first configuration's body (same notifyCodeConfigurationId/Arn and the FIRST name), no Error.Code and no message.
Why: the sole purpose of an idempotency token is to detect that a caller reused it for a DIFFERENT request. A client that mutates its request but forgets to mint a fresh token is told its create succeeded and is handed a configuration whose name and passcode policy are the OLD ones, so it proceeds believing its new parameters are in effect; against AWS it would get a 400 telling it to mint a new token.
Fix: CreateNotifyCodeConfiguration's handler and its idempotency bookkeeping in state — record the request fingerprint alongside the token and refuse a replay whose parameters differ with the 400 ValidationException idempotency-mismatch message, serving the stored configuration only on an exact parameter match.
Traces: check endusermessaging.create-notify-code-configuration-client-token-replay-and-mismatch.

=== Cause 2 of 4: a template body without the {{code}} placeholder is accepted on create and on update instead of refused ===
AWS: Smithy: ChannelParameters.text targets TextParameters; TextParameters.inlineTemplateBody is documented 'The freeform message template used to render the one-time passcode for the SMS or RCS channels. The template must contain the code placeholder.' (same documented requirement on the voice channel, and on UpdateTextParameters/UpdateVoiceParameters for the update path). A create or update whose supplied text/voice inlineTemplateBody omits the {{code}} placeholder is refused with 400 ValidationException and the message 'template body must contain {{code}} placeholder' — entirely fixed text, no variable part. A body that carries the placeholder is stored, and a refused update must leave the stored channelParameters untouched.
Emulator: no placeholder gate exists in either handler. CreateNotifyCodeConfiguration with channelParameters {"text": {"inlineTemplateBody": "Your sign-in code"}} answers HTTP 200 and persists the placeholder-less body (the response echoes channelParameters.text.inlineTemplateBody unchanged); the equivalent UpdateNotifyCodeConfiguration voice body is likewise not refused by a placeholder check.
Why: the configuration is accepted but can never render a passcode — the defect only surfaces later, at send time, as an undeliverable or placeholder-less message. A client that relies on AWS refusing the bad template at configuration time ships a silently broken OTP configuration, and its own negative test for the refusal never fires.
Fix: shared channel-parameter validation for CreateNotifyCodeConfiguration and UpdateNotifyCodeConfiguration (ahead of the state write) — reject any supplied text/voice inlineTemplateBody lacking the {{code}} placeholder with ValidationException, and ensure the refusal happens before the record is mutated.
Traces: check endusermessaging.notify-code-template-must-contain-code-placeholder.

=== Cause 3 of 4: a syntactically invalid notifyCodeConfigurationId is looked up instead of validated, so the refusal is 404 ResourceNotFoundException instead of 400 ValidationException ===
AWS: Smithy: GetNotifyCodeConfiguration.notifyCodeConfigurationId is an @httpLabel targeting NotifyCodeConfigurationIdOrArn (@length 1..256, @pattern ^[A-Za-z0-9_:/-]+$, documented 'System-generated NotifyCodeConfiguration identifier or full ARN ... Accepts either a bare id or a full ARN'), and the operation models both ValidationException (400) and ResourceNotFoundException (404). A value that satisfies the member pattern but is not a well-formed notify-code-configuration identifier or ARN is rejected by the service's own id-format gate BEFORE any store lookup: 400 ValidationException with message 'invalid notify code configuration ID format: <value>' — fixed text 'invalid notify code configuration ID format: ' plus the identifier taken verbatim from the request path. 404 ResourceNotFoundException with 'Resource not found - ResourceType="notify-code-configuration" ResourceId="<id>"' is reserved for a WELL-FORMED id that no longer resolves (that part of the check passes today).
Emulator: services/.../src/ops/get_notify_code_configuration.rs lines 160-172 pass the raw label straight to resolve_notify_code_configuration_id and, on a miss, return ResourceNotFoundException. A configuration NAME passed as the identifier therefore comes back as 404 ResourceNotFoundException with message 'Resource not found - ResourceType="notify-code-configuration" ResourceId="<the name>"' instead of 400 ValidationException.
Why: a client cannot distinguish 'you sent a malformed identifier' (non-retryable: fix the request) from 'the resource is gone' (retry or recreate). SDK error-class branching and retry policy differ between 400 ValidationException and 404 ResourceNotFoundException, so a caller that passed a name where an id belongs loops or recreates instead of being told its request is malformed.
Fix: GetNotifyCodeConfiguration's validation, ahead of the state lookup — apply an id/ARN format gate wherever NotifyCodeConfigurationIdOrArn is accepted and raise ValidationException with the id-format message before resolving against the store.
Traces: check endusermessaging.notify-code-configuration-lookup-by-arn-and-rejects-malformed-id.

=== Cause 4 of 4: the @range refusal on the UPDATE path re-words AWS's envelope (colon, echoed value, dotted path, one-sided sentence) ===
AWS: Smithy: UpdateNotifyCodeConfiguration.codeConfigurationParameters targets UpdateCodeConfigurationParameters, whose codeLength has @range min 4 max 8 ('Valid values range from 4 through 8') and validityPeriodMinutes @range min 1 max 60 ('Valid values range from 1 through 60'); the operation models ValidationException (400). For a range violation AWS answers 400 with message '<N> validation error detected. Value at '/codeConfigurationParameters/<member>' failed to satisfy constraint: Member must be between <min> and <max>, inclusive' — fixed text '<N> validation error detected. Value at ', "' failed to satisfy constraint: Member must be between ", ' and ', ', inclusive'; the variable parts are the failure count, the member's JSON POINTER path and the model's @range bounds (4 and 8 for codeLength, 1 and 60 for validityPeriodMinutes). The submitted value is NOT echoed. The refused update must leave the stored codeConfigurationParameters unchanged.
Emulator: answers 400 (correct status and ValidationException code) with '1 validation error detected: Value '<v>' at 'codeConfigurationParameters.codeLength' failed to satisfy constraint: Member must have value less than or equal to 8' — a colon instead of a period after 'detected', the rejected value interpolated, a dotted member path instead of a JSON pointer, and a one-sided 'less than or equal to <max>' sentence where AWS uses the two-sided inclusive-range wording for a two-sided @range.
Why: a client that classifies a refusal by its constraint sentence, or surfaces the valid range to a user, never sees the documented 4..8 / 1..60 inclusive wording and cannot map the dotted path back to the request member; the echoed value also places submitted input into the summary message where AWS does not. Note this is the UpdateNotifyCodeConfiguration path — the open wire issue 20261104T183251Z records the same envelope defect only for CreateNotifyCodeConfiguration, so fixing it in the shared renderer should close both.
Fix: the service's shared input-constraint validation / ValidationException renderer, reached from UpdateNotifyCodeConfiguration — render AWS's two-sided inclusive sentence with the nested member's JSON-pointer path, drop the echoed value, use a period after 'detected', and keep the refusal ahead of the state write.
Traces: check endusermessaging.update-notify-code-configuration-out-of-range-parameters-leave-config-unchanged.

## Evidence
Cause 1 — check endusermessaging.create-notify-code-configuration-client-token-replay-and-mismatch [mismatch]:
  Step: CreateNotifyCodeConfiguration{name=A, clientToken=T} -> 200; replay with the SAME token and name A -> 200 same id (correct); third call CreateNotifyCodeConfiguration{name=B, clientToken=T}.
  Expected: 400 ValidationException, Error.Message 'The idempotency token and parameters presented do not match' (all fixed text), and no second configuration in ListNotifyCodeConfigurations.
  Actual: HTTP 200 with notifyCodeConfiguration of the FIRST create — AssertionError: expected the call to be refused, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, ...}, 'notifyCodeConfiguration': {'notifyCodeConfigurationId': 'ncc-...', 'notifyCodeConfigurationName': '<the first name>', ...}}. The id, name and timestamps in that body are this run's generated values, not part of the requirement.
  Source: services/service-com-amazonaws-endusermessaging-endusermessaging/src/ops/create_notify_code_configuration.rs:182-192 — notify_code_configuration_minted_by(token) returns the stored view with no parameter comparison.

Cause 2 — check endusermessaging.notify-code-template-must-contain-code-placeholder [mismatch]:
  Step: CreateNotifyCodeConfiguration{notifyCodeConfigurationName=<valid>, channelParameters={"text": {"inlineTemplateBody": "Your sign-in code"}}} (no {{code}}).
  Expected: 400 ValidationException, Error.Message 'template body must contain {{code}} placeholder' (all fixed text). Same refusal for UpdateNotifyCodeConfiguration{channelParameters={"voice": {"inlineTemplateBody": "Your code follows"}}}, with the stored channelParameters left at the previously accepted value.
  Actual: HTTP 200 — AssertionError: expected the call to be refused, but it succeeded: ... 'channelParameters': {'text': {'inlineTemplateBody': 'Your sign-in code'}} ... — the placeholder-less template is persisted and echoed back.

Cause 3 — check endusermessaging.notify-code-configuration-lookup-by-arn-and-rejects-malformed-id [mismatch]:
  Step: GetNotifyCodeConfiguration with notifyCodeConfigurationId set to the configuration's NAME (pattern-legal under ^[A-Za-z0-9_:/-]+$, but not an id or ARN).
  Expected: 400 ValidationException, Error.Message 'invalid notify code configuration ID format: <value>' — fixed text 'invalid notify code configuration ID format: ' plus the identifier from the request path.
  Actual: AssertionError: expected ValidationException, got ResourceNotFoundException: 'Resource not found - ResourceType="notify-code-configuration" ResourceId="<the name>"' (HTTP 404). The name string in the message is this run's generated value.
  Lookup by id and by ARN agreeing, and the 404 for a DELETED well-formed id, both pass — only the malformed-identifier class diverges.
  Source: services/service-com-amazonaws-endusermessaging-endusermessaging/src/ops/get_notify_code_configuration.rs:160-172.

Cause 4 — check endusermessaging.update-notify-code-configuration-out-of-range-parameters-leave-config-unchanged [mismatch]:
  Step: UpdateNotifyCodeConfiguration{notifyCodeConfigurationId=<id>, codeConfigurationParameters={"codeLength": 9}} (and then {"validityPeriodMinutes": 61}).
  Expected: 400 ValidationException, Error.Message "1 validation error detected. Value at '/codeConfigurationParameters/codeLength' failed to satisfy constraint: Member must be between 4 and 8, inclusive" (and '/codeConfigurationParameters/validityPeriodMinutes' ... 'between 1 and 60, inclusive').
  Actual: AssertionError: codeLength: unexpected message "1 validation error detected: Value '9' at 'codeConfigurationParameters.codeLength' failed to satisfy constraint: Member must have value less than or equal to 8".
  Fixed vs variable: '<N> validation error detected. Value at ' and "' failed to satisfy constraint: Member must be between " / ', inclusive' are fixed; the JSON pointer comes from the request member and 4/8 and 1/60 are the model's @range bounds. '9' is this request's submitted value, which AWS does not echo.

## Steps to Reproduce
Cause 1:
1. POST /v1/notify-code-configurations {"notifyCodeConfigurationName": "<name-A>", "clientToken": "<T>"} -> 200, note notifyCodeConfigurationId.
2. Repeat the identical request -> 200 with the same id (correct today).
3. POST /v1/notify-code-configurations {"notifyCodeConfigurationName": "<name-B>", "clientToken": "<T>"}.
4. Emulator: 200 with the name-A configuration. AWS: 400 ValidationException 'The idempotency token and parameters presented do not match', and ListNotifyCodeConfigurations still shows exactly one configuration.

Cause 2:
1. POST /v1/notify-code-configurations {"notifyCodeConfigurationName": "<valid>", "channelParameters": {"text": {"inlineTemplateBody": "Your sign-in code"}}}.
2. Emulator: 200, template stored. AWS: 400 ValidationException 'template body must contain {{code}} placeholder'.
3. Create one WITH the placeholder, then PUT /v1/notify-code-configurations/<id> {"channelParameters": {"voice": {"inlineTemplateBody": "Your code follows"}}} — AWS refuses the same way and leaves the stored channelParameters unchanged.

Cause 3:
1. Create a configuration and note its notifyCodeConfigurationName.
2. GET /v1/notify-code-configurations/<that name>.
3. Emulator: 404 ResourceNotFoundException 'Resource not found - ResourceType="notify-code-configuration" ResourceId="<name>"'. AWS: 400 ValidationException 'invalid notify code configuration ID format: <name>'.

Cause 4:
1. Create a configuration with codeConfigurationParameters {"codeType": "NUMERIC", "codeLength": 6, "validityPeriodMinutes": 10, "maxAttempts": 3}.
2. PUT /v1/notify-code-configurations/<id> {"codeConfigurationParameters": {"codeLength": 9}}; then the same with {"validityPeriodMinutes": 61}.
3. Compare Error.Message to AWS's "1 validation error detected. Value at '/codeConfigurationParameters/codeLength' failed to satisfy constraint: Member must be between 4 and 8, inclusive" (resp. 'between 1 and 60, inclusive'); the emulator uses ':' after 'detected', echoes the value, uses a dotted path and a one-sided sentence.
4. GET the configuration: the refused updates must have left codeConfigurationParameters unchanged.
