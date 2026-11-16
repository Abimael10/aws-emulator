Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda-microvms.create-shell-auth-token-without-shell-ingress-rejected
Issue-Shape-Id: com.amazonaws.lambdamicrovms#LambdaMicrovms
Issue-Key: lambda-microvms.create-shell-auth-token-without-shell-ingress-rejected|run#8|AssertionError|assert error.get("Message") == (
Issue-Commit: 6f0fa83161e30152cf12b521a6456feb5eaff8e6

# lambda-microvms CreateMicrovmShellAuthToken rejects a MicroVM without SHELL_INGRESS using the model's documentation sentence instead of AWS's ValidationException message

> _Note: this issue was identified against the code base at commit `6f0fa83161e30152cf12b521a6456feb5eaff8e6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMicrovmShellAuthToken

## Description
One net-new root cause.

Cause 1 of 1: the ValidationException message for "this MicroVM was not run with the SHELL_INGRESS network connector" is wrong. Failing check: lambda-microvms.create-shell-auth-token-without-shell-ingress-rejected.

AWS: Smithy (services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/smithy-model/com-amazonaws-lambdamicrovms-lambdamicrovms.json) models CreateMicrovmShellAuthToken as POST /2025-09-09/microvms/{microvmIdentifier}/shell-auth-token, success 200, with ValidationException (HTTP 400, "The input does not satisfy the constraints specified by the service") among its errors; the operation doc states "The MicroVM must have been run with the SHELL_INGRESS network connector attached." The model does not state the wire message for that condition, so this is recorded behavior only: against real AWS the call on a RUNNING MicroVM whose ingressNetworkConnectors do not include SHELL_INGRESS is refused with HTTP 400, Code "ValidationException", Message exactly "Shell access requires SHELL_INGRESS network connector to be configured on the MicroVM." That message is entirely FIXED text — it interpolates no request value, no MicroVM id and no connector ARN. (The recording also shows the adjacent rule that ALL_INGRESS combined with another ingress connector is refused by this operation with "ALL_INGRESS cannot be combined with other ingress network connectors; use HTTP_INGRESS and/or SHELL_INGRESS instead".)

Emulator: the status (400) and the code ("ValidationException") are right, but the message is the model's documentation sentence: {'Message': 'The MicroVM must have been run with the SHELL_INGRESS network connector attached.', 'Code': 'ValidationException'}. The handler builds it from crate::state::no_shell_ingress_message(), i.e. the operation's doc prose was used as the error text rather than the service's own rejection message.

Why: a client that keys off the message — a retry/diagnostic path matching on the AWS wording, a log assertion, or a user-facing surface that forwards the service's sentence — never matches, so the "attach SHELL_INGRESS" remediation is not recognised; the error text a caller sees differs from every AWS document and SDK example for this operation. Nothing in the code or status tells the two apart, so this is only detectable in the message.

Fix: the rejection message for the missing-SHELL_INGRESS guard in CreateMicrovmShellAuthToken's validation — the shared message helper the handler calls, which should emit AWS's fixed sentence rather than the modeled doc sentence.

Traces: lambda-microvms.create-shell-auth-token-without-shell-ingress-rejected (the positive control in the same check — RunMicrovm with SHELL_INGRESS alone, then a token containing the X-aws-proxy-auth key — is not reached because the negative assertion fails first). Message source: services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/src/ops/create_microvm_shell_auth_token.rs:209-218 (crate::state::no_shell_ingress_message()).

## Evidence
Expected (real AWS, recorded): HTTP 400, Error.Code = "ValidationException", Error.Message = "Shell access requires SHELL_INGRESS network connector to be configured on the MicroVM." (fixed text, no variable parts).

Actual (emulator): HTTP 400, {'Code': 'ValidationException', 'Message': 'The MicroVM must have been run with the SHELL_INGRESS network connector attached.'}

Harness failure: AssertionError at check_lambda_microvms_create_shell_auth_token_without_shell_ingress_rejected.py line 37, `assert error.get("Message") == ("Shell access requires SHELL_INGRESS network connector to be configured on the MicroVM.")` — the preceding Code assertion passed.

## Steps to Reproduce
1. Provision a MicroVM image and wait for state CREATED / version SUCCESSFUL (CreateMicrovmImage with an S3 code artifact, a build role, and a Lambda-managed base image from ListManagedMicrovmImages).
2. RunMicrovm with ingressNetworkConnectors=[arn:aws:lambda:<region>:aws:network-connector:aws-network-connector:ALL_INGRESS] and egressNetworkConnectors=[...:INTERNET_EGRESS]; wait for state RUNNING.
3. Call CreateMicrovmShellAuthToken(microvmIdentifier=<that microvmId>, expirationInMinutes=10).
4. Observe HTTP 400 ValidationException with Message "The MicroVM must have been run with the SHELL_INGRESS network connector attached." where AWS answers "Shell access requires SHELL_INGRESS network connector to be configured on the MicroVM."
5. Positive control: RunMicrovm with ingressNetworkConnectors=[...:SHELL_INGRESS] only, wait RUNNING, then the same call must succeed and return authToken containing the key "X-aws-proxy-auth".
