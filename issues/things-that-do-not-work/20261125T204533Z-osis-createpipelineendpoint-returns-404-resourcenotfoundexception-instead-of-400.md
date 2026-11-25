Issue-Source: trace-sweep-divergence
Issue-Trace: osis-pipeline-endpoint-error-paths
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: osis-pipeline-endpoint-error-paths
Issue-Shape-Id: com.amazonaws.osis#AmazonOpenSearchIngestionService
Issue-Key: osis|CreatePipelineEndpoint|4xx->4xx|$.message
Issue-Key: osis|CreatePipelineEndpoint|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: osis|CreatePipelineEndpoint|4xx->4xx|status
Issue-Commit: 92643c761956d00ad64a8287d91ba241ce698bd7

# osis CreatePipelineEndpoint returns 404 ResourceNotFoundException instead of 400 ValidationException for unresolvable VpcOptions subnets

> _Note: this issue was identified against the code base at commit `92643c761956d00ad64a8287d91ba241ce698bd7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePipelineEndpoint

## Description
CreatePipelineEndpoint's VPC-validation error path is not implemented: instead of rejecting undescribable subnets with a modeled ValidationException, the emulator fails the pipeline lookup and answers ResourceNotFoundException.

Cause 1 of 1: CreatePipelineEndpoint with VpcOptions naming a subnet that cannot be described answers 404 ResourceNotFoundException "The pipeline could not be found" instead of 400 ValidationException "Unable to describe subnets provided: <subnetId>".

AWS: Smithy (com.amazonaws.osis, CreatePipelineEndpoint): POST /2022-01-01/osis/createPipelineEndpoint, success code 200; required input members PipelineArn (@length 46-76, @pattern ^arn:(aws|aws-cn|aws-us-gov|aws-iso|aws-iso-b):osis:.+:pipeline/.+$) and VpcOptions (PipelineEndpointVpcOptions with SubnetIds/SecurityGroupIds, each @length min 1 max 12); modeled errors include ValidationException (httpError 400, "An exception for missing or invalid input fields") and ResourceNotFoundException (httpError 404, "You attempted to access or delete a resource that does not exist"). The recording shows that for a request whose PipelineArn names an existing pipeline and whose VpcOptions.SubnetIds contain a subnet AWS cannot describe, AWS answers HTTP 400 with x-amzn-errortype: ValidationException and message "Unable to describe subnets provided: subnet-0000000000000000f". The message template is the fixed text "Unable to describe subnets provided: " followed by the offending subnet id(s) taken verbatim from the request's VpcOptions.SubnetIds; only that id is a per-request value, the rest of the text is fixed. The classification rule is the requirement: an unresolvable subnet in VpcOptions is invalid INPUT (ValidationException/400), not a missing resource (ResourceNotFoundException/404).

Emulator: the same step answers HTTP 404 with x-amzn-errortype: ResourceNotFoundException and message "The pipeline could not be found" — the wrong status class, the wrong error code, and a message about a different subject entirely. The emulator never evaluates VpcOptions at all: there is no subnet-resolution check on this path, so the request is turned away by a pipeline lookup instead. Hypothesis: the handler resolves PipelineArn against its pipeline state and returns ResourceNotFoundException when that resolution fails, and either the pipeline created earlier in the fixture is not indexed under the ARN form the request uses or the lookup runs before (and in place of) any VpcOptions validation.

Why: a client that creates a VPC pipeline endpoint cannot distinguish a bad subnet list from a missing pipeline. ValidationException is a non-retryable input error that a caller surfaces to the operator so the subnet ids can be corrected; ResourceNotFoundException drives the opposite remediation — re-create or wait for the pipeline — and SDK/IaC error handling branches on the error code and the 4xx subcode, so provisioning logic loops or aborts on a condition it would otherwise have reported precisely. It also hides any real pipeline-not-found bug, because every malformed-VpcOptions call produces the same 404.

Fix: belongs in the osis CreatePipelineEndpoint handler's validation stage — resolve and validate VpcOptions.SubnetIds (and SecurityGroupIds) before/independently of the PipelineArn state lookup, and raise ValidationException (400, x-amzn-errortype: ValidationException) with the message "Unable to describe subnets provided: " + the unresolvable subnet id(s) from the request. Reserve ResourceNotFoundException for a PipelineArn that genuinely names no pipeline in state, and make sure the pipeline state index resolves the ARN form this operation accepts.

Traces: osis-pipeline-endpoint-error-paths#task#3 (expected 400 ValidationException, got 404 ResourceNotFoundException).

Related open issues (neither covers this path): 20261109T184855Z-osis-createpipeline-accepts-an-unassumable-sts-role-arn.md (CreatePipeline role validation) and 20261021T234042Z-osis-validatepipeline-stub.md (ValidatePipeline stub).

## Evidence
Fixture osis-pipeline-endpoint-error-paths, step task#3, operation CreatePipelineEndpoint.
Expected (AWS recording): HTTP 400; header x-amzn-errortype: ValidationException; body $.message = "Unable to describe subnets provided: subnet-0000000000000000f" (fixed text "Unable to describe subnets provided: " + the subnet id from the request's VpcOptions.SubnetIds).
Actual (emulator): HTTP 404; header x-amzn-errortype: ResourceNotFoundException; body $.message = "The pipeline could not be found".
Replay detail: "status: expected 400, got 404"; "$.message: value_mismatch"; "$headers.x-amzn-errortype: value_mismatch".
Model: ValidationException httpError 400, ResourceNotFoundException httpError 404, both bound to CreatePipelineEndpoint; VpcOptions is a required member of CreatePipelineEndpointRequest.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Replay fixture osis-pipeline-endpoint-error-paths up to step task#3: POST /2022-01-01/osis/createPipelineEndpoint with a PipelineArn naming the pipeline the fixture set up and VpcOptions.SubnetIds containing a subnet id that cannot be described (e.g. subnet-0000000000000000f).
3. Observe HTTP 404 with x-amzn-errortype: ResourceNotFoundException and message "The pipeline could not be found"; AWS returns HTTP 400 with x-amzn-errortype: ValidationException and message "Unable to describe subnets provided: <subnetId>".
