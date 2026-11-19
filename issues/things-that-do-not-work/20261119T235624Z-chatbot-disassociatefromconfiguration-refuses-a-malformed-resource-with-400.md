Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: chatbot.disassociate-from-configuration-malformed-resource-arn-rejected
Issue-Shape-Id: com.amazonaws.chatbot#WheatleyOrchestration_20171011
Issue-Key: chatbot.disassociate-from-configuration-malformed-resource-arn-rejected|run#4|AssertionError|assert code == "UnauthorizedException", f"unexpected code={code!r}"
Issue-Commit: 7a863d1ad62c4708957aba7eef669d745200ca52

# Chatbot DisassociateFromConfiguration refuses a malformed Resource with 400 InvalidRequestException instead of 403 UnauthorizedException "Unauthorized"

> _Note: this issue was identified against the code base at commit `7a863d1ad62c4708957aba7eef669d745200ca52`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DisassociateFromConfiguration

## Description
Cause 1 of 1: DisassociateFromConfiguration evaluates the input-syntax (ResourceIdentifier @pattern) constraint on `Resource` BEFORE the resource-scoped authorization check, so a `Resource` value that is not a Chatbot custom-action ARN is refused as a constraint violation instead of failing closed as unauthorized. Failing check ids: chatbot.disassociate-from-configuration-malformed-resource-arn-rejected.

AWS: Smithy (com.amazonaws.chatbot, WheatleyOrchestration 2017-10-11) models DisassociateFromConfiguration with input member `Resource` -> ResourceIdentifier, required, @length min 1 / max 1011, @pattern ^arn:aws:chatbot:[A-Za-z0-9_/.-]{0,63}:[A-Za-z0-9_/.-]{0,63}:custom-action/[a-zA-Z0-9_-]{1,64}$ and `ChatConfiguration` -> ChatConfigurationArn, required; errors InternalServiceError (httpError 500, server), InvalidRequestException (httpError 400, client), UnauthorizedException (httpError 403, client); the operation is @idempotent with http {method POST, uri /disassociate-from-configuration, code 204}. The model does NOT state which of the two checks runs first — recorded behavior only for the ordering: AWS runs the resource-scoped authorization decision on the supplied `Resource` first, so a `Resource` that is not an ARN at all is refused with HTTP 403, Error.Code `UnauthorizedException` and Error.Message exactly `Unauthorized`. That message is entirely FIXED text for this error class: it contains no variable part — it does not echo the offending `Resource`, the `ChatConfiguration`, the account id or the member name. The check's positive control states the other half of the contract: a well-formed custom-action ARN that was never associated to the configuration disassociates as a harmless no-op (204, no error) and ListAssociations(ChatConfiguration=...) then lists no association whose `Resource` equals it.

Emulator: the same call is refused with HTTP 400 and Error.Code `InvalidRequestException` — the emulator applies the ResourceIdentifier @pattern constraint and returns the generic input-constraint refusal, never reaching the authorization decision. Replay output: `AssertionError: unexpected code='InvalidRequestException'` where `UnauthorizedException` was required. Because the code assertion fails first, the message assertion (`message == "Unauthorized"`) is unreachable, so the emulator's message for this case diverges too: it must be the fixed string `Unauthorized`, not a constraint-violation description.

Why: a client that distinguishes "denied" from "malformed input" takes the wrong branch. Against real AWS a caller passing an unverifiable or foreign `Resource` sees a 403 denial and treats it as a credentials/cross-account problem (re-assume a role, escalate, stop retrying); against the emulator it sees a 400 input error and rewrites the request instead. The HTTP status class also differs (403 vs 400), so retry policies, alarms keyed on 4xx subclass, and SDK error-shape handlers that catch `UnauthorizedException` specifically all misbehave. Any negative test asserting that Chatbot fails closed on an unverifiable resource passes on AWS and fails on the emulator.

