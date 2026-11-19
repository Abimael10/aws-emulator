Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appconfig__ssm.ssm-parameter-backed-profile-serves-the-deployed-parameter-version
Issue-Key: appconfigdata|GetLatestConfiguration|ResourceNotFoundException
Issue-Commit: af00353512b8f96b4224cdf0a6e66e42785b9365

# appconfig__ssm: an SSM Parameter Store-backed configuration profile is never served by the data plane — GetLatestConfiguration answers 404 ResourceNotFoundException "not held in the AppConfig hosted configuration store"

> _Note: this issue was identified against the code base at commit `af00353512b8f96b4224cdf0a6e66e42785b9365`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetLatestConfiguration

## Description
Cause 1 of 1: the AppConfig data plane resolves a deployed configuration ONLY against the AppConfig hosted configuration store. A configuration profile whose LocationUri points at an external source (here ssm-parameter://<parameter name>) is accepted by the control plane and its deployment reaches COMPLETE, but every appconfigdata poll for it is refused as not found instead of being served the deployed source version's bytes.

AWS: Smithy (services/service-com-amazonaws-appconfig-amazonappconfig/smithy-model/com-amazonaws-appconfig-amazonappconfig.json) documents CreateConfigurationProfile.LocationUri as "A URI to locate the configuration... For the AppConfig hosted configuration store and for feature flags, specify hosted... For an Amazon Web Services Systems Manager Parameter Store parameter, specify either the parameter name in the format ssm-parameter://<parameter name> or the ARN" (also s3://, secretsmanager://, ssm-document://, codepipeline://). For such a profile StartDeployment's ConfigurationVersion names the SOURCE version (the SSM parameter version number), AppConfig reads the source through the profile's RetrievalRoleArn, and appconfigdata GetLatestConfiguration then answers HTTP 200 with Configuration = the bytes of the version the deployment named and ContentType describing them. Recorded behavior: after a COMPLETE deployment of parameter version 1, fresh sessions are served that parameter version's value; overwriting the parameter (version 2) does NOT change what sessions are served until a new deployment names version 2, at which point every fresh session is served the new value.

Emulator: CreateConfigurationProfile(LocationUri="ssm-parameter://<name>", RetrievalRoleArn=<role>) succeeds and GetConfigurationProfile echoes both members; StartDeployment of ConfigurationVersion="1" reaches State COMPLETE; but appconfigdata GetLatestConfiguration on a session over that triple fails with HTTP 404, Error.Code "ResourceNotFoundException", Error.Message "Configuration version '<ConfigurationVersion>' of configuration profile '<profile>' is not held in the AppConfig hosted configuration store." The fixed text there is the emulator's own hosted-store sentence; its variable parts are the deployed ConfigurationVersion from state and the profile identifier — AWS sends no such refusal on this path at all, it serves 200 with the parameter's value. StartDeployment deliberately skips its hosted-version check for a non-hosted LocationUri, so the gap only surfaces at serve time.

Why: every external-source configuration profile is unusable end to end. A client that deploys configuration from Parameter Store (the documented non-hosted path, and by the same resolver S3, Secrets Manager, SSM documents and CodePipeline) gets a 404 ResourceNotFoundException from the data plane where AWS returns its configuration bytes, so the application boots with no configuration; and because nothing is ever served, AWS's source-version pinning (an out-of-band parameter overwrite must not reach running sessions) cannot be observed either.

Fix: the served-configuration resolver in the AppConfig state that the appconfigdata GetLatestConfiguration handler calls (crate::state::served_configuration, consumed at services/service-com-amazonaws-appconfigdata-appconfigdata/src/ops/get_latest_configuration.rs STEP 2) must, for a profile whose LocationUri is not the hosted store, resolve the content of the SOURCE version the deployment recorded — for ssm-parameter:// the named SSM parameter at that version — and serve it with its content type, keeping the deployed version pinned against later source mutations; the hosted-store "not held" refusal must be confined to hosted profiles.

Traces: failing check appconfig__ssm.ssm-parameter-backed-profile-serves-the-deployed-parameter-version.

## Evidence
Check appconfig__ssm.ssm-parameter-backed-profile-serves-the-deployed-parameter-version:
- PutParameter(Name=<param>, Value="tier-one", Type=String) -> Version 1; GetParameter confirms the value at the source.
- CreateConfigurationProfile(LocationUri="ssm-parameter://<param>", RetrievalRoleArn=<appconfig-trusted role with ssm:GetParameter>) succeeds; GetConfigurationProfile echoes LocationUri and RetrievalRoleArn; ListHostedConfigurationVersions is empty (so anything served can only come from Parameter Store).
- StartDeployment(ConfigurationVersion="1") reaches State COMPLETE.
- StartConfigurationSession + GetLatestConfiguration:
  Expected (AWS): HTTP 200, Configuration == b"tier-one" (the deployed parameter version's value).
  Actual (emulator): botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the GetLatestConfiguration operation: Configuration version '1' of configuration profile 'tn20ygr' is not held in the AppConfig hosted configuration store.
- The downstream assertions the refusal prevents from ever running: after PutParameter(Overwrite=True) -> version 2, three fresh sessions must still be served the version-1 bytes, and only after deploying ConfigurationVersion="2" must every fresh session be served the version-2 bytes.

## Steps to Reproduce
1. ssm.PutParameter(Name=<param>, Value="tier-one", Type="String") -> Version 1.
2. iam.CreateRole trusted by appconfig.amazonaws.com with inline ssm:GetParameter on the parameter ARN.
3. appconfig.CreateApplication; CreateEnvironment.
4. appconfig.CreateConfigurationProfile(LocationUri="ssm-parameter://<param>", RetrievalRoleArn=<role arn>).
5. appconfig.CreateDeploymentStrategy(DeploymentDurationInMinutes=0, GrowthFactor=100, FinalBakeTimeInMinutes=0, ReplicateTo="NONE").
6. appconfig.StartDeployment(..., ConfigurationProfileId=<profile>, ConfigurationVersion="1"); poll GetDeployment until State == "COMPLETE".
7. appconfigdata.StartConfigurationSession(app, env, profile) then GetLatestConfiguration(InitialConfigurationToken).
8. Observe 404 ResourceNotFoundException "Configuration version '1' of configuration profile '<profile>' is not held in the AppConfig hosted configuration store." where AWS returns 200 with Configuration == b"tier-one".
9. (Blocked by the above) PutParameter(Overwrite=True, Value="tier-two") -> version 2; fresh sessions must still be served "tier-one"; after StartDeployment with ConfigurationVersion="2" they must be served "tier-two".
