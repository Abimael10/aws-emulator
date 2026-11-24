Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: resiliencehubv2.function-resources-rejected-for-undiscovered-resource
Issue-Shape-Id: com.amazonaws.resiliencehubv2#NGRHServiceCore
Issue-Key: resiliencehubv2.function-resources-rejected-for-undiscovered-resource|run#7|AssertionError|assert (code, message) == ("ValidationException", (
Issue-Commit: f667faacbc6ecad8870388d49cf0b2c06890cb64

# resiliencehubv2 CreateServiceFunctionResources: undiscovered-resource ValidationException message is reworded

> _Note: this issue was identified against the code base at commit `f667faacbc6ecad8870388d49cf0b2c06890cb64`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateServiceFunctionResources

## Description
Behavioral replay of simparity-behavioral-aws-v1 against the emulator built from this checkout found ONE net-new root cause on the resiliencehubv2 CreateServiceFunctionResources refusal path. This surface is adjacent to the open issue "resiliencehubv2 error bodies diverge from AWS: validation messages reworded and clientToken conflict never raised" (20261111T074347Z-resiliencehubv2-error-bodies-diverge-from-aws-validation-messages.md), but that issue is bound to CreateAssertion / CreateInputSource / CreatePolicy wire keys only and carries neither this operation nor this check id, so nothing in it re-replays this check.

Cause 1 of 1: CreateServiceFunctionResources refuses a resource the service never discovered with the WRONG ValidationException message.
AWS: Smithy CreateServiceFunctionResources (POST /v2/create-service-function-resources, success 200) declares input members serviceArn -> Arn (required, @length min 31, ARN pattern), serviceFunctionId -> EntityId (required, @length 1..255, pattern ^\S{1,255}$) and resources -> ResourceList (required, @length 1..100), and models the errors AccessDeniedException 403, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404, ValidationException 400 (ValidationException: @error client, httpError 400, member `message` String required, plus `reason` and `fieldList`). The HTTP status (400) and error code (ValidationException) are modeled; the refusal TEXT is recorded behavior only: AWS answers with the message `Resource not found: <resource>. Resource must be discovered before it can be added to a function.` The FIXED text of that message is `Resource not found: ` and `. Resource must be discovered before it can be added to a function.`; the single VARIABLE part is the resource identifier taken verbatim from the request's `resources` entry that could not be resolved (in the replayed check an S3 ARN of the form arn:aws:s3:::<generated-bucket-name> — a per-run value that only fills the template and is not part of it).
Emulator: status 400 and code ValidationException are correct, but the body's `message` is the emulator's own wording `Resource not discovered for service: <resource>` — entirely different fixed text. The identifier is interpolated correctly; only the surrounding sentence diverges. The check asserts on (Error.Code, Error.Message) and fails on the message alone, so the remaining assertions of the check (serviceFunction.resourceCount staying 0 and ListResources returning an empty serviceResources for the function) were never reached and their fidelity is unverified by this replay.
Why: a client, IaC provider or test suite that keys on AWS's refusal text to distinguish "this resource has not been discovered yet, run/await discovery and retry" from any other ValidationException does not recognise the refusal and surfaces it as an unknown validation failure; retry-on-undiscovered logic never fires. The emulator's wording also drops AWS's "Resource not found" lead-in, which is the part clients match on.
Fix: the CreateServiceFunctionResources handler's undiscovered-resource refusal branch (the helper that builds the ValidationException for an identifier `discovered_resources::discover` did not resolve) — word the message exactly as AWS does, keeping the request's identifier as the only interpolated value. Nothing else in the operation changes; the status, code and `reason` are already right.
Traces: behavioral check resiliencehubv2.function-resources-rejected-for-undiscovered-resource. Code citation: services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/ops/create_service_function_resources.rs lines 274-280 (`undiscovered_resource`, message built at line 276), reached from the `first_undiscovered` branch at lines 194-196.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout (41 checks, 13 passed, bootstrap_signal=false — the emulator is up and answering; resiliencehubv2 11/35 passed). This failure is reported with verdict [mismatch] and is marked neither ALREADY FILED nor OWNED by any open issue.

Check resiliencehubv2.function-resources-rejected-for-undiscovered-resource, at check_resiliencehubv2_function_resources_rejected_for_undiscovered_resource.py line 107:

  assert (code, message) == ("ValidationException", f"Resource not found: {resource}. Resource must be discovered before it can be added to a function.")
  AssertionError: undiscovered resource: ValidationException: 'Resource not discovered for service: arn:aws:s3:::sim2real-behav-rhv2-undiscovered-341c38ddd72f'

  expected Error.Code:    ValidationException   (matches)
  expected Error.Message: 'Resource not found: <resource>. Resource must be discovered before it can be added to a function.'
  actual   Error.Message: 'Resource not discovered for service: <resource>'

The bucket name sim2real-behav-rhv2-undiscovered-341c38ddd72f in the replayed run is a per-run generated value filling <resource>; the requirement is the fixed sentence plus the request's own identifier.

Handler evidence (services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/ops/create_service_function_resources.rs):
  194  if let Some(undiscovered) = discovery.first_undiscovered(&input.resources) {
  195      return Err(undiscovered_resource(undiscovered).into());
  276      message: format!("Resource not discovered for service: {identifier}"),

Model evidence (services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/smithy-model/com-amazonaws-resiliencehubv2-ngrhservicecore.json, service NGRHServiceCore, sdkId resiliencehubv2, protocol restJson1): CreateServiceFunctionResources POST /v2/create-service-function-resources, success 200; errors AccessDeniedException 403, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404, ValidationException 400.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from this checkout.
2. Create an IAM role resiliencehub.amazonaws.com may assume, then CreateService with regions=[<region>] and permissionModel={invokerRoleName: <role>}; take its serviceArn.
3. CreateServiceFunction(serviceArn=<arn>, name="ledger", criticality="PRIMARY"); take its serviceFunctionId.
4. Call CreateServiceFunctionResources(serviceArn=<arn>, serviceFunctionId=<id>, resources=["arn:aws:s3:::<a-bucket-the-service-never-discovered>"]).
5. Observe HTTP 400 / ValidationException with message "Resource not discovered for service: arn:aws:s3:::<bucket>" instead of AWS's "Resource not found: arn:aws:s3:::<bucket>. Resource must be discovered before it can be added to a function."
6. Re-replay the behavioral check resiliencehubv2.function-resources-rejected-for-undiscovered-resource to confirm the fix; note it continues past the message assertion to require that the function's resourceCount stays 0 and that ListResources(serviceArn, serviceFunctionId, billable=False) returns that serviceFunctionId with an empty serviceResources list.
