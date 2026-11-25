Issue-Source: trace-sweep-divergence
Issue-Trace: medialive-account-configuration-roundtrip
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: medialive-account-configuration-roundtrip
Issue-Shape-Id: com.amazonaws.medialive#MediaLive
Issue-Key: medialive|DescribeAccountConfiguration|2xx->2xx|$.accountConfiguration.kmsKeyId
Issue-Commit: 75f577a9b4ae922afffed072560bc3cac4bdc4f1

# medialive DescribeAccountConfiguration omits kmsKeyId after UpdateAccountConfiguration sets it

> _Note: this issue was identified against the code base at commit `75f577a9b4ae922afffed072560bc3cac4bdc4f1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAccountConfiguration

## Description
1 cause.

Cause 1 of 1: the account-level KMS key does not survive the write
AWS: Smithy: UpdateAccountConfiguration (PUT /prod/accountConfiguration, 200) takes and returns AccountConfiguration, whose single member KmsKeyId (jsonName kmsKeyId) is documented 'Specifies the KMS key to use for all features that use key encryption. Specify the ARN of a KMS key that you have created. Or leave blank to use the key that MediaLive creates and manages for you.'; DescribeAccountConfiguration (GET /prod/accountConfiguration, 200) returns the same AccountConfiguration shape. So once an update sets kmsKeyId, every later describe reports that key until it is changed - the member is absent only while no key has been configured.
Emulator: answers 200 with $.accountConfiguration present but kmsKeyId absent (null) - both on the describe immediately following the update (task#2) and on the later describe in the same fixture (task#6). The account configuration write is therefore accepted and discarded, or stored somewhere the describe projection does not read.
Why: the KMS key is a security-relevant account setting. A client that configures a customer-managed key and reads it back is told no key is configured, so it cannot verify encryption, and a converging configuration loop (apply, read, compare) never converges - it re-applies the key forever. A client that reads the configuration to decide whether MediaLive's service-managed key is in use draws the opposite conclusion from the truth.
Fix: the account-configuration handlers in services/service-com-amazonaws-medialive-medialive/src/ops - UpdateAccountConfiguration must persist the submitted kmsKeyId into the account-scoped state in src/state.rs, and DescribeAccountConfiguration must project that stored value into accountConfiguration.kmsKeyId (emitting the member only when a key is configured).
Traces: medialive-account-configuration-roundtrip#task#2 (cluster also covers #task#6).

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout, DescribeAccountConfiguration, expected 200 / actual 200, fixture medialive-account-configuration-roundtrip steps task#2 and task#6:
'$.accountConfiguration.kmsKeyId: missing (expected \'{{baseline_kms_key_id}}\', got None)'
The expected value is the fixture's own variable for the key ARN the preceding UpdateAccountConfiguration submitted - the requirement is that describe echo whatever key the update stored, not that one particular ARN appear. Smithy (services/service-com-amazonaws-medialive-medialive/smithy-model): AccountConfiguration{KmsKeyId -> jsonName kmsKeyId}; UpdateAccountConfiguration PUT /prod/accountConfiguration returns AccountConfiguration; DescribeAccountConfiguration GET /prod/accountConfiguration returns AccountConfiguration.

## Steps to Reproduce
1. Replay fixture medialive-account-configuration-roundtrip: UpdateAccountConfiguration with accountConfiguration.kmsKeyId set to a KMS key ARN (earlier step of the fixture).
2. Call DescribeAccountConfiguration (step task#2).
3. Observe the 200 body contains accountConfiguration with no kmsKeyId member; repeating the describe later in the fixture (step task#6) gives the same result.
