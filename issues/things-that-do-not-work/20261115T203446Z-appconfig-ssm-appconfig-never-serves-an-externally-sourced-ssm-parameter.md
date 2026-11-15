Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appconfig__ssm.ssm-parameter-backed-profile-serves-the-deployed-parameter-version
Issue-Key: appconfig__ssm.ssm-parameter-backed-profile-serves-the-deployed-parameter-version|_wait_terminal#38|Timeout|time.sleep(3.0)
Issue-Commit: 51772985bc72cfea1a888e0d513791715cd162c8

# AppConfig never serves an externally sourced (ssm-parameter://) configuration: the deployed-parameter data-plane scenario hangs past its budget

> _Note: this issue was identified against the code base at commit `51772985bc72cfea1a888e0d513791715cd162c8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetLatestConfiguration

## Description
One net-new root cause.

Cause 1 of 1: a configuration profile whose LocationUri names an external source (here an SSM Parameter Store parameter, `ssm-parameter://<parameter name>`) is never delivered by the AppConfig data plane, and the control-plane/data-plane round trip for it never settles, so the whole scenario runs past the harness budget instead of either serving the parameter's bytes or refusing with a modelled error. Failing check: appconfig__ssm.ssm-parameter-backed-profile-serves-the-deployed-parameter-version.

AWS: CreateConfigurationProfile.LocationUri is modelled to accept an external source — Smithy doc for CreateConfigurationProfileRequest.LocationUri: "For an Amazon Web Services Systems Manager Parameter Store parameter, specify either the parameter name in the format ssm-parameter://<parameter name> or the ARN" (also `hosted`, `s3://…`, `secretsmanager://…`, `codepipeline://…`, `ssm-document://…`). For such a profile AppConfig assumes RetrievalRoleArn, reads the named SOURCE version, and StartDeployment reaches State=COMPLETE (DeploymentState one of BAKING|COMPLETE|DEPLOYING|REVERTED|ROLLED_BACK|ROLLING_BACK|VALIDATING); afterwards appconfigdata StartConfigurationSession + GetLatestConfiguration serve the bytes of the parameter version the deployment named, and keep serving exactly that version even after the parameter is overwritten at the source, until a new deployment names the new version. There are no hosted configuration versions for such a profile at all (ListHostedConfigurationVersions is empty), so everything served must come from the external source.

Emulator: the check never completes — the replay reports `Check timed out after 60.0s`. Every wait in the check is inside the AppConfig control-plane/data-plane loops: the StartDeployment retry + GetDeployment poll for a terminal state, and the StartConfigurationSession/GetLatestConfiguration serve loop. The checkout shows why no poll of either loop can ever succeed for an externally sourced profile: the data plane resolves what to serve ONLY out of the hosted configuration store — `served_in` takes `profile.resolve_hosted_version(&deployment.configuration_version)` and, when it resolves to nothing, answers UnresolvedResource `configuration_version_not_hosted`, i.e. GetLatestConfiguration can only ever return ResourceNotFoundException for an `ssm-parameter://` profile; nothing reads Parameter Store and no retrieval role is ever assumed. StartDeployment likewise skips the version lookup for a non-hosted LocationUri (`if !version_is_held && configuration_location_uri == HOSTED_LOCATION_URI`) and records the location URI without ever materialising the source bytes, so the deployment carries no configuration content for the data plane to serve.

Why: a client that stores configuration in Parameter Store (one of the two most common AppConfig sources, and the whole point of RetrievalRoleArn) gets a profile it can create and a deployment it cannot observe reaching COMPLETE, and sessions that are never served the configuration. Code under test either blocks forever waiting for its configuration or receives ResourceNotFoundException for a profile AWS serves normally; the version-pinning guarantee (overwriting the parameter must not change what sessions are served until a new deployment names the new version) cannot be exercised at all.

Fix: AppConfig's external-source retrieval, in the configuration-profile/deployment state and the data-plane projection — StartDeployment must resolve and snapshot the named SOURCE version for a non-hosted LocationUri (via the profile's RetrievalRoleArn) and drive the deployment to a terminal state, and appconfigdata's served-configuration resolution must serve that snapshot instead of consulting only the hosted store, so a non-hosted profile is not forced down the `configuration_version_not_hosted` refusal path.

Traces: appconfig__ssm.ssm-parameter-backed-profile-serves-the-deployed-parameter-version. Supporting citations from the checkout: services/service-com-amazonaws-appconfigdata-appconfigdata/src/state.rs lines 785-831 (`served_in`, hosted-only resolution); services/service-com-amazonaws-appconfig-amazonappconfig/src/ops/start_deployment.rs lines 441-482 (hosted-only version resolution, no external read).

## Evidence
Failing check: [mismatch] appconfig__ssm.ssm-parameter-backed-profile-serves-the-deployed-parameter-version
Replay detail: `Error: Check timed out after 60.0s`

Expected (AWS): PutParameter creates version 1; CreateConfigurationProfile with LocationUri=ssm-parameter://<name> and RetrievalRoleArn succeeds and echoes both; ListHostedConfigurationVersions is empty; StartDeployment(ConfigurationVersion="1") reaches State=COMPLETE with ConfigurationVersion="1" and ConfigurationLocationUri=ssm-parameter://<name>; StartConfigurationSession + GetLatestConfiguration serve the parameter's bytes; after an Overwrite that creates version 2, every fresh session is still served version 1's bytes and the deployment history is unchanged; a second deployment naming version 2 then makes every session see version 2's bytes.

Actual (emulator): the scenario never reaches a verdict inside the harness budget — no deployment of the externally sourced version is observed terminal and no session is served the parameter value. The code paths in the checkout show the data plane can only resolve HOSTED versions (`profile.resolve_hosted_version(...)` in `served_in`, else the `configuration_version_not_hosted` refusal) and that StartDeployment performs no external-source read for a non-`hosted` LocationUri, so an ssm-parameter:// profile has nothing for GetLatestConfiguration to serve. (Hypothesis: the hang is in the StartDeployment/GetDeployment terminal-state loop or the serve loop for exactly this reason; the harness reports only the timeout.)

## Steps to Reproduce
1. ssm PutParameter Name=<param> Value=tier-one Type=String -> Version 1.
2. iam CreateRole trusted by appconfig.amazonaws.com with inline ssm:GetParameter on the parameter ARN.
3. appconfig CreateApplication; CreateEnvironment; CreateConfigurationProfile LocationUri=ssm-parameter://<param> RetrievalRoleArn=<role arn>.
4. appconfig CreateDeploymentStrategy DeploymentDurationInMinutes=0 GrowthFactor=100 FinalBakeTimeInMinutes=0 ReplicateTo=NONE.
5. appconfig StartDeployment ConfigurationVersion="1"; poll GetDeployment for a terminal State -> expect COMPLETE.
6. appconfigdata StartConfigurationSession on (app, env, profile) then GetLatestConfiguration -> expect the bytes "tier-one".
7. ssm PutParameter Overwrite=True Value=tier-two -> Version 2; fresh sessions must still be served "tier-one".
8. appconfig StartDeployment ConfigurationVersion="2" -> COMPLETE; fresh sessions must then be served "tier-two".
Against the emulator steps 5-6 never settle (replay: check timed out after 60s).
