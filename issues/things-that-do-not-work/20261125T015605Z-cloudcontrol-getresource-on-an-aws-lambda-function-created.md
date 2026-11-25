Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudcontrol.empty-vs-absent-property-distinguished
Issue-Shape-Id: com.amazonaws.cloudcontrol#CloudApiService
Issue-Key: cloudcontrol.empty-vs-absent-property-distinguished|run#26|AssertionError|assert "Environment" in props_empty, (
Issue-Commit: 6bae9ee14af172aae629fb16389ee320a8de1819

# cloudcontrol: GetResource on an AWS::Lambda::Function created with an explicit empty Environment reports no Environment block, so empty and absent are indistinguishable

> _Note: this issue was identified against the code base at commit `6bae9ee14af172aae629fb16389ee320a8de1819`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResource

## Description
Cause 1 of 1: Cloud Control's resource model for AWS::Lambda::Function drops an explicitly configured but empty Environment, collapsing "configured with no variables" into "never configured". Failing check: cloudcontrol.empty-vs-absent-property-distinguished.

AWS: CreateResource(TypeName=AWS::Lambda::Function) with DesiredState containing Environment={"Variables":{}} configures the function's environment block with an empty variable map; Lambda itself then reports it back (Smithy, lambda: FunctionConfiguration.Environment targets EnvironmentResponse, whose Variables member is a map that may legitimately be empty — EnvironmentResponse documents "if the operation succeeds, the response contains the environment variables"; the input shape Environment has the same Variables map). Cloud Control reads the resource through the type's read handler and returns that model as a JSON document in GetResourceOutput.ResourceDescription.Properties (Smithy, cloudcontrol: GetResource output member ResourceDescription). Because the AWS::Lambda::Function resource schema models Environment as a settable property, a function created with an explicit empty Environment reports an Environment object (with Variables == {}) in Properties, while a function whose DesiredState omitted Environment entirely reports no Environment key at all. The distinction between an explicitly-empty structure and an absent one is observable in the model; recorded behavior confirms the two reads differ.

Emulator: for the function created with Environment={"Variables":{}}, GetResource returns Properties with NO Environment key at all. The whole returned model is: FunctionName, Arn, Role, Handler, Description, Runtime, PackageType, MemorySize, Timeout, Architectures, TracingConfig, EphemeralStorage, LoggingConfig, SnapStartResponse, Tags, Code — Environment is missing. The read of the function that never declared Environment is identical, so the two cases are indistinguishable. The check fails on `assert "Environment" in props_empty` with "expected an explicit empty Environment block to be reported, got {...}" (the listed model is the emulator's response; the function name, role ARN and account in it are per-run values, not part of the requirement).

Why: a client that round-trips a Cloud Control model (drift detection, `GetResource` -> edit -> `UpdateResource`, IaC importers, CDK/Terraform-style refresh) cannot tell that the function has an environment block configured. It therefore computes a spurious diff (re-adding Environment on every refresh, or dropping it) and cannot reproduce the resource's actual configured state from its own read. Any logic that keys on presence of a property rather than its contents is wrong against the emulator.

Fix: the AWS::Lambda::Function read-handler/model projection in the Cloud Control resource layer (the code that builds ResourceDescription.Properties from Lambda's function state), together with the Lambda function state that must remember that Environment was set even when its Variables map is empty — an empty map must not be normalized away into "unset" at create time or at serialization time. Absent-vs-empty has to be carried through the create handler, the stored state and the model projection.

Traces: cloudcontrol.empty-vs-absent-property-distinguished (create two AWS::Lambda::Function resources, one with Environment={"Variables":{}} and one with no Environment, then GetResource both).

## Evidence
Check cloudcontrol.empty-vs-absent-property-distinguished, line 91:
  assert "Environment" in props_empty
AssertionError: expected an explicit empty Environment block to be reported, got {'FunctionName': ..., 'Arn': ..., 'Role': ..., 'Handler': 'index.handler', 'Description': '', 'Runtime': 'python3.12', 'PackageType': 'Zip', 'MemorySize': 128, 'Timeout': 3, 'Architectures': ['x86_64'], 'TracingConfig': {'Mode': 'PassThrough'}, 'EphemeralStorage': {'Size': 512}, 'LoggingConfig': {...}, 'SnapStartResponse': {...}, 'Tags': [...], 'Code': {}}

Expected (AWS): Properties of the function created with Environment={"Variables":{}} contains an "Environment" key whose "Variables" is {}; Properties of the function created without Environment contains no "Environment" key.
Actual (emulator): neither model contains "Environment"; HTTP 200 in both cases, no error — the divergence is purely the absent member.

## Steps to Reproduce
1. Create an IAM role assumable by lambda.amazonaws.com.
2. cloudcontrol CreateResource(TypeName="AWS::Lambda::Function", DesiredState={FunctionName:<A>, Runtime:"python3.12", Handler:"index.handler", Role:<roleArn>, Code:{ZipFile:<b64 zip>}, Environment:{"Variables":{}}}) and wait for SUCCESS.
3. cloudcontrol CreateResource for a second function <B> with the same DesiredState minus the Environment member; wait for SUCCESS.
4. cloudcontrol GetResource(TypeName="AWS::Lambda::Function", Identifier=<A>) and json.loads(ResourceDescription.Properties).
5. Observe no "Environment" key, identical to the model returned for <B>. AWS returns Environment:{"Variables":{}} for <A> and no Environment key for <B>.
