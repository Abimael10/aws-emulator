Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: amplify.create-backend-environment-lifecycle
Issue-Shape-Id: com.amazonaws.amplify#Amplify
Issue-Key: amplify.create-backend-environment-lifecycle|run#44|AssertionError|assert err_bad_name == (
Issue-Commit: dadb011cf8b38a521b5f566109c5c6b86765cb16

# amplify CreateBackendEnvironment refuses a malformed environmentName with an unmodeled ValidationException instead of AWS's BadRequestException sentence

> _Note: this issue was identified against the code base at commit `dadb011cf8b38a521b5f566109c5c6b86765cb16`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateBackendEnvironment

## Description
Cause 1 of 1: the refusal CreateBackendEnvironment answers for an environmentName outside Amplify's undocumented 2-10-lowercase-letter form carries the wrong error code and an emulator-authored message.

AWS: Smithy (services/service-com-amazonaws-amplify-amplify/smithy-model/com-amazonaws-amplify-amplify.json): CreateBackendEnvironment.environmentName is EnvironmentName, @required, @length 1..255, @pattern ^(?s).+$; the operation models exactly BadRequestException (400), InternalFailureException (500), LimitExceededException (429), NotFoundException (404) and UnauthorizedException (401) — ValidationException is NOT a modeled error of this service's operation. The stricter name rule is recorded behavior only: real Amplify refuses a name that is not 2-10 lowercase alphabetic characters with code BadRequestException and message "Failed to create backend environment.The environment name <environmentName> is invalid. Environment name should be between 2 and 10 characters (only lowercase alphabets)." In that message the fixed text is everything but <environmentName>, which is the environmentName taken verbatim from the request (note AWS's own missing space after the first period, and the parenthetical wording). The same check's positive controls confirm the boundary is the name alone: bare, real-stack and nonexistent-stack environments with 2-10 lowercase names are all accepted, read back, listed and deleted.

Emulator: the call is refused with code ValidationException and message "Environment name must be between 2 and 10 characters, and lowercase only." — a code the model does not declare for this operation, a message AWS never emits, and one that does not echo the offending name at all.

Why: a client that handles Amplify's documented BadRequestException (or that branches on botocore's amplify.exceptions.BadRequestException) does not catch this refusal at all and sees an unknown error class; a client that parses the message to report which environment name was rejected gets no name. Rejection classification and error-driven retry/report paths therefore behave differently against the emulator than against AWS even though the request is refused in both.

Fix: the environmentName validation of the CreateBackendEnvironment handler — services/service-com-amazonaws-amplify-amplify/src/ops/create_backend_environment.rs calls crate::state::require_backend_environment_name (line 162), whose refusal must be raised as the modeled BadRequestException with AWS's sentence templated on the request's environmentName, not as a ValidationException with emulator wording.

Traces: amplify.create-backend-environment-lifecycle (assertion at check_amplify_create_backend_environment_lifecycle.py line 119: got ('ValidationException', 'Environment name must be between 2 and 10 characters, and lowercase only.')).

## Evidence
Check amplify.create-backend-environment-lifecycle, mismatch.

Expected (AWS): (code, message) == ("BadRequestException", "Failed to create backend environment.The environment name <bad_name> is invalid. Environment name should be between 2 and 10 characters (only lowercase alphabets)."), HTTP 400, where <bad_name> is the environmentName the request sent.

Actual (emulator): ("ValidationException", "Environment name must be between 2 and 10 characters, and lowercase only.") — AssertionError: ('ValidationException', 'Environment name must be between 2 and 10 characters, and lowercase only.')

ValidationException is not among the operation's modeled errors (BadRequestException, InternalFailureException, LimitExceededException, NotFoundException, UnauthorizedException).

## Steps to Reproduce
1. amplify CreateApp to obtain an appId.
2. Call CreateBackendEnvironment(appId=<appId>, environmentName=<a name that is not 2-10 lowercase letters, e.g. 'env-7f3Ab12'>).
3. Observe the error code and message: the emulator answers ValidationException "Environment name must be between 2 and 10 characters, and lowercase only."; AWS answers BadRequestException "Failed to create backend environment.The environment name env-7f3Ab12 is invalid. Environment name should be between 2 and 10 characters (only lowercase alphabets)."
4. For contrast, CreateBackendEnvironment with a 2-10 lowercase name succeeds in both.
