Issue-Source: trace-sweep-divergence
Issue-Trace: es-application-extension-errors
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: es-application-extension-errors esval-topology-standby-single-data-node esval-topology-three-az-two-data-nodes
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: es|AttachDataSource|4xx->4xx|$.message
Issue-Key: es|CreateDomain|4xx->4xx|$.message
Issue-Commit: 249ba79e4912d920a234429df8a39f85e3c7aa16

# es ValidationException refusals carry emulator-authored message text: AttachDataSource blames the wrong member and CreateDomain's AZ/data-node rule is re-worded (2 causes)

> _Note: this issue was identified against the code base at commit `249ba79e4912d920a234429df8a39f85e3c7aa16`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AttachDataSource

## Description
Two net-new causes in the OpenSearch/ES (`es`) control plane. Both are 400 ValidationException refusals where the emulator returns the right status and error class but a message that is not the one AWS words for that error, so the refusal cannot be parsed or acted on by a client. Shared fault: the ValidationException message text (and, in cause 1, the member the validation is attributed to) is emulator-authored rather than AWS's template.

Cause 1 of 2 — AttachDataSource attributes the validation failure to `dataSourceArn` against an ARN pattern the model does not impose, instead of to the invalid `id`.
AWS: Smithy (com.amazonaws.opensearch, AttachDataSource) binds `id` as a required @httpLabel on POST /2021-01-01/opensearch/application/{id}/attachDataSource with @pattern `^[a-z0-9]{3,30}$`; `dataSourceArn` is a required ARN with @length 20..2048 and @pattern `.*` — i.e. NO ARN-shape constraint at all. Modeled refusal is ValidationException (HTTP 400). For a request whose `id` does not match the id pattern, the recorded AWS response is 400 with the standard constraint-violation template: fixed text `1 validation error detected: Value '<value>' at '<member>' failed to satisfy constraint: Member must satisfy regular expression pattern: <pattern>`, where `<value>` is the rejected value taken from the request, `<member>` is the failing member name (`id`) and `<pattern>` is that member's modeled id pattern. Only that fixed scaffolding is the requirement; the particular value, member and pattern are filled from the request and the model.
Emulator: answers 400 but names a different member and a pattern that is not in the model — `1 validation error detected: Value '<dataSourceArn value>' at 'dataSourceArn' failed to satisfy constraint: Member must satisfy regular expression pattern: arn:aws[a-z\-]*:[a-z]+:[a-z0-9\-]*:[0-9]*:[a-z0-9\-]+/[a-z0-9_\-\.]+`. So it (a) enforces an invented ARN regex on a member whose modeled pattern is `.*`, and (b) never evaluates the `id` label pattern, so the actually-invalid member is not reported.
Why: a client that reads the `at '<member>'` field to point the user at the offending input is told to fix `dataSourceArn`, which is well-formed, while the malformed application `id` passes unreported; and a caller passing any legitimately shaped non-ARN-pattern data-source ARN (the model allows it) is refused outright.
Fix: AttachDataSource's input validation — drop the synthetic ARN regex on `dataSourceArn` (keep only the modeled @length 20..2048), add the modeled `id` @pattern check, and evaluate the URI-label member before body members so the first reported violation matches AWS.
Traces: es-application-extension-errors#task#6.

