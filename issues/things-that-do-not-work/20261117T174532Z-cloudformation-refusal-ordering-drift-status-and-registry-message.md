Issue-Source: trace-sweep-divergence
Issue-Trace: create-stack-template-refusals-past-validation
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: create-stack-template-refusals-past-validation cfnarch-stale-create-complete-after-backing-record-deleted cloudformation-register-type-error
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation|CreateStack|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: cloudformation|DescribeStackResource|2xx->2xx|$.DescribeStackResourceResponse.DescribeStackResourceResult.StackResourceDetail.DriftInformation.StackResourceDriftStatus
Issue-Key: cloudformation|RegisterType|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Related: 20261110T100112Z-cloudformation-stack-policy-and-non-terminal-state-refusal.md
Issue-Commit: 0d074a71eaca712c4eb2c65274b3985903118640

# CloudFormation refusal ordering, drift status and registry message diverge from AWS (3 causes)

> _Note: this issue was identified against the code base at commit `0d074a71eaca712c4eb2c65274b3985903118640`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Three net-new CloudFormation causes found by the wire replay. They share one shape of fault: the operation answers with the WRONG refusal/status value even though the HTTP status class is right — a request-validation result is replaced by a state-collision result, a drift-detection result is replaced by the never-checked default, and a registry refusal message is emulator-authored. This issue carries 3 CAUSES; all three must be closed before it is submitted.

