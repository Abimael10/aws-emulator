Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: greengrassv2.get-component-version-artifact-malformed-arn-rejected
Issue-Shape-Id: com.amazonaws.greengrassv2#GreengrassV2
Issue-Key: greengrassv2.get-component-version-artifact-malformed-arn-rejected|run#15|AssertionError|assert control_code == "ValidationException", (
Issue-Commit: 7d1a8774b1003db2f9d4d4b18e934eaa760d5e3d

# greengrassv2 GetComponentVersionArtifact answers 501 NotImplemented for a well-formed component-version ARN that names no existing component, instead of 400 ValidationException "Component (<name>:<version>) or Artifact (<artifactName>) for account <accountId> does not exist"

> _Note: this issue was identified against the code base at commit `7d1a8774b1003db2f9d4d4b18e934eaa760d5e3d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetComponentVersionArtifact

## Description
1 cause to close.

Cause 1 of 1: GetComponentVersionArtifact refuses a syntactically VALID but nonexistent component-version ARN with 501 NotImplemented instead of AWS's 400 ValidationException.

AWS: Smithy (com.amazonaws.greengrassv2#GreengrassV2): GetComponentVersionArtifact, GET /greengrass/v2/components/{arn}/artifacts/{artifactName+}, success 200 with required output member `preSignedUrl`; input `arn` (ComponentVersionARN, @httpLabel, required, pattern ^arn:[^:]*:greengrass:[^:]*:(aws|[0-9]+):components:[^:]+:versions:[^:]+$) and `artifactName` (NonEmptyString, @httpLabel, required, @length min 1); modeled errors AccessDeniedException(403), InternalServerException(500), ResourceNotFoundException(404), ThrottlingException(429), ValidationException(400). Recorded behavior: this operation does NOT use ResourceNotFoundException to separate "malformed ARN" from "no such component" — both collapse into 400 ValidationException, with two distinct messages. An ARN that fails the pattern is refused 400 ValidationException with message `Invalid Input: Component version ARN <arn> is invalid` (fixed text `Invalid Input: Component version ARN ` and ` is invalid`; the variable part is the `arn` value copied from the request). An ARN that MATCHES the pattern but names a component version the account never created is refused 400 ValidationException with message `Component (<componentName>:<componentVersion>) or Artifact (<artifactName>) for account <accountId> does not exist` — fixed text is `Component (`, `) or Artifact (`, `) for account `, ` does not exist`; componentName and componentVersion are parsed out of the request's ARN, artifactName comes from the request path, and accountId is the calling account. No recording value (the generated component name, the account id, the region) is part of the requirement — only that template filled from the request and from state.

Emulator: the malformed-ARN branch is already correct (the emulator answers 400 ValidationException with the exact `Invalid Input: Component version ARN not-an-arn is invalid` wording, so that assertion passes). The well-formed-but-nonexistent branch never reaches any such check: the request is answered HTTP 501 with error code `NotImplemented` (message `operation GetComponentVersionArtifact is scaffolded but not implemented in this emulator`). Replay: `AssertionError: expected ValidationException for a well-formed but nonexistent arn, got 'NotImplemented'`. The implication is that the ARN-pattern rejection happens before dispatch, while the handler behind it is still a scaffold stub, so every request whose ARN is well formed — existing component or not — gets 501 and the success path (`preSignedUrl`) and the not-exist refusal class are both unreachable.

Why: a client cannot obtain an artifact pre-signed URL at all, and cannot distinguish "you gave me a bad ARN" from "that component version or artifact does not exist": it sees a 501 server-side NotImplemented, which botocore classifies as a non-retryable server fault rather than a 400 client validation error. Code that probes for artifact existence via this operation's ValidationException (installers, deployment tooling, SDK conformance tests) treats the 501 as an emulator/service outage instead of the modeled refusal.

Fix: the operation's handler, services/service-com-amazonaws-greengrassv2-greengrassv2/src/ops/get_component_version_artifact.rs (`handle`) — it must stop returning the scaffold `not_implemented` and instead look the ARN up in the component-version/recipe state: when the component version (or the named artifact in its recipe) is absent, emit 400 ValidationException with the `Component (...) or Artifact (...) for account ... does not exist` message assembled from the request's ARN, the requested artifactName and the caller's account; otherwise serialize the 200 response with `preSignedUrl`. The existing pre-dispatch ARN-pattern validation that produces `Invalid Input: Component version ARN <arn> is invalid` is correct and must be preserved.

Traces: greengrassv2.get-component-version-artifact-malformed-arn-rejected.

Relation to already-open work: 20261103T065241Z-greengrassv2-7-net-new-unimplemented-operations-answer-501.md (Cause 5 of 7) names the same scaffolded handler but is keyed only on greengrassv2.get-component-version-artifact-undeclared-artifact-rejected (an artifact name the recipe of an EXISTING component version does not declare). This check exercises a different refusal class on the same operation — a component version that does not exist at all — and the replay report did not mark it as owned by that issue, so it is filed here; whoever fixes either should satisfy both messages.

## Evidence
Check: greengrassv2.get-component-version-artifact-malformed-arn-rejected [mismatch]

Step 1 (passes, shown for contrast): get_component_version_artifact(arn="not-an-arn", artifactName="thing.zip")
  Expected: HTTP 400, Code=ValidationException, Message="Invalid Input: Component version ARN not-an-arn is invalid"
  Actual:   same — emulator matches.

Step 2 (diverges): get_component_version_artifact(arn="arn:aws:greengrass:<region>:<accountId>:components:<never-created-name>:versions:1.0.0", artifactName="thing.zip")
  Expected: HTTP 400, Code=ValidationException, Message="Component (<never-created-name>:1.0.0) or Artifact (thing.zip) for account <accountId> does not exist"
            (fixed text: "Component (", ") or Artifact (", ") for account ", " does not exist"; variable parts: component name and version from the request ARN, artifact name from the request, account id from the caller)
  Actual:   HTTP 501, Code=NotImplemented, Message="operation GetComponentVersionArtifact is scaffolded but not implemented in this emulator"

Replay assertion:
  File "check_greengrassv2_get_component_version_artifact_malformed_arn_rejected.py", line 50, in run
    assert control_code == "ValidationException", (
  AssertionError: expected ValidationException for a well-formed but nonexistent arn, got 'NotImplemented'

Smithy evidence: GetComponentVersionArtifact declares ValidationException(400) and ResourceNotFoundException(404) among its errors and a 200 success with required preSignedUrl; NotImplemented/501 is not a modeled outcome for this operation.

## Steps to Reproduce
1. Boot the emulator and point a greengrassv2 client at it (plus an sts client for the account id).
2. create_component_version(inlineRecipe=<a minimal recipe declaring no artifacts>) — succeeds; keep the returned `arn`.
3. get_component_version_artifact(arn="not-an-arn", artifactName="thing.zip") — observe the correct 400 ValidationException "Invalid Input: Component version ARN not-an-arn is invalid".
4. Build a pattern-valid ARN for a component that was never created: arn:aws:greengrass:<region>:<accountId>:components:<random-unused-name>:versions:1.0.0
5. get_component_version_artifact(arn=<that ARN>, artifactName="thing.zip").
   Expected: 400 ValidationException with message "Component (<random-unused-name>:1.0.0) or Artifact (thing.zip) for account <accountId> does not exist".
   Observed: 501 with error code NotImplemented.
6. describe_component(arn=<the arn from step 2>) still returns the created component, confirming the service is otherwise serving and only this operation's handler is a stub.
