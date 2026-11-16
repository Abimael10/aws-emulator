Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appconfig.in-flight-deployment-does-not-block-leaf-deletion
Issue-Shape-Id: com.amazonaws.appconfig#AmazonAppConfig
Issue-Key: appconfig.in-flight-deployment-does-not-block-leaf-deletion|run#20|AssertionError|assert start_code == "ConflictException", (
Issue-Commit: af00353512b8f96b4224cdf0a6e66e42785b9365

# appconfig: StartDeployment checks the hosted configuration version before the in-flight-deployment gate, so a second deployment on a Deploying environment answers 404 ResourceNotFoundException instead of 409 ConflictException

> _Note: this issue was identified against the code base at commit `af00353512b8f96b4224cdf0a6e66e42785b9365`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartDeployment

## Description
Cause 1 of 1: StartDeployment evaluates its preconditions in the wrong order — the "is this configuration version held in the hosted store" lookup is decided before the environment-state (in-flight deployment) conflict gate — so a second StartDeployment issued against an environment that is already Deploying is refused as a missing resource instead of as a state conflict.

AWS: Smithy (services/service-com-amazonaws-appconfig-amazonappconfig/smithy-model/com-amazonaws-appconfig-amazonappconfig.json) models StartDeployment with errors ConflictException (HTTP 409, "The request could not be processed because of conflict in the current state of the resource.") and ResourceNotFoundException (HTTP 404, "The requested resource could not be found."). Recorded behavior: while an environment's deployment is in flight (GetEnvironment State == "Deploying"), AWS refuses any further StartDeployment on that environment with 409 ConflictException and the message "Environment <EnvironmentId> currently has a state of DEPLOYING. The current deployment must finish before a new deployment can be started." The fixed text of that message is "Environment " / " currently has a state of " / ". The current deployment must finish before a new deployment can be started."; the variable parts are the environment id taken from the request and the environment's own state rendered SCREAMING_SNAKE (DEPLOYING) from service state. The ordering is part of the contract: the conflict is decided on the ENVIRONMENT before the configuration version named in the request is resolved inside the named profile, so a second start that happens to name a profile holding no such hosted version still gets the 409 conflict, not a 404.

Emulator: the second StartDeployment (same application/environment, a different hosted profile that holds no version yet, ConfigurationVersion from the first profile) is refused with Error.Code "ResourceNotFoundException" (HTTP 404) carrying the hosted-store message "Configuration version '<ConfigurationVersion>' of configuration profile '<profile>' is not held in the AppConfig hosted configuration store." — the state conflict is never reported. The ConflictException gate exists but is unreachable in this request class because it runs after the version lookup.

Why: a client cannot tell a transient live-state conflict from a permanent addressing mistake. On 409 the correct recovery is to wait for the running deployment (or StopDeployment) and retry; on 404 the correct recovery is to fix identifiers. Deployment orchestrators, CDK/CloudFormation-style retry loops and any waiter keyed on ConflictException give up or fail the rollout instead of retrying, and the refusal also hides that the environment is busy.

Fix: precondition ordering in the StartDeployment handler — the environment-state / active-deployment conflict gate must be evaluated before the hosted-configuration-version existence check (and before any other per-request resource-content lookup), and must produce the 409 ConflictException message above naming the environment id and its SCREAMING_SNAKE state.

Traces: failing check appconfig.in-flight-deployment-does-not-block-leaf-deletion, which asserts start_code == "ConflictException" and got 'ResourceNotFoundException'. Code citations: services/service-com-amazonaws-appconfig-amazonappconfig/src/ops/start_deployment.rs lines 417-425 (the hosted-version ResourceNotFoundException) run before lines 452-464 (the active-deployment ConflictException gate).

## Evidence
Check appconfig.in-flight-deployment-does-not-block-leaf-deletion:
- Setup: application + environment + two hosted profiles; one hosted configuration version created under profile A; a 15-minute/10-minute-bake deployment strategy; StartDeployment of profile A's version; poll until GetEnvironment State == "Deploying" (observed, so the environment really is in flight).
- Call under test: StartDeployment(ApplicationId=app, EnvironmentId=env, DeploymentStrategyId=ds, ConfigurationProfileId=<spare profile>, ConfigurationVersion="<version of profile A>").
- Expected (AWS): HTTP 409, Error.Code "ConflictException", Error.Message "Environment <env_id> currently has a state of DEPLOYING. The current deployment must finish before a new deployment can be started."
- Actual (emulator): HTTP 404, Error.Code "ResourceNotFoundException", Error.Message "Configuration version '<v>' of configuration profile '<spare profile>' is not held in the AppConfig hosted configuration store."
- Harness assertion: "AssertionError: a second StartDeployment on an environment that is Deploying must be refused with ConflictException (409), got 'ResourceNotFoundException'; BadRequestException would conflate a live-state conflict with AppConfig's structural 400s"

## Steps to Reproduce
1. CreateApplication; CreateEnvironment; CreateConfigurationProfile A (LocationUri="hosted"); CreateConfigurationProfile B (LocationUri="hosted", no hosted versions).
2. CreateHostedConfigurationVersion under A (note VersionNumber).
3. CreateDeploymentStrategy with DeploymentDurationInMinutes=15, GrowthFactor=10, GrowthType=LINEAR, FinalBakeTimeInMinutes=10, ReplicateTo=NONE.
4. StartDeployment(app, env, strategy, profile A, ConfigurationVersion=str(VersionNumber)); poll GetEnvironment until State == "Deploying".
5. StartDeployment(app, env, strategy, profile B, ConfigurationVersion=str(VersionNumber)).
6. Observe 404 ResourceNotFoundException (hosted-store message) where AWS answers 409 ConflictException "Environment <env_id> currently has a state of DEPLOYING. The current deployment must finish before a new deployment can be started."
