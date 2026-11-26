Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: chime-sdk-identity.describe-app-instance-bot-not-found-rejected
Issue-Shape-Id: com.amazonaws.chimesdkidentity#ChimeIdentityService
Issue-Key: chime-sdk-identity.describe-app-instance-bot-not-found-rejected|run#6|AssertionError|assert error["Code"] == "ForbiddenException", f"expected ForbiddenException, got {error!r}"
Issue-Commit: 352f4536081af191daff3bc92f8d9fe5d775df3a

# DescribeAppInstanceBot answers 404 NotFoundException for a well-formed AppInstanceBotArn that names no bot, where AWS refuses the read with 403 ForbiddenException

> _Note: this issue was identified against the code base at commit `352f4536081af191daff3bc92f8d9fe5d775df3a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAppInstanceBot

## Description
Cause 1 of 1: DescribeAppInstanceBot classifies "this ARN names no AppInstanceBot I hold" as a not-found report, where AWS refuses the read as a request the caller may not make — and in doing so echoes back the ARN AWS deliberately declines to say anything about. Failing check: chime-sdk-identity.describe-app-instance-bot-not-found-rejected.

NOTE ON PRIOR ART: the open issue 20261125T095120Z-chime-sdk-identity-describeappinstancebot-on-a-well-formed-appinstancebotarn-that-names describes this same handler branch, but the replay suite does not report this check as OWNED by it (the check carries no ownership marking at all), so the divergence is filed here with the failing check id as its replay key. If a triager confirms that issue already owns this check, close this one as its duplicate; the two must not be fixed twice. The nearby open issues are NOT this defect: 20261122T042546Z-chime-sdk-identity-getappinstanceretentionsettings-error-response concerns a handler that never calls unresolved_arn at all (this handler does call it and mis-routes one outcome), and 20261120T131514Z-chime-sdk-meetings-getattendee-error-response is the inverse direction in a different service.

AWS: recorded behavior only for the choice between the two classes. The Smithy model (services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/smithy-model/com-amazonaws-chimesdkidentity-chimeidentityservice.json) declares BOTH errors on this operation — DescribeAppInstanceBot: GET /app-instance-bots/{AppInstanceBotArn}, success 200; errors ForbiddenException (client, httpError 403), NotFoundException (client, httpError 404), plus BadRequestException 400, UnauthorizedClientException 401, ThrottledClientException 429, ServiceFailureException 500, ServiceUnavailableException 503 — so the model alone does not say which class an absent bot earns. The recording settles it: for a syntactically valid AppInstanceBotArn under a LIVE AppInstance that names no AppInstanceBot, Chime SDK Identity answers HTTP 403 with Code "ForbiddenException" and Message "You are not allowed to make this request." The whole sentence is FIXED text: it interpolates nothing from the request or from state, and in particular never names the ARN that was addressed. The refusal is per-resource, not a credential failure — the parent AppInstance in the same request stays readable (DescribeAppInstance on it answers 200) and ListAppInstanceBots on it answers 200 with no such bot.

Emulator: for the same input it reports absence instead — HTTP 404 with Code "NotFoundException" and Message "AppInstanceBot <AppInstanceBotArn> does not exist", a message shape AWS never serves for this operation, whose only variable part is the ARN copied straight out of the request path. The handler reaches this deliberately: on a bot-lookup miss it consults state.unresolved_arn(..) and routes only the DeletedAppInstance outcome to ForbiddenException (with the crate's own correct forbidden-request message constant), while the NeverExisted outcome becomes the 404. AWS serves the 403 for the NeverExisted outcome too, so the branch selection, not the helper, is what diverges.

Why: a client branching on the error code takes the opposite branch. Code "NotFoundException" with 404 reads as "the resource is absent, the call itself was permitted", so a caller probing for a bot treats the answer as authoritative absence and proceeds to create/retry logic; against AWS the same call yields 403 ForbiddenException, which SDK error handling and IAM-aware retry/alarm logic classify as an authorization refusal and never as absence. It is also an information-disclosure divergence of the kind AWS avoids on purpose: the emulator confirms — and echoes — an ARN AWS says nothing about, so bot ARNs can be enumerated against the emulator and cannot against AWS.

Fix: the absence branch of the DescribeAppInstanceBot handler's refusal selection — the arm that maps the "never existed" unresolved-ARN outcome to NotFoundException must instead refuse with the modelled ForbiddenException carrying the crate's existing forbidden-request message constant, the same refusal its deleted-parent sibling arm already mints, so an unresolvable bot ARN is refused identically however it came to be unresolvable. The ARN-interpolating "AppInstanceBot ... does not exist" sentence must not be served by this operation at all, and the handler comment stating that an unheld bot ARN is "reported as ABSENT" has to change with it.

Traces: chime-sdk-identity.describe-app-instance-bot-not-found-rejected. Code citations: services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/describe_app_instance_bot.rs (the two-arm refusal on the bot-lookup miss, around lines 156-189) and the helper plus message constant it uses in services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/state.rs (unresolved_arn, the UnresolvedArn enum, and FORBIDDEN_REQUEST_MESSAGE).

## Evidence
Failing check: chime-sdk-identity.describe-app-instance-bot-not-found-rejected (verdict: mismatch; group "overly-permissive"; tags axis:rejection, cap:c1, concern:negative-classes, plane:control).

Assertion that failed, from the replay output:
    assert error["Code"] == "ForbiddenException", f"expected ForbiddenException, got {error!r}"
    AssertionError: expected ForbiddenException, got {'Message': 'AppInstanceBot arn:aws:chime:us-east-1:<account>:app-instance/<instance-id>/bot/<bot-id> does not exist', 'Code': 'NotFoundException'}

Expected (AWS, as the check pins it):
  HTTP 403; Code "ForbiddenException"; Message "You are not allowed to make this request." — the entire sentence is fixed text, with no interpolation of the request or of state, and no mention of the addressed ARN.

Actual (emulator):
  HTTP 404; Code "NotFoundException"; Message "AppInstanceBot <AppInstanceBotArn> does not exist" — fixed text "AppInstanceBot " + " does not exist" around one variable part, the AppInstanceBotArn taken verbatim from the request path. (The account id, instance id and bot id inside that ARN are per-run values of this one recording, not part of any requirement.)

The check's two controls both PASS, which localises the divergence to the refusal class/status/message alone: ListAppInstanceBots on the parent AppInstanceArn lists no bot with that ARN, and DescribeAppInstance on the parent AppInstanceArn answers 200 with that AppInstanceArn — the same credentials are plainly able to make requests against this instance, so AWS's 403 here is a per-resource refusal and not an auth failure.

Smithy (model_path services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/smithy-model/com-amazonaws-chimesdkidentity-chimeidentityservice.json):
  DescribeAppInstanceBot: http GET /app-instance-bots/{AppInstanceBotArn}, success code 200; output AppInstanceBot; errors ForbiddenException (errorKind client, httpError 403) and NotFoundException (errorKind client, httpError 404) are BOTH modelled on the operation, so the handler is choosing between two modelled classes and the recording decides which.
  Input member AppInstanceBotArn: target ChimeArn, required, httpLabel, @length 5..1600, @pattern ^arn:[a-z0-9-\.]{1,63}:[a-z0-9-\.]{0,63}:[a-z0-9-\.]{0,63}:[a-z0-9-\.]{0,63}:[^/].{0,1023}$ — the ARN the check sends satisfies the pattern and the length bounds, so the request is well-formed and no input validation is at issue; BadRequestException is correctly not what is served.

Emulator source: services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/describe_app_instance_bot.rs — on `state.bot(&input.app_instance_bot_arn)` returning None the handler matches `state.unresolved_arn(&input.app_instance_bot_arn)`, mapping UnresolvedArn::DeletedAppInstance to ForbiddenException with state::FORBIDDEN_REQUEST_MESSAGE, and UnresolvedArn::NeverExisted to NotFoundException with format!("AppInstanceBot {} does not exist", input.app_instance_bot_arn). The handler's own comment states the intent ("an AppInstanceBotArn this service holds no bot for is reported as ABSENT", with the 403 "reserved" for an ARN under a DELETED AppInstance), so the divergence is a deliberate design choice the recording contradicts. The correct text already exists in the crate as state.rs's FORBIDDEN_REQUEST_MESSAGE = "You are not allowed to make this request.", which the sibling arm already uses.

## Steps to Reproduce
1. Address Chime SDK Identity's control plane in us-east-1. POST /app-instances with a Name, Metadata "" and a fresh ClientRequestToken; keep the returned AppInstanceArn.
2. Build a well-formed but non-existent bot ARN under that LIVE instance: <AppInstanceArn>/bot/<a fresh UUID>. Create no bot.
3. GET /app-instance-bots/<that ARN> (DescribeAppInstanceBot).
   Expected (AWS): HTTP 403, body Code "ForbiddenException", Message "You are not allowed to make this request." — the ARN is not mentioned.
   Actual (emulator): HTTP 404, body Code "NotFoundException", Message "AppInstanceBot <that ARN> does not exist".
4. Controls that already behave correctly and must keep doing so after the fix: GET /app-instances/<AppInstanceArn>/bots lists no bot with that ARN, and GET /app-instances/<AppInstanceArn> answers 200 with that AppInstanceArn.
5. Contrast with the arm that is already right: DELETE /app-instances/<AppInstanceArn>, then repeat step 3 against the same bot ARN — the emulator answers 403 ForbiddenException "You are not allowed to make this request." AWS answers that same 403 in BOTH cases; the emulator only serves it once the parent is deleted.
6. Re-replay chime-sdk-identity.describe-app-instance-bot-not-found-rejected to confirm the fix.