=== Cause 1 of 4 (of 3): CreateStack returns AlreadyExistsException instead of the template ValidationError ===
Fixture: create-stack-template-refusals-past-validation, step task#3 (cluster also covers task#4, task#5, task#6, task#7, task#8, task#9 of the same fixture — 7 steps).
AWS: Smithy (com.amazonaws.cloudformation#CreateStack) models CreateStack with input members TemplateBody (@length min 1) / TemplateURL and errors AlreadyExistsException, InsufficientCapabilitiesException, LimitExceededException, TokenAlreadyExistsException — all awsQuery code=<same name>, httpResponseCode 400. Template-structure refusals are NOT in that modeled error set: AWS reports them as the service-common query error Code 'ValidationError' with HTTP 400 and a message of the form 'Template error: <the specific structural problem>' — here 'Template error: unresolved condition dependency Missing in Fn::If', whose fixed text is 'Template error: unresolved condition dependency ' + ' in Fn::If' and whose variable part is the condition name taken from the submitted template (here the logical condition name). AWS performs this template parse/validation on every CreateStack and reports it in preference to the stack-name state check; a CreateStack that AWS refuses creates NO stack, so the requested StackName stays free for the next attempt.
Emulator: every one of the seven template-refusal probes in this fixture answers HTTP 400 with $.ErrorResponse.Error.Code = 'AlreadyExistsException' and $.ErrorResponse.Error.Message = 'Stack [<stackName>] already exists' instead of Code 'ValidationError' with the 'Template error: ...' message. Status is 400 in both, so only the code and message diverge.
Hypothesis: the stack-name collision check runs ahead of template parsing, and/or a refused CreateStack still inserts a stack record that reserves the name, so the first probe poisons every later probe of the same name.
Why: a client cannot distinguish 'my template is malformed' from 'that stack name is taken'. SDK/CLI callers and CDK/`deploy` wrappers branch on the error code: AlreadyExistsException drives them to update or adopt an existing stack, so a malformed template is retried as an update or reported as a spurious name conflict, and the actual template defect is never surfaced to the author.
Fix: CreateStack's request-validation path in the CloudFormation operation handler — run template parse/condition-resolution validation and emit the ValidationError before consulting stack state for a name collision, and do not register a stack record for a create that is refused.
Traces: create-stack-template-refusals-past-validation#task#3 (plus task#4..task#9 of the same fixture).

=== Cause 2 of 3: DescribeStackResource reports StackResourceDriftStatus NOT_CHECKED after drift was detected ===
Fixture: cfnarch-stale-create-complete-after-backing-record-deleted, step task#6 (cluster also covers task#8 of the same fixture).
AWS: Smithy StackResourceDriftInformation.StackResourceDriftStatus documents the enum as DELETED / MODIFIED / NOT_CHECKED / IN_SYNC, and states explicitly that NOT_CHECKED means 'CloudFormation has not checked if the resource differs from its expected configuration' while IN_SYNC means 'The resource's actual configuration matches its expected configuration'. Once drift has been detected for a resource, DescribeStackResource's StackResourceDetail.DriftInformation must report that recorded result (IN_SYNC here), not the never-checked default.
Emulator: $.DescribeStackResourceResponse.DescribeStackResourceResult.StackResourceDetail.DriftInformation.StackResourceDriftStatus = 'NOT_CHECKED' where IN_SYNC is expected; HTTP 200 on both sides.
Why: drift status is the whole point of the drift API. A client that detects drift and then reads the resource back can never observe a settled verdict — IN_SYNC, MODIFIED and DELETED are all indistinguishable from 'never checked', so drift-gating automation either loops forever or treats a genuinely drifted resource as unknown and skips remediation.
Fix: the CloudFormation drift state — DetectStackResourceDrift must persist the per-resource drift verdict and timestamp onto the stack resource, and DescribeStackResource's serializer must project that stored verdict into DriftInformation instead of hard-defaulting NOT_CHECKED. Related but distinct open issue: 20261103T171935Z-cloudformation-state-guards-misfire-operations-accepted-or-refused.md owns this fixture's task#3 DetectStackResourceDrift 400; it does not cover the DescribeStackResource projection.
Traces: cfnarch-stale-create-complete-after-backing-record-deleted#task#6 (plus task#8).

=== Cause 3 of 3: RegisterType's CFNRegistryException message is emulator-authored ===
Fixture: cloudformation-register-type-error, step task#1.
AWS: Smithy models RegisterType with required TypeName (@pattern ^[A-Za-z0-9]{2,64}::[A-Za-z0-9]{2,64}::[A-Za-z0-9]{2,64}(::MODULE){0,1}$, @length 10..204) and required SchemaHandlerPackage (S3Url, @length 1..4096), and exactly one modeled error: CFNRegistryException (client, HTTP 400, awsQuery code CFNRegistryException). Recorded behavior only for the message: the corpus asserts the recorded message by shape rather than literal text, so the exact AWS sentence is redacted in the fixture — what the recording establishes is that AWS's CFNRegistryException message for an unreadable schema handler package is NOT the fixed sentence the emulator serves, and that it carries request-derived variable parts (the submitted SchemaHandlerPackage S3 URL / type name) rather than being a constant.
Emulator: HTTP 400 with $.ErrorResponse.Error.Message = 'SchemaHandlerPackage must reference a valid S3 object to which you have access.' — a constant sentence with no request-derived content, which does not match the recorded message. Status and error code both match; only the message diverges.
Why: a caller or test asserting on the registry refusal text (or parsing it to learn WHICH package reference was rejected) gets a sentence AWS never sends and no indication of the offending S3 URL, so registry-publication tooling cannot report an actionable cause.
Fix: RegisterType's validation/error-construction path in the CloudFormation operation handler — word the CFNRegistryException message as AWS words it for an unreadable/inaccessible schema handler package, interpolating the submitted SchemaHandlerPackage URL (and type name) from the request instead of emitting a fixed sentence.
Traces: cloudformation-register-type-error#task#1.

## Evidence
Cause 1 — fixture create-stack-template-refusals-past-validation#task#3, operation CreateStack, expected_status 400 / actual_status 400:
  expected $.ErrorResponse.Error.Code = 'ValidationError'; actual 'AlreadyExistsException'
  expected $.ErrorResponse.Error.Message = 'Template error: unresolved condition dependency Missing in Fn::If'; actual 'Stack [sim2real-probe-cfn-3135679a] already exists'
  (fixed text of the expected message: 'Template error: unresolved condition dependency ' ... ' in Fn::If'; variable part: the unresolved condition name from the submitted template. The emulator message's variable part is the StackName from the request. The stack name and probe suffix in the recording are per-run values, not requirements.)
  affected_count 7: task#3, task#4, task#5, task#6, task#7, task#8, task#9 of the same fixture.

Cause 2 — fixture cfnarch-stale-create-complete-after-backing-record-deleted#task#6, operation DescribeStackResource, expected_status 200 / actual_status 200:
  $.DescribeStackResourceResponse.DescribeStackResourceResult.StackResourceDetail.DriftInformation.StackResourceDriftStatus: value_mismatch (expected 'IN_SYNC', got 'NOT_CHECKED')
  affected_count 2: task#6 and task#8.

Cause 3 — fixture cloudformation-register-type-error#task#1, operation RegisterType, expected_status 400 / actual_status 400:
  $.ErrorResponse.Error.Message: value_mismatch (expected the recorded AWS message, shape-asserted in the corpus; got 'SchemaHandlerPackage must reference a valid S3 object to which you have access.')
  affected_count 1.

Model citations: Smithy CreateStack errors = {AlreadyExistsException, InsufficientCapabilitiesException, LimitExceededException, TokenAlreadyExistsException} (no template-structure error — AWS uses common Code 'ValidationError'); Smithy StackResourceDriftInformation.StackResourceDriftStatus doc (NOT_CHECKED = 'CloudFormation has not checked...', IN_SYNC = 'actual configuration matches its expected configuration'); Smithy RegisterType errors = {CFNRegistryException} with required SchemaHandlerPackage: S3Url.

## Steps to Reproduce
Boot the emulator from this checkout and replay the simparity-wire-aws-v1 corpus (replay_wire_corpus), or exercise each cause directly:

Cause 1: replay fixture 'create-stack-template-refusals-past-validation' and read step task#3 (also task#4..task#9). Equivalently: CreateStack with a StackName and a template whose Fn::If references an undeclared condition. Expect HTTP 400 with Error.Code 'ValidationError' and Error.Message 'Template error: unresolved condition dependency <conditionName> in Fn::If'; observe Error.Code 'AlreadyExistsException' and 'Stack [<name>] already exists'. Confirm the ordering defect by issuing the invalid-template CreateStack first against a never-used StackName and then re-issuing it.

Cause 2: replay fixture 'cfnarch-stale-create-complete-after-backing-record-deleted' and read steps task#6 and task#8. Equivalently: create a stack, run DetectStackResourceDrift for one of its resources, then DescribeStackResource for that LogicalResourceId. Expect StackResourceDetail.DriftInformation.StackResourceDriftStatus 'IN_SYNC'; observe 'NOT_CHECKED'.

Cause 3: replay fixture 'cloudformation-register-type-error' and read step task#1. Equivalently: RegisterType with a valid TypeName and a SchemaHandlerPackage S3 URL that cannot be read. Expect HTTP 400 CFNRegistryException with AWS's message naming the submitted package; observe the fixed sentence 'SchemaHandlerPackage must reference a valid S3 object to which you have access.'
