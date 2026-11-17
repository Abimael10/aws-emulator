Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codedeploy.create-deployment-config-predefined-name-prefix-rejected codedeploy.tag-resource-duplicate-key-in-request-rejected
Issue-Shape-Id: com.amazonaws.codedeploy#CodeDeploy_20141006
Issue-Key: codedeploy.create-deployment-config-predefined-name-prefix-rejected|run#5|AssertionError|assert code == "InvalidDeploymentConfigNameException", code
Issue-Key: codedeploy.tag-resource-duplicate-key-in-request-rejected|expect_error#7|AssertionError|raise AssertionError(f"expected the call to be rejected with {code}, but it succeeded: {result!r}")
Issue-Commit: dce52ec55fc752bbcad968bb3711eedbc16de828

# CodeDeploy: CreateDeploymentConfig does not reserve the CodeDeployDefault. prefix, and TagResource accepts a duplicate tag key (2 causes)

> _Note: this issue was identified against the code base at commit `dce52ec55fc752bbcad968bb3711eedbc16de828`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDeploymentConfig

## Description
Two independent input-validation preconditions of the CodeDeploy control plane are missing. Both calls are refused by real AWS with a modeled 400, and both are instead handled as ordinary requests by the emulator. Two distinct validation rules, two root causes.

Cause 1 of 2: CreateDeploymentConfig does not reserve the `CodeDeployDefault.` name prefix, so a request naming a predefined configuration is classified as a name collision instead of a reserved-prefix rejection.
AWS: Smithy CreateDeploymentConfig declares `deploymentConfigName` required (DeploymentConfigName, @length 1-100, @pattern ^[A-Za-z0-9+=,.@_-]*$) and models both InvalidDeploymentConfigNameException ("The deployment configuration name was specified in an invalid format.", HTTP 400) and DeploymentConfigAlreadyExistsException (HTTP 400). Any name beginning with the reserved prefix `CodeDeployDefault.` can never be created by a caller at all, so the prefix rule is evaluated BEFORE existence: the refusal is InvalidDeploymentConfigNameException with the message `The prefix CodeDeployDefault. is reserved for predefined configuration names`. That message is entirely fixed text — the only variable part in the class is the reserved prefix itself (`CodeDeployDefault.`), not the requested name. A name outside the reserved prefix with the identical body is accepted.
Emulator: the same call is refused with code `DeploymentConfigAlreadyExistsException` instead of `InvalidDeploymentConfigNameException` (replay: `AssertionError: DeploymentConfigAlreadyExistsException`). The emulator is only matching the requested name against its seeded predefined-config records, so it reports a collision; a reserved-prefix name that is NOT one of the seeded predefined configs (e.g. `CodeDeployDefault.NotARealConfig`) would therefore not be refused at all.
Why: a client distinguishing "this name is mine but taken" from "this name is in AWS's reserved namespace" gets the wrong answer. Collision handling normally retries with a suffixed name, which can never succeed under the reserved prefix, so provisioning code loops or aborts instead of surfacing the naming error; conversely a reserved-prefix name that collides with nothing is silently created in a namespace AWS owns.
Fix: CreateDeploymentConfig's input validation in the operation's handler — check the reserved `CodeDeployDefault.` prefix on `deploymentConfigName` before the existence check, and emit InvalidDeploymentConfigNameException.
Traces: codedeploy.create-deployment-config-predefined-name-prefix-rejected.

Cause 2 of 2: TagResource accepts a Tags list containing the same Key twice.
AWS: Smithy TagResource declares `ResourceArn` and `Tags` required and models InvalidTagsToAddException ("The specified tags are not valid.", HTTP 400). A request whose Tags list repeats a key is refused with InvalidTagsToAddException and the message `Duplicate Tag key: <key>` — fixed text `Duplicate Tag key: ` followed by the repeated key taken from the request. Nothing is tagged: ListTagsForResource still answers an empty Tags list. Sending that key once succeeds.
Emulator: returns HTTP 200 with an empty JSON body (`content-length: 2`) and applies the tags (last write wins), so the duplicate-key request is treated as valid (replay: "expected the call to be rejected with InvalidTagsToAddException, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200 ...}}").
Why: a client that relies on AWS rejecting a malformed tag set persists a non-deterministic tag state — which of the two duplicate values survives is an emulator implementation detail — and never sees the error it handles in real AWS, so tag-building bugs that AWS catches at the API boundary go undetected.
Fix: TagResource's input validation in the operation's handler — reject a Tags list with repeated keys before any state mutation, and leave the resource's tag set untouched.
Traces: codedeploy.tag-resource-duplicate-key-in-request-rejected.

