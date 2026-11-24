Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.capacity-provider-reserved-name-update-and-delete-rejected ecs.update-capacity-provider-after-delete-fails
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.capacity-provider-reserved-name-update-and-delete-rejected|run#5|AssertionError|assert error == _RESERVED, f"UpdateCapacityProvider(FARGATE): expected {_RESERVED!r}, got {error!r}"
Issue-Key: ecs|UpdateCapacityProvider|ClientException
Issue-Commit: 2d1d2911b5095265487653cd392f1f2d36a8c5ca

# ecs: 2 net-new causes — UpdateCapacityProvider reports the reserved built-in providers (FARGATE/FARGATE_SPOT) and a deleted (INACTIVE) provider as "does not exist" instead of AWS's reserved / has-been-deleted refusals

> _Note: this issue was identified against the code base at commit `2d1d2911b5095265487653cd392f1f2d36a8c5ca`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateCapacityProvider

## Description
Two independent root causes in the ECS capacity-provider refusal surface, both grounded in failing checks the suite lists under `### ecs` and neither marked ALREADY FILED nor OWNED by an open issue. Both must be closed before submitting. No open issue covers them: the capacity-provider issues on file are about other surfaces — 20261108T150857Z-ecs-2-net-new-causes-updatecapacityprovider-is-a.md describes UpdateCapacityProvider as a 501 NotImplemented scaffold stub (it is now implemented and answers a modeled ClientException, so that description no longer matches what is observed here), 20261104T062603Z-ecs-runtask-clienttoken-is-ignored-capacity-provider-base.md is about RunTask base/weight, and 20261116T093306Z / 20261116T055542Z are bedrock-agentcore-control, a different service.

Cause 1 of 2: the reserved, Amazon-ECS-owned capacity providers are not recognised as reserved by UpdateCapacityProvider, so modifying one is refused as a name that does not exist.
  AWS: FARGATE and FARGATE_SPOT exist in every account (DescribeCapacityProviders reports them ACTIVE) but are reserved: UpdateCapacityProvider and DeleteCapacityProvider both refuse them with HTTP 400, error code `ClientException`, message `The specified capacity provider is reserved and cannot be modified by this operation. Specify a valid name or ARN and try again.` That whole sentence is AWS's fixed text for this error class — it carries no value from the request or from state (the provider name does not appear in it). Smithy (com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113): UpdateCapacityProvider models ClientException (HTTP 400, "might be specifying an identifier that isn't valid") and InvalidParameterException (HTTP 400) as separate errors; DeleteCapacityProvider's documentation states the FARGATE and FARGATE_SPOT capacity providers are reserved and can't be deleted. The reserved-refusal wording itself is recorded behavior (the check asserts it verbatim for both operations).
  Emulator: UpdateCapacityProvider(name=FARGATE) answers `ClientException` with `The specified capacity provider does not exist. Specify a valid name or ARN and try again.` — the built-in providers are resolved rather than stored, so the handler's store lookup finds nothing and falls through to its missing-provider branch; there is no reserved-name guard ahead of it. DescribeCapacityProviders(["FARGATE"]) in the same check does report it ACTIVE, so the same name is simultaneously describable and "nonexistent". The check's DeleteCapacityProvider(FARGATE) assertion was never reached (it fails fast on the update), but the delete handler's reserved branch answers a different code and sentence than the one required here — `InvalidParameterException` with its own reserved-delete message — so the delete half of this cause is very likely divergent too and must be replayed, not assumed: AWS answers `ClientException` with the reserved-and-cannot-be-modified sentence above for both operations.
  Why: a client cannot tell "this is a built-in provider you may not modify" from "you mistyped the name". Code that reacts to a does-not-exist refusal by creating the provider will attempt to create FARGATE (which AWS refuses on its own grounds), and an operator reading the message concludes the account is missing its built-in providers while Describe says otherwise. On the delete path the error CODE also differs, so a client branching on ClientException vs InvalidParameterException takes the wrong branch.
  Fix: the reserved-name guard belongs in UpdateCapacityProvider's handler, ahead of its store lookup, and DeleteCapacityProvider's existing reserved branch needs its error class and message aligned with AWS's (file and line citations under Traces).
  Traces: ecs.capacity-provider-reserved-name-update-and-delete-rejected.

