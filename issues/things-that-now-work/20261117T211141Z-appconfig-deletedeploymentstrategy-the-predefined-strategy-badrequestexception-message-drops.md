Issue-Severity: LOW
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appconfig.delete-predefined-deployment-strategies-rejected-each-fields-unchanged
Issue-Shape-Id: com.amazonaws.appconfig#AmazonAppConfig
Issue-Key: appconfig.delete-predefined-deployment-strategies-rejected-each-fields-unchanged|run#2|AssertionError|assert message == expected_message, (
Issue-Commit: a39251e5c9db86afe29d1902591682be414d06f8

# appconfig DeleteDeploymentStrategy: the predefined-strategy BadRequestException message drops AWS's terminating period

> _Note: this issue was identified against the code base at commit `a39251e5c9db86afe29d1902591682be414d06f8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteDeploymentStrategy

## Description
Cause 1 of 1: DeleteDeploymentStrategy on an AWS-predefined deployment strategy is correctly refused with BadRequestException, but the error message is missing the sentence-terminating period AWS emits, so the message text does not match AWS byte-for-byte.

AWS: Smithy (services/service-com-amazonaws-appconfig-amazonappconfig): DeleteDeploymentStrategy input DeploymentStrategyId @required @httpLabel with pattern ^(^[a-z0-9]{4,7}$|^AppConfig\.[A-Za-z0-9]{9,40}$)$; modeled errors BadRequestException (client, HTTP 400), ResourceNotFoundException (404), InternalServerException (500); success is HTTP 204 on DELETE /deployementstrategies/{DeploymentStrategyId}. The model does not state the message text, so for the message this is recorded behavior only: deleting any of the four predefined strategies (AppConfig.AllAtOnce, AppConfig.Linear50PercentEvery30Seconds, AppConfig.Canary10Percent20Minutes, AppConfig.Linear20PercentEvery6Minutes) is refused with HTTP 400, Error.Code 'BadRequestException' and Error.Message of the form 'Cannot delete predefined Deployment Strategy <DeploymentStrategyId>.' — the fixed text is 'Cannot delete predefined Deployment Strategy ' plus a terminating '.'; the only variable part is <DeploymentStrategyId>, taken verbatim from the request path label (the literal 'AppConfig.AllAtOnce' seen in one replay is just that run's value filling the template, not part of it).

Emulator: status and error code are right (400 / BadRequestException) but the message is emitted without the final period: 'Cannot delete predefined Deployment Strategy AppConfig.AllAtOnce' instead of 'Cannot delete predefined Deployment Strategy AppConfig.AllAtOnce.'. The replay fails on the first predefined id it tries, so the same truncation applies to the whole predefined set served by the same refusal path.

Why: a client (or a test harness, or a log/alarm pattern) that matches AWS's exact message for this refusal class does not match the emulator's. Because the error code is generic (BadRequestException covers many AppConfig validation refusals), the message is the only field that distinguishes 'you tried to delete a predefined strategy' from any other bad request, so callers routinely assert or parse it; against the emulator that discrimination silently fails and the refusal is misclassified.

Fix: the DeleteDeploymentStrategy handler's predefined-strategy guard in the appconfig service crate — the place that constructs the BadRequestException message for a predefined DeploymentStrategyId. Append the terminating period so the message is 'Cannot delete predefined Deployment Strategy {id}.' for every predefined id, with {id} echoed from the request.

Traces: appconfig.delete-predefined-deployment-strategies-rejected-each-fields-unchanged.

## Evidence
Check appconfig.delete-predefined-deployment-strategies-rejected-each-fields-unchanged, [mismatch]:

AssertionError: AppConfig.AllAtOnce: expected 'Cannot delete predefined Deployment Strategy AppConfig.AllAtOnce.', got 'Cannot delete predefined Deployment Strategy AppConfig.AllAtOnce'

Expected (AWS): HTTP 400, Error.Code = 'BadRequestException', Error.Message = 'Cannot delete predefined Deployment Strategy <id>.' (fixed text + terminating period; <id> from the request).
Actual (emulator): HTTP 400, Error.Code = 'BadRequestException', Error.Message = 'Cannot delete predefined Deployment Strategy <id>' (no terminating period).

The status code and error code match; only the message text diverges.

## Steps to Reproduce
1. Point an AppConfig client at the emulator.
2. Call GetDeploymentStrategy(DeploymentStrategyId='AppConfig.AllAtOnce') to confirm the predefined strategy exists.
3. Call DeleteDeploymentStrategy(DeploymentStrategyId='AppConfig.AllAtOnce') and catch the ClientError.
4. Read response['Error']['Code'] -> 'BadRequestException' (correct) and response['Error']['Message'].
5. Observe the message is 'Cannot delete predefined Deployment Strategy AppConfig.AllAtOnce' where AWS returns 'Cannot delete predefined Deployment Strategy AppConfig.AllAtOnce.' (trailing period).
6. Repeat for AppConfig.Linear50PercentEvery30Seconds, AppConfig.Canary10Percent20Minutes and AppConfig.Linear20PercentEvery6Minutes — same refusal path, same missing period.
