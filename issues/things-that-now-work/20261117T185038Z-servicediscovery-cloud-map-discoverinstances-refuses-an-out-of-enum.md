Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: servicediscovery.discover-instances-invalid-health-status-rejected
Issue-Shape-Id: com.amazonaws.servicediscovery#Route53AutoNaming_v20170314
Issue-Key: servicediscovery.discover-instances-invalid-health-status-rejected|run#14|AssertionError|assert code == "ValidationException", f"expected ValidationException, got {code!r}"
Issue-Commit: 5f72b15ceea68fb83a63d5bc988a846400d3d8ef

# Cloud Map DiscoverInstances refuses an out-of-enum HealthStatus with the modeled InvalidInput error instead of the framework ValidationException

> _Note: this issue was identified against the code base at commit `5f72b15ceea68fb83a63d5bc988a846400d3d8ef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DiscoverInstances

## Description
Cause 1 of 1: DiscoverInstances does not enforce the HealthStatusFilter enum constraint through the framework's input-validation channel, so an out-of-enum HealthStatus value comes back under this service's own modeled InvalidInput error code rather than AWS's standard ValidationException. Failing check: servicediscovery.discover-instances-invalid-health-status-rejected.

AWS: Smithy (services/service-com-amazonaws-servicediscovery-route53autonaming-v20170314/smithy-model/com-amazonaws-servicediscovery-route53autonaming-v20170314.json) models DiscoverInstancesRequest$HealthStatus with target com.amazonaws.servicediscovery#HealthStatusFilter, an enum whose members are HEALTHY, UNHEALTHY, ALL, HEALTHY_OR_ELSE_ALL; DiscoverInstances models only InvalidInput (400), NamespaceNotFound (404), RequestLimitExceeded (429) and ServiceNotFound (404) — a value outside a modeled enum is NOT one of those, it is refused by AWS's generic request-validation layer with HTTP 400 and error code ValidationException. The message is the standard AWS constraint-violation text, whose FIXED parts are '1 validation error detected: Value ', " at '", "' failed to satisfy constraint: Member must satisfy enum value set: " and the bracketed value set; its VARIABLE parts come from the request: the rejected value (quoted) and the member path (here the lowerCamel wire name 'healthStatus'). The value set AWS renders for HealthStatusFilter is, in order, [ALL, UNHEALTHY, HEALTHY_OR_ELSE_ALL, HEALTHY] — recorded behavior only: the model declares the members in a different order (HEALTHY, UNHEALTHY, ALL, HEALTHY_OR_ELSE_ALL) and says nothing about the order used in the message, so the rendered order is a per-enum property of AWS's message (the same general rule already recorded for RecordType in open issue 20261117T054412Z-servicediscovery-cloud-map-createservice-validation-message-lists-the-recordtype.md, which covers CreateService.DnsConfig only and does NOT cover this operation or this member).

Emulator: the call is refused, but with error code 'InvalidInput' — the service's own modeled 400 — instead of 'ValidationException'. The check asserts the code first and stops there, so the body text of the emulator's refusal is not in the replay output; the handler (services/service-com-amazonaws-servicediscovery-route53autonaming-v20170314/src/ops/discover_instances.rs, the HealthStatus match arm commented "a value outside the modelled HealthStatusFilter enum is refused before this handler runs") assumes the framework rejects it, and the same framework does emit a proper ValidationException for CreateService body members, so the enum constraint on this input member is either not wired into validation or its violation is being mapped onto the operation's modeled InvalidInput.

Why: a client that distinguishes AWS's generic input-validation failure (ValidationException — a malformed request, never retried, surfaced to the caller as a programming error) from Cloud Map's own InvalidInput (a service-semantics refusal) branches the wrong way; SDK error-shape matching on ValidationException does not fire, and negative tests that assert AWS's documented refusal for a bad enum value pass against the emulator and fail against AWS. Any caller that parses the bracketed value set to show the allowed HealthStatus values gets nothing at all here.

Fix: the input-validation path for DiscoverInstances — the enum constraint on DiscoverInstancesRequest$HealthStatus must be applied by the same framework validation that produces ValidationException for other members, with the standard message above, and the HealthStatusFilter value set rendered in AWS's order rather than the model's declaration order. It does not belong in the handler's filter logic, which correctly assumes the value is already constrained.

Traces: servicediscovery.discover-instances-invalid-health-status-rejected.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout (87/101 passed; servicediscovery 67/77).

[mismatch] servicediscovery.discover-instances-invalid-health-status-rejected, check_servicediscovery_discover_instances_invalid_health_status_rejected.py line 80:
  assert code == "ValidationException", f"expected ValidationException, got {code!r}"
  AssertionError: expected ValidationException, got 'InvalidInput'

The check creates an HTTP namespace and a service in it, then calls sd.discover_instances(NamespaceName=<ns>, ServiceName=<svc>, HealthStatus='BOGUS') and expects:
  code    == 'ValidationException'
  message == "1 validation error detected: Value 'BOGUS' at 'healthStatus' failed to satisfy constraint: Member must satisfy enum value set: [ALL, UNHEALTHY, HEALTHY_OR_ELSE_ALL, HEALTHY]"
(the quoted value 'BOGUS' and the member path are the request's contribution to that template, not part of it). The emulator refuses the call with code 'InvalidInput'; the message assertion is never reached because the code assertion fails first.

The check's own positive control — the identical call with HealthStatus='ALL' returning Instances == [] — is not reported failing, so only the rejection class diverges.

Model support: smithy_query(operation='DiscoverInstances') shows HealthStatus -> HealthStatusFilter (type enum) and errors [InvalidInput 400, NamespaceNotFound 404, RequestLimitExceeded 429, ServiceNotFound 404] — no ValidationException is modeled, confirming it is the framework-level refusal. smithy_query(shape='HealthStatusFilter') -> values [HEALTHY, UNHEALTHY, ALL, HEALTHY_OR_ELSE_ALL], i.e. a different order from the one AWS renders in the message.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the behavioral suite (run_behavioral_suite), or drive the endpoint with botocore directly.
2. sd = boto3 client('servicediscovery') against the emulator; op = sd.create_http_namespace(Name='<unique>')['OperationId']; poll sd.get_operation(OperationId=op) until Status == 'SUCCESS' and take Targets['NAMESPACE'].
3. svc = sd.create_service(Name='<unique>', NamespaceId=<namespace id>)['Service']['Id'].
4. Call sd.discover_instances(NamespaceName='<the namespace name>', ServiceName='<the service name>', HealthStatus='BOGUS').
5. Inspect the ClientError: the emulator reports Error['Code'] == 'InvalidInput'; AWS reports HTTP 400 with Error['Code'] == 'ValidationException' and Error['Message'] == "1 validation error detected: Value 'BOGUS' at 'healthStatus' failed to satisfy constraint: Member must satisfy enum value set: [ALL, UNHEALTHY, HEALTHY_OR_ELSE_ALL, HEALTHY]".
6. Confirm the positive control still works: sd.discover_instances(NamespaceName=..., ServiceName=..., HealthStatus='ALL') returns 200 with Instances == [].
7. Re-replay servicediscovery.discover-instances-invalid-health-status-rejected to confirm the fix.
