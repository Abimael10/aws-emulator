Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__lambda__signer.deploy-lambda-codesigningconfig-round-trip
Issue-Key: cloudformation__lambda__signer.deploy-lambda-codesigningconfig-round-trip|run#8|AssertionError|assert status == "CREATE_COMPLETE", (
Issue-Commit: 6ad539f79015bc27d2d0777d4c5c145ae7d45c50

# CloudFormation refuses a valid AWS::Lambda::CodeSigningConfig whose AllowedPublishers comes from Fn::GetAtt Profile.ProfileVersionArn: Csc goes CREATE_FAILED and the stack rolls back

> _Note: this issue was identified against the code base at commit `6ad539f79015bc27d2d0777d4c5c145ae7d45c50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: a well-formed AWS::Lambda::CodeSigningConfig cannot be created by CloudFormation — the resource fails and the whole stack rolls back.

AWS: a stack with an AWS::Signer::SigningProfile ('Profile', PlatformId AWSLambda-SHA384-ECDSA) and an AWS::Lambda::CodeSigningConfig ('Csc') whose AllowedPublishers.SigningProfileVersionArns is [{"Fn::GetAtt": ["Profile", "ProfileVersionArn"]}] and CodeSigningPolicies.UntrustedArtifactOnDeployment is 'Enforce' reaches CREATE_COMPLETE: the GetAtt resolves to the profile's live profileVersionArn, which satisfies the resource type's SigningProfileVersionArns ARN pattern, and lambda:GetCodeSigningConfig then reads the config back with that publisher and policy. There is no error code in the AWS path — the status is a successful create. Recorded behavior only: resource-type property schemas are not part of CloudFormation's Smithy model.
Emulator: the resource 'Csc' goes CREATE_FAILED and the stack settles ROLLBACK_COMPLETE — the observed event sequence is [('Profile','CREATE_COMPLETE'), ('Csc','CREATE_FAILED'), ('Profile','DELETE_COMPLETE'), ('<stack>','ROLLBACK_COMPLETE')]. The sibling negative check in the same service (a deliberately non-ARN publisher) now also rolls back, so the refusal is not selective: a valid Fn::GetAtt-sourced publisher ARN is refused exactly like a malformed literal. Hypothesis: the publisher-ARN validation runs against the UNRESOLVED Fn::GetAtt intrinsic (or against a profileVersionArn the Signer resource reports in a shape the pattern rejects) rather than against the resolved value.
Why: no CloudFormation-managed Lambda code-signing configuration can be deployed at all. A template that AWS deploys green rolls back, taking the signing profile with it, so code-signing enforcement cannot be provisioned by stack and anything downstream of the config ARN (a function's CodeSigningConfigArn) cannot be deployed either.
Fix: the AWS::Lambda::CodeSigningConfig resource handler in CloudFormation — resolve Fn::GetAtt (Profile.ProfileVersionArn) to the Signer profile's live profileVersionArn BEFORE validating AllowedPublishers.SigningProfileVersionArns against the ARN pattern, and create the config via Lambda's CreateCodeSigningConfig so the resource reaches CREATE_COMPLETE. Note the open issue 20261109T154607Z-cloudformation-lambda-signer-2-net-new-causes-stack.md lists this check, but under a different failure (a KeyError on a DescribeStacks Output with no OutputValue, reached only AFTER a successful create); it does not cover this refusal, which now stops the stack before any Output exists.
Traces: cloudformation__lambda__signer.deploy-lambda-codesigningconfig-round-trip.

## Evidence
Behavioral suite replay against the emulator built from this checkout; service cloudformation__lambda__signer 0/2 (0%).

[mismatch] cloudformation__lambda__signer.deploy-lambda-codesigningconfig-round-trip:
  File check_cloudformation__lambda__signer_deploy_lambda_codesigningconfig_round_trip.py, line 201, in run
  assert status == "CREATE_COMPLETE"
  AssertionError: stack ended 'ROLLBACK_COMPLETE': [('Profile', 'CREATE_COMPLETE'), ('Csc', 'CREATE_FAILED'), ('Profile', 'DELETE_COMPLETE'), ('sim2real-cfn-csc-e605b08615a0', 'ROLLBACK_COMPLETE')]

Template under test: Resources {Profile: AWS::Signer::SigningProfile {ProfileName, PlatformId: AWSLambda-SHA384-ECDSA}, Csc: AWS::Lambda::CodeSigningConfig {Description, AllowedPublishers: {SigningProfileVersionArns: [{Fn::GetAtt: [Profile, ProfileVersionArn]}]}, CodeSigningPolicies: {UntrustedArtifactOnDeployment: Enforce}}}, Outputs ProfileVersionArnOut/CscArnOut/CscIdOut.
Expected instead: the stack settles CREATE_COMPLETE, lambda:ListCodeSigningConfigs lists exactly one config under the template's Description with (CodeSigningConfigArn, CodeSigningConfigId) equal to (CscArnOut, CscIdOut), and lambda:GetCodeSigningConfig returns AllowedPublishers {SigningProfileVersionArns: [<profile's live profileVersionArn>]} and CodeSigningPolicies {UntrustedArtifactOnDeployment: 'Enforce'}. (The stack name in the trace is this run's generated name.)

## Steps to Reproduce
1. Clone the repo and build/boot the emulator from this branch.
2. CreateStack with the template above (AWS::Signer::SigningProfile on PlatformId AWSLambda-SHA384-ECDSA plus AWS::Lambda::CodeSigningConfig whose AllowedPublishers.SigningProfileVersionArns is [{Fn::GetAtt: [Profile, ProfileVersionArn]}]).
3. Poll DescribeStacks/DescribeStackEvents: observe 'Csc' CREATE_FAILED and the stack ROLLBACK_COMPLETE instead of CREATE_COMPLETE.
4. Replay cloudformation__lambda__signer.deploy-lambda-codesigningconfig-round-trip.
