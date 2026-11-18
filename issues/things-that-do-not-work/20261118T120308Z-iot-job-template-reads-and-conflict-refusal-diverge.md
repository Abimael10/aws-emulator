Issue-Source: trace-sweep-divergence
Issue-Trace: iot-job-continuous-lifecycle
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-job-continuous-lifecycle
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|DescribeJobTemplate|2xx->2xx|$.jobExecutionsRolloutConfig
Issue-Key: iot|DescribeJobTemplate|2xx->2xx|$.presignedUrlConfig
Issue-Key: iot|DescribeJobTemplate|2xx->2xx|$.timeoutConfig
Issue-Key: iot|CreateJobTemplate|4xx->4xx|$.message
Issue-Key: iot|CreateJobTemplate|4xx->4xx|$.resourceId
Issue-Commit: 53a07ddf88f361ed2bd809df47996759a4dc983b

# iot job-template reads and conflict refusal diverge: DescribeJobTemplate drops unset config containers, CreateJobTemplate mis-words its 409

> _Note: this issue was identified against the code base at commit `53a07ddf88f361ed2bd809df47996759a4dc983b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeJobTemplate

## Description
Two net-new causes on the IoT job-template surface (both replayed from fixture iot-job-continuous-lifecycle). Neither is owned by an open issue: the replay marked both clusters unowned. A fixer must close BOTH before submitting.

Cause 1 of 2 — DescribeJobTemplate omits the config container members AWS returns for a template created without them (fixture iot-job-continuous-lifecycle, step task#4).
AWS: Smithy (services/service-com-amazonaws-iot-awsiotservice) DescribeJobTemplate -> GET /job-templates/{jobTemplateId}, output DescribeJobTemplateResponse carries presignedUrlConfig (PresignedUrlConfig {roleArn, expiresInSec}), jobExecutionsRolloutConfig (JobExecutionsRolloutConfig {maximumPerMinute, exponentialRate}) and timeoutConfig (TimeoutConfig {inProgressTimeoutInMinutes}). The model does not state whether an unset container is serialized, so: recorded behavior only — the recording shows AWS emitting all three container members on the 200 for a job template created WITHOUT any of those configs, each container present with its own members unset. The read projection is therefore expected to echo the container structures the job-template record always holds, not to prune them when their contents are defaulted.
Emulator: the 200 body omits the three keys entirely. Replay reports `$.jobExecutionsRolloutConfig: missing (expected {'exponentialRate': None, 'maximumPerMinute': None}, got None)`, `$.presignedUrlConfig: missing (expected {'expiresInSec': None, 'roleArn': None}, got None)`, `$.timeoutConfig: missing (expected {'inProgressTimeoutInMinutes': None}, got None)`. Status matches (200).
Why: a client that reads a job template to inspect or round-trip its rollout/timeout/presigned-URL settings (for example to re-submit them on a CreateJob, or to render them) gets absent containers where AWS gives it present-but-empty ones. Typed SDKs surface these as null structures instead of structures with null fields, so code that dereferences template.jobExecutionsRolloutConfig() to read maximumPerMinute null-faults instead of reading an unset value, and any describe->recreate round-trip silently loses the fact that the template has these sections at all.
Fix: the DescribeJobTemplate read projection/serializer for the iot service — the output must emit the presignedUrlConfig, jobExecutionsRolloutConfig and timeoutConfig containers that the stored job-template record holds rather than skipping containers whose members are unset.
Traces: iot-job-continuous-lifecycle#task#4 (DescribeJobTemplate, 200 vs 200, 1 affected step).

Cause 2 of 2 — CreateJobTemplate's duplicate-id 409 mis-words the message and adds a resourceId AWS does not populate (fixture iot-job-continuous-lifecycle, step task#15).
AWS: Smithy CreateJobTemplate -> PUT /job-templates/{jobTemplateId}, errors include ConflictException (client, httpError 409); ConflictException members are message and resourceId ("A resource with the same name already exists."). The refusal text is prose-only in the model, so: recorded behavior only — re-creating an existing job template answers 409 with message "Job Template <jobTemplateId> already exists." and NO resourceId member. The fixed text of that message is "Job Template " ... " already exists." with the capital T in "Template"; the only variable part is the jobTemplateId taken from the request URI label.
Emulator: answers 409 but with message "Job template <jobTemplateId> already exists." — lowercase "template" in the fixed text — and additionally sets `resourceId` to the request's jobTemplateId, which AWS leaves unset on this refusal. Replay reports `$.message: value_mismatch (expected 'Job Template <id> already exists.', got 'Job template <id> already exists.')` and `$.resourceId: type_mismatch (expected None, got <the request's jobTemplateId>)`.
Why: the error body is a requirement, not cosmetics. Clients and test suites that match the ConflictException text (or that treat a populated resourceId as meaning the service identified a different conflicting resource) read a different refusal than AWS produced: idempotency wrappers keyed on the exact sentence stop recognizing the duplicate-template case, and a resourceId present only against the emulator makes emulator-only branches reachable.
Fix: the CreateJobTemplate handler's duplicate-id refusal path in the iot service — emit AWS's fixed sentence with the capital-T "Job Template" and leave ConflictException.resourceId unset for this case.
Traces: iot-job-continuous-lifecycle#task#15 (CreateJobTemplate, 409 vs 409, 1 affected step).

Related but NOT covering these: open issue 20261109T223435Z-cloudformation-iot-iot-createjobtemplate-is-an-unimplemented-scaffold.md describes CreateJobTemplate as an unimplemented stub whose duplicate-id refusal is unreachable — the operation now answers 409, so the refusal is reachable and its wording/extra member are the net-new divergence filed here. Open issue 20261114T091403Z-iot-job-updates-and-thing-group-membership-are.md covers the Job (not JobTemplate) projection. Open issue 20261115T161531Z-iot-managed-job-template-catalog-diverges-5-built.md covers the built-in MANAGED job template catalog, a different surface.

## Evidence
Cause 1 — DescribeJobTemplate, iot-job-continuous-lifecycle#task#4, expected 200 / actual 200:
  expected $.jobExecutionsRolloutConfig = {'exponentialRate': None, 'maximumPerMinute': None}; actual: key absent (None)
  expected $.presignedUrlConfig       = {'expiresInSec': None, 'roleArn': None};              actual: key absent (None)
  expected $.timeoutConfig            = {'inProgressTimeoutInMinutes': None};                  actual: key absent (None)

Cause 2 — CreateJobTemplate, iot-job-continuous-lifecycle#task#15, expected 409 / actual 409:
  expected $.message    = 'Job Template <jobTemplateId> already exists.'   (fixed text: 'Job Template ' + ' already exists.'; variable: jobTemplateId from the request)
  actual   $.message    = 'Job template <jobTemplateId> already exists.'   (fixed text diverges: lowercase 'template')
  expected $.resourceId = absent/unset
  actual   $.resourceId = the request's jobTemplateId

Model citations: Smithy services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json — DescribeJobTemplate output members presignedUrlConfig/jobExecutionsRolloutConfig/timeoutConfig; CreateJobTemplate error ConflictException (httpError 409) with members {message, resourceId}.

## Steps to Reproduce
Replay the wire corpus (replay_wire_corpus) and inspect fixture iot-job-continuous-lifecycle, or by hand against the emulator's iot endpoint:

Cause 1:
1. PUT /job-templates/<templateId> (CreateJobTemplate) with only jobTemplateId, document and description — no presignedUrlConfig, jobExecutionsRolloutConfig or timeoutConfig.
2. GET /job-templates/<templateId> (DescribeJobTemplate) -> step task#4. Observe the 200 body has no presignedUrlConfig, jobExecutionsRolloutConfig or timeoutConfig keys; AWS returns all three containers with their members unset.

Cause 2:
3. PUT /job-templates/<templateId> again with the SAME jobTemplateId (CreateJobTemplate) -> step task#15. Observe 409 with message 'Job template <templateId> already exists.' plus a resourceId member; AWS answers 409 with 'Job Template <templateId> already exists.' and no resourceId.