Cause 2 of 2: a deleted (INACTIVE) capacity provider is reported as a name that does not exist, instead of as a provider that has been deleted.
  AWS: after DeleteCapacityProvider the provider becomes INACTIVE and stays describable; a later (stale/raced) UpdateCapacityProvider naming it is refused with HTTP 400, error code `ClientException`, message `The capacity provider could not be updated because it has been deleted.` The whole sentence is fixed text for this error class — no request or state value appears in it. Smithy: UpdateCapacityProvider models ClientException (HTTP 400) for an identifier it cannot act on, and its output is the updated `capacityProvider`; the distinction between a deleted provider and an unresolvable name is recorded behavior (the check asserts the sentence verbatim) and the model does not spell it out.
  Emulator: the same `ClientException` is returned but with `The specified capacity provider does not exist. Specify a valid name or ARN and try again.` — the handler's lookup filters any record whose status is INACTIVE out of the store and then reports the generic missing-provider sentence, collapsing "deleted tombstone" and "never existed" into one refusal. The positive controls around it pass: the provider is created, reaches ACTIVE, is successfully updated to managedScaling ENABLED, is deleted, and is observed INACTIVE — only the post-delete update's message diverges.
  Why: a client reconciling a concurrent delete against an in-flight update cannot distinguish a provider it deleted (stop, do not retry, re-create under a new name) from a bad identifier (fix the name and retry), so idempotency and recovery logic keyed on this message takes the wrong path.
  Fix: the provider-resolution branch of UpdateCapacityProvider's handler — resolve an INACTIVE record as a tombstone and refuse it with the has-been-deleted message, keeping the missing-provider message for a name that resolves to nothing at all (file and line citations under Traces).
  Traces: ecs.update-capacity-provider-after-delete-fails.

Source locations (for reference, not a patch): services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/ops/update_capacity_provider.rs lines 304-315 (the `capacity_providers.get_mut(&name).filter(status != CAPACITY_PROVIDER_INACTIVE)` lookup that emits `crate::state::CAPACITY_PROVIDER_MISSING_MESSAGE` for both causes — it has no reserved-name guard and no tombstone branch) and services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/ops/delete_capacity_provider.rs lines 231-247 (the reserved branch returning InvalidParameterException with RESERVED_CAPACITY_PROVIDER_DELETE_MESSAGE).

## Evidence
Cause 1 — expected (AWS): `aws ecs update-capacity-provider --name FARGATE --auto-scaling-group-provider '{"managedScaling":{"status":"DISABLED"}}'` -> HTTP 400, `Error.Code = ClientException`, `Error.Message = "The specified capacity provider is reserved and cannot be modified by this operation. Specify a valid name or ARN and try again."` (entire message is fixed text; no per-run value in it). Actual (emulator): HTTP 400, `Error.Code = ClientException`, `Error.Message = "The specified capacity provider does not exist. Specify a valid name or ARN and try again."`
  File "check_ecs_capacity_provider_reserved_name_update_and_delete_rejected.py", line 48, in run
    assert error == _RESERVED, f"UpdateCapacityProvider(FARGATE): expected {_RESERVED!r}, got {error!r}"
  AssertionError: UpdateCapacityProvider(FARGATE): expected ('ClientException', 'The specified capacity provider is reserved and cannot be modified by this operation. Specify a valid name or ARN and try again.'), got ('ClientException', 'The specified capacity provider does not exist. Specify a valid name or ARN and try again.')
