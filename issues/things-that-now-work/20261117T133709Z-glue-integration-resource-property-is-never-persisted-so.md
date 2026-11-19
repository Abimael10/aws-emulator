Issue-Source: trace-sweep-divergence
Issue-Trace: glue-integration-resource-property-lifecycle-and-validation
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: glue-integration-resource-property-lifecycle-and-validation
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue|GetIntegrationResourceProperty|2xx->4xx|$.ResourceArn
Issue-Key: glue|GetIntegrationResourceProperty|2xx->4xx|$.ResourcePropertyArn
Issue-Key: glue|GetIntegrationResourceProperty|2xx->4xx|$.TargetProcessingProperties
Issue-Key: glue|GetIntegrationResourceProperty|2xx->4xx|status
Issue-Key: glue|DeleteIntegrationResourceProperty|2xx->4xx|$
Issue-Key: glue|DeleteIntegrationResourceProperty|2xx->4xx|status
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# Glue integration resource property is never persisted, so read and delete refuse

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetIntegrationResourceProperty

## Description
After a successful CreateIntegrationResourceProperty, the emulator cannot find the property again: both the read and the delete answer 400 EntityNotFoundException where AWS answers 200. The write path reports success without committing state (or commits it under a key the read path does not use).

Cause 1 of 2: GetIntegrationResourceProperty for a property that was just created.
AWS: Smithy: GetIntegrationResourceProperty input ResourceArn @required (String512, length 1-512); output ResourceArn, ResourcePropertyArn, SourceProcessingProperties, TargetProcessingProperties; errors include EntityNotFoundException (400). Recorded behavior: 200 echoing ResourceArn (the database ARN that was passed in), a ResourcePropertyArn derived from it (arn:aws:glue:<region>:<account>:integrationresourceproperty/<resource-relative-id>) and the TargetProcessingProperties stored at create time (e.g. its RoleArn).
Emulator: 400 EntityNotFoundException with none of the three members.
Why: the resource appears to be created and is then invisible, so a client cannot verify, update or tear down the integration property it just configured.
Fix: the CreateIntegrationResourceProperty state write and the GetIntegrationResourceProperty lookup (they must agree on the ResourceArn key).
Traces: glue-integration-resource-property-lifecycle-and-validation#task#3.

Cause 2 of 2: DeleteIntegrationResourceProperty for the same property.
AWS: Smithy: DeleteIntegrationResourceProperty input ResourceArn @required; output is an EMPTY structure; errors include EntityNotFoundException (400). Recorded behavior: 200 with an empty body ({}).
Emulator: 400 with __type 'EntityNotFoundException' and Message 'Failed to find the resource property with arn <resourceArn> in the table' — an emulator-authored sentence for a refusal AWS does not make here at all.
Why: teardown fails, so the lifecycle cannot be completed or repeated idempotently.
Fix: the DeleteIntegrationResourceProperty handler's state lookup (same keying defect as cause 1).
Traces: glue-integration-resource-property-lifecycle-and-validation#task#6.

## Evidence
1. glue-integration-resource-property-lifecycle-and-validation#task#3 GetIntegrationResourceProperty — status expected 200, got 400; $.ResourceArn expected 'arn:aws:glue:us-west-2:000000000000:database/<db>', $.ResourcePropertyArn expected 'arn:aws:glue:us-west-2:000000000000:integrationresourceproperty/database/<db>', $.TargetProcessingProperties expected {'RoleArn':'arn:aws:iam::000000000000:role/<role>'} — all absent.
2. glue-integration-resource-property-lifecycle-and-validation#task#6 DeleteIntegrationResourceProperty — status expected 200 with body {}, got 400 {'__type':'EntityNotFoundException','Message':'Failed to find the resource property with arn arn:aws:glue:us-west-2:000000000000:database/<db> in the table'}.
(The region/account/db/role values above are one run's; the requirement is the 200, the echoed ResourceArn, the derived ResourcePropertyArn form and the stored TargetProcessingProperties.)

## Steps to Reproduce
1. Replay the wire corpus (replay_wire_corpus).
2. Create a Glue database, then call CreateIntegrationResourceProperty for its ARN with TargetProcessingProperties.RoleArn set — it reports success.
3. Call GetIntegrationResourceProperty with the same ResourceArn — 400 EntityNotFoundException instead of 200 with the stored properties.
4. Call DeleteIntegrationResourceProperty with the same ResourceArn — 400 EntityNotFoundException instead of 200 with an empty body.