Fix: the DisassociateFromConfiguration handler's request-validation ordering — the resource-scoped authorization check on `Resource` must be evaluated before the ResourceIdentifier @length/@pattern constraint, and when it denies, the handler's error serializer must emit UnauthorizedException with HTTP 403 and the fixed message `Unauthorized`. The @pattern check stays in place for inputs that pass authorization. The idempotent no-op path (204, no error, absent from ListAssociations) for a well-formed, never-associated custom-action ARN must be preserved.

Note on prior art: the open issue 20261119T212914Z-chatbot-disassociatefromconfiguration-rejects-a-malformed-resource-with-400.md describes this same divergence but was NOT reported by the replay as owning this check's replay key, so this check is still failing and unowned; it is filed here with its replay key so the fix is gated. If a fixer closes the two together, re-replay the check id below. The other open issue 20261030T111746Z-chatbot-untagresource-on-a-missing-resource-returns-403.md is the opposite direction on a different operation (UntagResource answering 403 where 404 is required) and does not cover this one.

Traces: chatbot.disassociate-from-configuration-malformed-resource-arn-rejected.

## Evidence
Expected (real AWS, per the recorded check): chatbot.disassociate_from_configuration(Resource="not-a-valid-arn-at-all", ChatConfiguration=<a Chime webhook configuration's ChatConfigurationArn>) -> HTTP 403, Error.Code = "UnauthorizedException", Error.Message = "Unauthorized". Message anatomy: "Unauthorized" is fixed text in full; there are no request- or state-derived substitutions in it.

Actual (emulator): HTTP 400, Error.Code = "InvalidRequestException" (the ResourceIdentifier @pattern constraint refusal), hence a constraint-violation message rather than "Unauthorized".

Replay excerpt:
  File "check_chatbot_disassociate_from_configuration_malformed_resource_arn_rejected.py", line 30, in run
    assert code == "UnauthorizedException", f"unexpected code={code!r}"
  AssertionError: unexpected code='InvalidRequestException'

Smithy (services/service-com-amazonaws-chatbot-wheatleyorchestration-20171011/smithy-model/com-amazonaws-chatbot-wheatleyorchestration-20171011.json): DisassociateFromConfiguration input Resource -> ResourceIdentifier (required, @length 1..1011, @pattern ^arn:aws:chatbot:[A-Za-z0-9_/.-]{0,63}:[A-Za-z0-9_/.-]{0,63}:custom-action/[a-zA-Z0-9_-]{1,64}$), ChatConfiguration -> ChatConfigurationArn (required); errors InternalServiceError(500) / InvalidRequestException(400) / UnauthorizedException(403); @idempotent; http {POST /disassociate-from-configuration, code 204}.

Unexercised control requirement (blocked by the failure above): a well-formed custom-action ARN never associated to the configuration must disassociate without error (204) and must not appear in ListAssociations(ChatConfiguration=...).

## Steps to Reproduce
1. Create an SNS topic and an IAM role whose trust policy allows chatbot.amazonaws.com to sts:AssumeRole.
2. chatbot.create_chime_webhook_configuration(WebhookDescription=..., WebhookUrl="https://hooks.chime.aws/incomingwebhooks/<token>?token=<token>x6", SnsTopicArns=[<topic arn>], IamRoleArn=<role arn>, ConfigurationName=<unique name>); keep WebhookConfiguration.ChatConfigurationArn.
3. Call chatbot.disassociate_from_configuration(Resource="not-a-valid-arn-at-all", ChatConfiguration=<that ChatConfigurationArn>).
4. Observe: the emulator answers HTTP 400 with Error.Code "InvalidRequestException"; real AWS answers HTTP 403 with Error.Code "UnauthorizedException" and Error.Message exactly "Unauthorized".
5. Control that must keep working: chatbot.create_custom_action(ActionName=<matching [a-zA-Z0-9_-]{1,64}>, Definition={"CommandText": "echo ctl"}), then disassociate_from_configuration(Resource=<that CustomActionArn>, ChatConfiguration=<ChatConfigurationArn>) must succeed as a no-op (204), and list_associations(ChatConfiguration=...) must not list that Resource.
6. Replay: run the behavioral suite check chatbot.disassociate-from-configuration-malformed-resource-arn-rejected.
