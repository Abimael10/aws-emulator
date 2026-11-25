Issue-Source: trace-sweep-divergence
Issue-Trace: codedeploy-deployment-group-lookup-and-validation-errors
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: codedeploy-deployment-group-lookup-and-validation-errors
Issue-Shape-Id: com.amazonaws.codedeploy#CodeDeploy_20141006
Issue-Key: codedeploy|BatchGetDeploymentGroups|2xx->2xx|$.errorMessage
Issue-Commit: 919514db1c4457ecce0a02ad6f08fc1eff91bbf9

# CodeDeploy BatchGetDeploymentGroups returns an empty errorMessage for unretrievable deployment groups

> _Note: this issue was identified against the code base at commit `919514db1c4457ecce0a02ad6f08fc1eff91bbf9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchGetDeploymentGroups

## Description
One cause.

Cause 1 of 1: BatchGetDeploymentGroups does not populate the top-level `errorMessage` member when one or more of the requested deployment group names cannot be retrieved for the application.

AWS: Smithy (codedeploy-2014-10-06) models BatchGetDeploymentGroupsOutput with members `deploymentGroupsInfo` (DeploymentGroupInfoList) and `errorMessage` (ErrorMessage), documented as "Information about errors that might have occurred during the API call." BatchGetDeploymentGroups is a batch operation: a deployment group name that does not exist under an existing application is NOT an operation-level error (the modeled errors are only ApplicationDoesNotExistException, ApplicationNameRequiredException, BatchLimitExceededException, DeploymentConfigDoesNotExistException, DeploymentGroupNameRequiredException, InvalidApplicationNameException, InvalidDeploymentGroupNameException). Instead AWS answers HTTP 200 and reports the partial failure in `errorMessage`. Per the recorded response, the message for this error class is the fixed text "The following deployment groups cannot be retrieved for the application: " followed by the application name from the request, then a newline, then the bracketed list of the requested deployment group names that could not be retrieved — i.e. "The following deployment groups cannot be retrieved for the application: <applicationName>\n[<unretrievable deployment group names>]". Fixed text: "The following deployment groups cannot be retrieved for the application: ", the newline, and the surrounding square brackets. Variable parts taken from the request/state: <applicationName> (the request's `applicationName`) and the list of names from the request's `deploymentGroupNames` that have no deployment group in state. The recording's concrete values (an application named sim2real-probe-codedeploy-app-<suffix> and a deployment group named sim2real-probe-codedeploy-dg-missing-<suffix>) are per-run values that merely fill that template and are not themselves requirements.

Emulator: the emulator answers HTTP 200 with the expected status and the expected response shape, but serializes `errorMessage` as the empty string '' — it never composes the partial-failure text at all. Replay reports `$.errorMessage: value_mismatch (expected 'The following deployment groups cannot be retrieved for the application: <applicationName>\n[<missing-dg-name>]', got '')`.

Why: a client that asks for a mix of existing and non-existing deployment groups gets a 200 with a short `deploymentGroupsInfo` list and no indication of which names failed. Tooling and SDK callers that branch on a non-empty `errorMessage` to detect partial batch failure (the only signal AWS gives, since the call does not throw) silently treat a partial result as a complete one, so a typo'd or deleted deployment group looks like success instead of surfacing the "cannot be retrieved" diagnostic.

Fix: BatchGetDeploymentGroups' handler/serializer for the batch result — after resolving each requested name against state, collect the unresolved names and populate the output's `errorMessage` member with the "The following deployment groups cannot be retrieved for the application: <applicationName>\n[<names>]" text, leaving it unset/empty only when every requested name resolved.

Traces: codedeploy-deployment-group-lookup-and-validation-errors#task#4 (expected_status 200, actual_status 200, verdict mismatch on $.errorMessage).

## Evidence
Fixture codedeploy-deployment-group-lookup-and-validation-errors, step task#4, operation BatchGetDeploymentGroups, service codedeploy.

Expected (recorded AWS response): HTTP 200, body carries errorMessage = "The following deployment groups cannot be retrieved for the application: sim2real-probe-codedeploy-app-54d5d65a\n[sim2real-probe-codedeploy-dg-missing-54d5d65a]" (fixed text + applicationName + newline + bracketed list of unretrievable deploymentGroupNames).

Actual (emulator): HTTP 200, errorMessage = "" (empty string).

Replay detail: "$.errorMessage: value_mismatch (expected 'The following deployment groups cannot be retrieved for the application: sim2real-probe-codedeploy-app-54d5d65a\n[sim2real-probe-codedeploy-dg-missing-54d5d65a]', got '')"

Smithy reference: BatchGetDeploymentGroups output = {deploymentGroupsInfo: DeploymentGroupInfoList, errorMessage: ErrorMessage}; errorMessage doc "Information about errors that might have occurred during the API call." No modeled error covers a non-existent deployment group name, confirming the partial failure must be reported in-band at HTTP 200.

## Steps to Reproduce
1. CreateApplication with an application name (e.g. the fixture's sim2real-probe-codedeploy-app-<suffix>) and create at least one deployment group under it.
2. Call BatchGetDeploymentGroups with applicationName = that application and deploymentGroupNames containing at least one name that does NOT exist under the application (the fixture uses a *-dg-missing-* name).
3. Observe the HTTP 200 response: deploymentGroupsInfo omits the missing group, and errorMessage is "" instead of "The following deployment groups cannot be retrieved for the application: <applicationName>\n[<missing deployment group names>]".
4. Equivalent: replay fixture codedeploy-deployment-group-lookup-and-validation-errors, step task#4.