Cause 2 of 2 — CreateDomain's zone-awareness data-node refusal is re-worded and hard-codes "three".
AWS: Smithy ClusterConfig.ZoneAwarenessConfig.AvailabilityZoneCount documents valid values 2 and 3, and ClusterConfig.InstanceCount is the data-node count; a mismatch is refused with ValidationException (HTTP 400, com.amazonaws.opensearch#ValidationException). The recorded AWS message is the AZ-count-independent sentence `The number of data nodes must be a multiple of the number of Availability Zones configured for the domain.` — entirely fixed text, with no request value interpolated.
Emulator: answers 400 with `You must choose a multiple of three data nodes for a three Availability Zone deployment` — emulator-authored prose that additionally bakes the AZ count into the sentence, so it is wrong text for a 2-AZ domain as well as for a 3-AZ one. Observed on both a MultiAZWithStandby single-data-node domain and a three-AZ two-data-node domain, i.e. the same handler path for every AZ count.
Why: a client matching on AWS's documented sentence (or surfacing it to a user) sees unrecognized text for a refusal it is expected to handle, and the message misstates the rule as three-AZ-specific.
Fix: CreateDomain's cluster-topology validation — emit the fixed AWS sentence for the InstanceCount-not-a-multiple-of-AvailabilityZoneCount rule instead of a per-AZ-count generated string.
Traces: esval-topology-standby-single-data-node#task#1, esval-topology-three-az-two-data-nodes#task#1.

Neither cause is covered by an existing open issue. The only open AttachDataSource issue (20261103T141919Z-opensearch-attachdatasource-is-not-idempotent-after-its-attachment.md) is about attachment idempotency, not input validation; no open issue addresses the zone-awareness refusal wording.

## Evidence
Cause 1 — fixture es-application-extension-errors, step task#6, AttachDataSource. Expected (recorded AWS): HTTP 400, $.message = "1 validation error detected: Value '00000000-0000-0000-0000-000000000000' at 'id' failed to satisfy constraint: Member must satisfy regular expression pattern: ^(?!-)[a-z0-9\-]{3,30}$". Actual (emulator): HTTP 400, $.message = "1 validation error detected: Value 'arn:aws:s3:::sim2real-probe-absent-bucket' at 'dataSourceArn' failed to satisfy constraint: Member must satisfy regular expression pattern: arn:aws[a-z\-]*:[a-z]+:[a-z0-9\-]*:[0-9]*:[a-z0-9\-]+/[a-z0-9_\-\.]+". Status and error class agree; the member name, the quoted value and the quoted pattern all diverge. Model cross-check: AttachDataSourceRequest.id @pattern ^[a-z0-9]{3,30}$ (required, httpLabel); AttachDataSourceRequest.dataSourceArn @length 20..2048 @pattern .* (no ARN shape).

Cause 2 — fixture esval-topology-standby-single-data-node, step task#1, CreateDomain (cluster also covers esval-topology-three-az-two-data-nodes#task#1). Expected (recorded AWS): HTTP 400, $.message = 'The number of data nodes must be a multiple of the number of Availability Zones configured for the domain.' Actual (emulator): HTTP 400, $.message = 'You must choose a multiple of three data nodes for a three Availability Zone deployment'. Fixed text differs wholesale; AWS's sentence interpolates nothing, the emulator's interpolates the AZ count into the wording.

Replay context: trace set simparity-wire-aws-v1, emulator up (health 200), 486 steps replayed, these two clusters reported with verdict=mismatch, stub_path empty and owned_by empty (no existing issue carries their replay keys).

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Cause 1: replay fixture es-application-extension-errors step task#6 — POST /2021-01-01/opensearch/application/00000000-0000-0000-0000-000000000000/attachDataSource with a body whose dataSourceArn is an S3 bucket ARN (arn:aws:s3:::<bucket>). Observe HTTP 400 whose message blames 'dataSourceArn' against an ARN regex; AWS instead blames 'id' against the modeled ^[a-z0-9]{3,30}$ pattern.
3. Cause 2: replay fixture esval-topology-standby-single-data-node step task#1 — CreateDomain with ZoneAwarenessEnabled/MultiAZWithStandby and a data-node InstanceCount that is not a multiple of AvailabilityZoneCount (also esval-topology-three-az-two-data-nodes#task#1: 3 AZs, 2 data nodes). Observe HTTP 400 with 'You must choose a multiple of three data nodes for a three Availability Zone deployment' instead of AWS's 'The number of data nodes must be a multiple of the number of Availability Zones configured for the domain.'
4. Re-run replay_wire_corpus() and confirm both clusters pass.