## Evidence
Replayed behavioral suite, service heading `### codedeploy` (120/144 passed).

Cause 1 — codedeploy.create-deployment-config-predefined-name-prefix-rejected [mismatch]:
  Request: CreateDeploymentConfig(deploymentConfigName="CodeDeployDefault.LambdaLinear10PercentEvery1Minute", minimumHealthyHosts={type: HOST_COUNT, value: 1}).
  Expected (AWS): HTTP 400, Error.Code = "InvalidDeploymentConfigNameException", Error.Message = "The prefix CodeDeployDefault. is reserved for predefined configuration names" (all fixed text); the predefined config's own record is unchanged (same computePlatform and trafficRoutingConfig, still no minimumHealthyHosts member).
  Actual (emulator): HTTP 400 with Error.Code = "DeploymentConfigAlreadyExistsException" — `assert code == "InvalidDeploymentConfigNameException", code` -> `AssertionError: DeploymentConfigAlreadyExistsException`.
  Positive control in the same check (an otherwise-identical create under a non-reserved name) passes.

Cause 2 — codedeploy.tag-resource-duplicate-key-in-request-rejected [mismatch]:
  Request: TagResource(ResourceArn=arn:aws:codedeploy:<region>:<account>:application:<app>, Tags=[{Key: owner, Value: platform}, {Key: owner, Value: infra}]).
  Expected (AWS): HTTP 400, Error.Code = "InvalidTagsToAddException", Error.Message = "Duplicate Tag key: owner" (fixed text "Duplicate Tag key: " + the repeated key from the request, here `owner`); ListTagsForResource afterwards returns [].
  Actual (emulator): HTTP 200, headers content-type application/x-amz-json-1.1, content-length 2 — `AssertionError: expected the call to be rejected with InvalidTagsToAddException, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, ...}}`.
  Positive control in the same check (the same key sent once) passes.

Smithy: CreateDeploymentConfig errors include InvalidDeploymentConfigNameException and DeploymentConfigAlreadyExistsException (both client, HTTP 400); TagResource errors include InvalidTagsToAddException (client, HTTP 400). Account id, region, generated application name and the response date above are environment-specific; the requirement is the status, the error code and the message template.

## Steps to Reproduce
Cause 1:
1. Boot the emulator.
2. GetDeploymentConfig(deploymentConfigName="CodeDeployDefault.LambdaLinear10PercentEvery1Minute") and keep the record.
3. CreateDeploymentConfig(deploymentConfigName="CodeDeployDefault.LambdaLinear10PercentEvery1Minute", minimumHealthyHosts={"type":"HOST_COUNT","value":1}) -> expect HTTP 400 InvalidDeploymentConfigNameException with message "The prefix CodeDeployDefault. is reserved for predefined configuration names"; observe DeploymentConfigAlreadyExistsException.
4. Also try a reserved-prefix name that matches no predefined config, e.g. CreateDeploymentConfig(deploymentConfigName="CodeDeployDefault.NotARealConfig", minimumHealthyHosts={"type":"HOST_COUNT","value":1}) -> expect the same InvalidDeploymentConfigNameException.
5. GetDeploymentConfig the predefined name again -> its computePlatform/trafficRoutingConfig must be unchanged and it must carry no minimumHealthyHosts.
6. CreateDeploymentConfig with the identical body under a non-reserved name -> expect success (positive control).

Cause 2:
1. CreateApplication(applicationName=<app>, computePlatform="Lambda").
2. TagResource(ResourceArn="arn:aws:codedeploy:<region>:<account>:application:<app>", Tags=[{"Key":"owner","Value":"platform"},{"Key":"owner","Value":"infra"}]) -> expect HTTP 400 InvalidTagsToAddException with message "Duplicate Tag key: owner"; observe HTTP 200.
3. ListTagsForResource(ResourceArn=<same>) -> expect [] (the refused call must leave no tags).
4. TagResource with only {"Key":"owner","Value":"platform"} -> expect success and ListTagsForResource == [{owner: platform}] (positive control).
