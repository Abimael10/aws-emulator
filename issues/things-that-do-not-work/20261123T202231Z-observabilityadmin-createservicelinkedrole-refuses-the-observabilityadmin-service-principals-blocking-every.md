Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: observabilityadmin.get-telemetry-rule-not-found observabilityadmin.list-resource-telemetry-resource-type-filter-invariant observabilityadmin.telemetry-evaluation-start-stop-lifecycle observabilityadmin.telemetry-rule-create-get-delete-lifecycle
Issue-Shape-Id: com.amazonaws.observabilityadmin#ObservabilityAdmin
Issue-Key: iam|CreateServiceLinkedRole|AccessDenied
Issue-Commit: a08ae734d9d7961d5ba788b615d3d13f86d08737

# CreateServiceLinkedRole refuses the observabilityadmin service principals, blocking every telemetry-evaluation check

> _Note: this issue was identified against the code base at commit `a08ae734d9d7961d5ba788b615d3d13f86d08737`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateServiceLinkedRole

## Description
One net-new root cause.

Cause 1 of 1: IAM's published service-linked-role registry does not carry ObservabilityAdmin's service principals, so CreateServiceLinkedRole refuses a legitimate AWS service principal instead of minting the role. Failing checks: observabilityadmin.get-telemetry-rule-not-found, observabilityadmin.telemetry-evaluation-start-stop-lifecycle, observabilityadmin.telemetry-rule-create-get-delete-lifecycle (principal `telemetry-enablement.observabilityadmin.amazonaws.com`), observabilityadmin.list-resource-telemetry-resource-type-filter-invariant (principal `observabilityadmin.amazonaws.com`).

