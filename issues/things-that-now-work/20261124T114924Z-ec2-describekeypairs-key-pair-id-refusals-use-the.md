Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-asgssh-traj-key-pair-import-format-and-lookup-mistakes
Issue-Step: task#11
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-asgssh-traj-key-pair-import-format-and-lookup-mistakes ec2-create-tags-delete-tags-key-pair-existence
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeKeyPairs|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|DescribeKeyPairs|4xx->4xx|$.Response.Errors.Error.Message
Issue-Commit: a5b444fd2887334b1205747ac6df31fc24ea8f9f

# ec2 DescribeKeyPairs key-pair-id refusals use the wrong error code and non-AWS message wording (2 causes)

> _Note: this issue was identified against the code base at commit `a5b444fd2887334b1205747ac6df31fc24ea8f9f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeKeyPairs

## Description
DescribeKeyPairs refuses an unresolvable KeyPairId with a single home-grown refusal, where EC2 uses two distinct refusal classes and different message text.

Cause 1 of 2 - wrong error code for a keyPairId EC2 rejects as an invalid parameter value (fixture ec2-asgssh-traj-key-pair-import-format-and-lookup-mistakes, step task#11).
AWS: recorded behavior only - DescribeKeyPairs declares no modeled errors (Smithy: DescribeKeyPairs input KeyPairIds -> KeyPairIdStringList, xmlName KeyPairId; errors: []), so the recorded response is the requirement. EC2 answers HTTP 400 with the EC2-query error envelope Response/Errors/Error, Code 'InvalidParameterValue' and Message "Invalid value '<value>' for keyPairId." - fixed text "Invalid value '" + the rejected keyPairId value, taken verbatim from the request + "' for keyPairId." Hypothesis: EC2 validates the KeyPairId value before any state lookup and reports an invalid value rather than a miss; the exact predicate that separates 'invalid value' from 'does not exist' is not stated by the model or the recording, so a fixer should read the recorded request of this step to pin the discriminator.
Emulator: answers HTTP 400 but with Code 'InvalidKeyPair.NotFound' and Message "The key pair ID '<id>' does not exist" - it has only the not-found class, so the invalid-value class never fires.
Why: clients branch on the error Code. Tooling that retries or re-imports on InvalidParameterValue (a bad argument) versus treating InvalidKeyPair.NotFound as 'create it' takes the wrong branch, and an SDK waiter/validator that distinguishes malformed input from a missing resource mis-reports the failure to the user.
Fix: the DescribeKeyPairs handler's KeyPairId validation, ahead of the state lookup (services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_key_pairs.rs).

Cause 2 of 2 - not-found message wording (fixture ec2-create-tags-delete-tags-key-pair-existence, step task#7).
AWS: recorded behavior only. For a well-formed key-pair id that is not in the account EC2 answers HTTP 400, Code 'InvalidKeyPair.NotFound', Message "The keyPairId '<id>' does not exist" - fixed text "The keyPairId '" and "' does not exist", with the id taken from the request's KeyPairId value.
Emulator: same status and code, but the message is "The key pair ID '<id>' does not exist" - it rewrites the fixed text, spelling the parameter as three English words instead of EC2's wire parameter name 'keyPairId'.
Why: the message is part of the contract. Fixtures, log assertions and user-facing error surfaces that match EC2's wording do not match, and a client that parses the parameter name out of the message reads a name that is not an EC2 parameter.
Fix: the message template in the DescribeKeyPairs not-found refusal (services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_key_pairs.rs); only the fixed text changes - the id stays a value from the request.

Neither cluster is attributed to an open issue by the replay.

## Evidence
Cause 1 - fixture ec2-asgssh-traj-key-pair-import-format-and-lookup-mistakes, step task#11, verdict mismatch, expected status 400 / actual status 400:
  $.Response.Errors.Error.Code: expected 'InvalidParameterValue', got 'InvalidKeyPair.NotFound'
  $.Response.Errors.Error.Message: expected "Invalid value 'key-0123456789abcdef0' for keyPairId.", got "The key pair ID 'key-0123456789abcdef0' does not exist"
  (the id 'key-0123456789abcdef0' is the request's value, not part of the template)

Cause 2 - fixture ec2-create-tags-delete-tags-key-pair-existence, step task#7, verdict mismatch, expected status 400 / actual status 400:
  $.Response.Errors.Error.Message: expected "The keyPairId 'key-000000000000000a7' does not exist", got "The key pair ID 'key-000000000000000a7' does not exist"
  (Code matches: InvalidKeyPair.NotFound; only the fixed text 'The keyPairId' vs 'The key pair ID' diverges - 'key-000000000000000a7' is the request's value)

Smithy (services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json): DescribeKeyPairs input members KeyNames -> KeyNameStringList, KeyPairIds -> KeyPairIdStringList (xmlName KeyPairId), Filters, IncludePublicKey, DryRun; errors: [] - the refusal codes and messages are therefore recorded behavior.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Cause 1: replay fixture ec2-asgssh-traj-key-pair-import-format-and-lookup-mistakes step task#11 - a DescribeKeyPairs call whose KeyPairId value EC2 rejects as an invalid parameter value. Expect HTTP 400 with Response/Errors/Error/Code 'InvalidParameterValue' and Message "Invalid value '<value>' for keyPairId."; the emulator returns Code 'InvalidKeyPair.NotFound' with the not-found message.
3. Cause 2: replay fixture ec2-create-tags-delete-tags-key-pair-existence step task#7 - DescribeKeyPairs for a key-pair id that does not exist. Expect Message "The keyPairId '<id>' does not exist"; the emulator returns "The key pair ID '<id>' does not exist".