The check's positive control passes immediately before this: `describe_capacity_providers(capacityProviders=["FARGATE"])` returns name FARGATE, status ACTIVE. The check's next assertion — `delete_capacity_provider(capacityProvider="FARGATE")` must also answer ('ClientException', 'The specified capacity provider is reserved and cannot be modified by this operation. Specify a valid name or ARN and try again.') — was not reached because the run stops at the update assertion; the delete handler currently answers InvalidParameterException with a different sentence, so replay it after fixing the update.

Cause 2 — expected (AWS): create an ASG-backed capacity provider, update it (succeeds, managedScaling ENABLED), `aws ecs delete-capacity-provider --capacity-provider <p>` (provider becomes INACTIVE, still describable), then `aws ecs update-capacity-provider --name <p> --auto-scaling-group-provider '{"managedScaling":{"status":"DISABLED"}}'` -> HTTP 400, `Error.Code = ClientException`, `Error.Message = "The capacity provider could not be updated because it has been deleted."` (entire message is fixed text; the provider name is a per-run value and does not appear in it), and the provider stays INACTIVE with managedScaling still ENABLED from the last successful update. Actual (emulator): HTTP 400, `Error.Code = ClientException`, `Error.Message = "The specified capacity provider does not exist. Specify a valid name or ARN and try again."`
  File "check_ecs_update_capacity_provider_after_delete_fails.py", line 151, in run
    assert error == expected, ...
  AssertionError: expected ('ClientException', 'The capacity provider could not be updated because it has been deleted.') for a stale update on a deleted capacity provider, got ('ClientException', 'The specified capacity provider does not exist. Specify a valid name or ARN and try again.')
Everything before that assertion passes (create -> ACTIVE, positive-control update -> managedScaling ENABLED, delete -> INACTIVE), so the divergence is confined to the refusal of the post-delete update.

## Steps to Reproduce
Cause 1:
1. `aws ecs describe-capacity-providers --capacity-providers FARGATE` -> 200, name FARGATE, status ACTIVE (positive control; passes today).
2. `aws ecs update-capacity-provider --name FARGATE --auto-scaling-group-provider '{"managedScaling":{"status":"DISABLED"}}'`
   -> emulator: ClientException / "The specified capacity provider does not exist. Specify a valid name or ARN and try again."
   -> AWS: ClientException / "The specified capacity provider is reserved and cannot be modified by this operation. Specify a valid name or ARN and try again."
3. `aws ecs delete-capacity-provider --capacity-provider FARGATE` -> AWS: the same ClientException and the same reserved sentence; the emulator answers InvalidParameterException with a different sentence.
4. Re-describe FARGATE: it must still be ACTIVE and unchanged (holds in both).
Replay: ecs.capacity-provider-reserved-name-update-and-delete-rejected.

Cause 2:
1. Create a launch template and a zero-capacity Auto Scaling group, then `aws ecs create-capacity-provider --name <p> --auto-scaling-group-provider '{"autoScalingGroupArn":"<asg-arn>","managedScaling":{"status":"DISABLED"},"managedTerminationProtection":"DISABLED"}'`; poll describe-capacity-providers until ACTIVE.
2. `aws ecs update-capacity-provider --name <p> --auto-scaling-group-provider '{"managedScaling":{"status":"ENABLED","targetCapacity":100,"minimumScalingStepSize":1,"maximumScalingStepSize":1}}'` -> 200; describe shows managedScaling ENABLED (positive control).
3. `aws ecs delete-capacity-provider --capacity-provider <p>`; poll describe-capacity-providers until status INACTIVE.
4. `aws ecs update-capacity-provider --name <p> --auto-scaling-group-provider '{"managedScaling":{"status":"DISABLED"}}'`
   -> emulator: ClientException / "The specified capacity provider does not exist. Specify a valid name or ARN and try again."
   -> AWS: ClientException / "The capacity provider could not be updated because it has been deleted."
5. Re-describe <p>: status INACTIVE and managedScaling still ENABLED (the rejected update must not leak into state).
Replay: ecs.update-capacity-provider-after-delete-fails.