AWS: `CreateServiceLinkedRole` takes `AWSServiceName`, documented as "The service principal for the Amazon Web Services service to which this role is attached... For example: elasticbeanstalk.amazonaws.com" (Smithy: CreateServiceLinkedRole.AWSServiceName, target groupNameType, required, @length 1..128, @pattern ^[\w+=,.@-]+$). ObservabilityAdmin is a service that uses service-linked roles: `observabilityadmin.amazonaws.com` and `telemetry-enablement.observabilityadmin.amazonaws.com` are real AWS service principals (the latter backs AWSServiceRoleForObservabilityAdmin_TelemetryEnablement, which StartTelemetryEvaluation depends on — the emulator's own observabilityadmin surface models FAILURE reason SLR_UNAVAILABLE for its absence). Against real AWS, CreateServiceLinkedRole for either principal returns HTTP 200 with the created `Role`, and a repeat for the same principal is refused with InvalidInput (the behavioral checks already treat InvalidInput as "already exists"). Note also that this operation models NO AccessDenied/403 at all: its only modeled client errors are InvalidInputException (400, awsQueryError code `InvalidInput`), LimitExceededException (409, `LimitExceeded`) and NoSuchEntityException (404, `NoSuchEntity`), so even for a principal that genuinely names no service, 403 `AccessDenied` is off-contract — AWS words that refusal as `InvalidInput` with the message `<AWSServiceName> is not a valid AWS service name (Service Principal)`, whose only variable part is the `AWSServiceName` value copied from the request.

Emulator: both principals are rejected. Over the wire the call comes back as error code `AccessDenied` with message `telemetry-enablement.observabilityadmin.amazonaws.com is not a valid AWS service name (Service Principal)` (and `observabilityadmin.amazonaws.com is not a valid AWS service name (Service Principal)` for the other principal) — fixed text `is not a valid AWS service name (Service Principal)`, variable part being the principal copied verbatim from the request. The handler reaches that refusal because `crate::state::published_service_linked_role_name(&input.aws_service_name)` returns None for these principals, i.e. the registry behind it has no entry publishing a role name for them; it then answers through the scaffold's 403 `access_denied` channel rather than any modeled error.

Why: a client cannot create (or confirm the existence of) the service-linked role ObservabilityAdmin's onboarding requires, so `StartTelemetryEvaluation` can never be driven to RUNNING on a cold account and nothing downstream of telemetry evaluation is reachable — GetTelemetryRule, CreateTelemetryRule/DeleteTelemetryRule and ListResourceTelemetry are all unreachable in the emulator even though those operations are themselves implemented. The wrong error class compounds it: callers written against AWS branch on `InvalidInput` to mean "the role already exists" and treat `AccessDenied` as a credentials/permissions failure, so retry and idempotency logic takes the wrong branch instead of proceeding.

Fix: IAM's published service-linked-role registry and the refusal path in `CreateServiceLinkedRole`'s handler — register the ObservabilityAdmin principals (with the role names AWS publishes for them, `AWSServiceRoleForObservabilityAdmin` and `AWSServiceRoleForObservabilityAdmin_TelemetryEnablement`) in the table `crate::state::published_service_linked_role_name` consults, and classify an unknown `AWSServiceName` as this operation's modeled `InvalidInputException` (400, `InvalidInput`) instead of the unmodeled 403 `AccessDenied`. Related but distinct, and not a duplicate of this: open issue 20261029T234938Z-iam-deleteservicelinkedrole-answers-404-nosuchentity-for-an-existing.md covers DeleteServiceLinkedRole.

Traces: observabilityadmin.get-telemetry-rule-not-found, observabilityadmin.list-resource-telemetry-resource-type-filter-invariant, observabilityadmin.telemetry-evaluation-start-stop-lifecycle, observabilityadmin.telemetry-rule-create-get-delete-lifecycle. Handler: services/service-com-amazonaws-iam-awsidentitymanagementv20100508/src/ops/create_service_linked_role.rs lines 96-103 (message builder) and 200-216 (registry lookup and the AccessDenied refusal); registry: services/service-com-amazonaws-iam-awsidentitymanagementv20100508/src/state.rs (`PUBLISHED_SERVICE_LINKED_ROLE_NAMES` / `published_service_linked_role_name`).

## Evidence
Expected (real AWS): iam.create_service_linked_role(AWSServiceName='telemetry-enablement.observabilityadmin.amazonaws.com') -> HTTP 200 with Role.RoleName = AWSServiceRoleForObservabilityAdmin_TelemetryEnablement; a second identical call -> HTTP 400, Error.Code 'InvalidInput'. Same for AWSServiceName='observabilityadmin.amazonaws.com'.

Actual (emulator): botocore.exceptions.ClientError: An error occurred (AccessDenied) when calling the CreateServiceLinkedRole operation: telemetry-enablement.observabilityadmin.amazonaws.com is not a valid AWS service name (Service Principal)

and, for the other principal:

botocore.exceptions.ClientError: An error occurred (AccessDenied) when calling the CreateServiceLinkedRole operation: observabilityadmin.amazonaws.com is not a valid AWS service name (Service Principal)

Because the checks' helper only tolerates Error.Code == 'InvalidInput' as "already exists", the AccessDenied refusal propagates and aborts the check before any observabilityadmin assertion runs. Per-service pass rate: observabilityadmin 14/18 (78%), all 4 failures with this one error.

## Steps to Reproduce
1. Boot the emulator.
2. iam.create_service_linked_role(AWSServiceName='telemetry-enablement.observabilityadmin.amazonaws.com') -> observe HTTP 403 / Error.Code 'AccessDenied' with message '<principal> is not a valid AWS service name (Service Principal)'; AWS returns 200 and the created Role.
3. Repeat with AWSServiceName='observabilityadmin.amazonaws.com' -> same refusal.
4. Replay the behavioral checks observabilityadmin.get-telemetry-rule-not-found, observabilityadmin.list-resource-telemetry-resource-type-filter-invariant, observabilityadmin.telemetry-evaluation-start-stop-lifecycle, observabilityadmin.telemetry-rule-create-get-delete-lifecycle: each aborts in its _ensure_telemetry_enablement_slr / SLR-bootstrap helper on that AccessDenied.
