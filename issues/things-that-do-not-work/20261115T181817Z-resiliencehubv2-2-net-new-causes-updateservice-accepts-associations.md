Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: resiliencehubv2.associate-service-rejects-unknown-system resiliencehubv2.deleting-the-associated-service-releases-the-system
Issue-Shape-Id: com.amazonaws.resiliencehubv2#NGRHServiceCore
Issue-Key: resiliencehubv2.associate-service-rejects-unknown-system|_error#7|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: resiliencehubv2.deleting-the-associated-service-releases-the-system|run#11|AssertionError|assert code == "ConflictException", f"the association must block DeleteSystem first, got {code}"
Issue-Commit: 2c275d8e7c049de119f415d052ef726706c4f0b6

# resiliencehubv2: 2 net-new causes — UpdateService accepts associations to non-existent systems, and DeleteSystem is an unimplemented stub answering 501 NotImplemented

> _Note: this issue was identified against the code base at commit `2c275d8e7c049de119f415d052ef726706c4f0b6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateService

## Description
Two distinct root causes in the resiliencehubv2 system-association lifecycle, found by replaying the behavioral suite. Neither is covered by an open issue: 20261110T225437Z-resiliencehubv2-4-net-new-causes-availabilityslo-target-allowed.md covers availabilitySlo/clientToken/duplicate-name causes, and 20261111T074347Z-resiliencehubv2-error-bodies-diverge-from-aws-validation-messages.md covers validation-message wording and clientToken conflicts — neither concerns system-reference validation or DeleteSystem, which has no stub issue filed.

Cause 1 of 2: UpdateService does not resolve the systemArn values in `associatedSystems`, so an association to a system that does not exist is accepted and persisted.
AWS: Smithy: UpdateService input member `associatedSystems` targets AssociatedSystemList (@length max 20) whose entries carry an `Arn` systemArn; the operation models ValidationException (HTTP 400) and ResourceNotFoundException (HTTP 404). The ARN pattern is satisfied by the request, so the reference must be resolved against the account's systems, not merely pattern-checked. Recorded behavior: AWS refuses UpdateService naming a systemArn with no system behind it with HTTP 400, error code `ValidationException`, message `System not found: <systemArn>` — fixed text `System not found: ` followed by the offending systemArn value taken verbatim from the request's associatedSystems entry (never a literal from one run). No association is recorded by the refused call, so a following GetService on the service omits `associatedSystems` entirely, and a subsequent UpdateService naming the real system returns `associatedSystems == [{"systemArn": <real arn>}]`.
Emulator: the call succeeds with HTTP 200 and the dangling reference is written into state and echoed back: `service.associatedSystems == [{'serviceArn-sibling': ...}]` — specifically `[{'systemArn': 'arn:aws:resiliencehub:<region>:<account>:zzzzzz'}]` for an ARN whose resource part names no system. The handler merges the member unconditionally (services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/ops/update_service.rs, `if let Some(associated_systems) = input.associated_systems { record.associated_systems = associated_systems; }`), with no lookup in `store.systems`.
Why: a client that relies on AWS rejecting an unknown system silently records a broken association. Negative-path code never sees the ValidationException it handles, GetService reports a membership that does not exist, and every later read (ListServices by systemArn, deletion ordering, assessment scoping) is computed from a reference to a system that was never created — so a caller's typo or stale ARN becomes corrupt state instead of a 400.
Fix: UpdateService's handler/validation — resolve every `associatedSystems[].systemArn` against the service store's systems before merging, and return the modeled ValidationException (400, `System not found: <systemArn>`) leaving the record untouched when any entry does not resolve. CreateService's equivalent path needs the same gate.
Traces: resiliencehubv2.associate-service-rejects-unknown-system.

Cause 2 of 2: DeleteSystem is still a scaffold stub, so the whole system-deletion lifecycle is unreachable.
AWS: Smithy: DeleteSystem (POST /v2/delete-system) takes required `systemArn` (Arn) and returns required `systemArn` on HTTP 200; it models AccessDeniedException 403, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404, ValidationException 400. Recorded behavior: while a service is still associated with the system, DeleteSystem is refused with HTTP 409, error code `ConflictException`; after the associated service is deleted (which also removes it from ListServices(systemArn=...), leaving `serviceSummaries` empty), DeleteSystem succeeds and echoes the input ARN in `systemArn`, and a following GetSystem is refused with HTTP 404, code `ResourceNotFoundException`, message `System not found: <systemId>` — fixed text `System not found: ` plus the system's own id from state.
Emulator: every DeleteSystem call answers HTTP 501 with error code `NotImplemented`; the check asserting the association-blocked refusal observed code `NotImplemented` instead of `ConflictException`. The handler is the untouched scaffold: services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/ops/delete_system.rs returns `Err(crate::scaffolded::Scaffolded::not_implemented("DeleteSystem"))` and still carries the `SCAFFOLDED, NOT IMPLEMENTED` marker.
Why: a client can create systems but never delete them. Teardown and reconciliation loops get a 501 that is neither retryable nor modeled, the referential guard that protects an in-use system is absent, and the post-delete 404 contract that tells a caller the system is gone can never be observed — resources accumulate for the life of the emulator.
Fix: implement DeleteSystem in its handler file (named above) over the service state — refuse with the modeled ConflictException (409) while any service still lists the system in `associatedSystems`, otherwise remove the system, return its `systemArn`, make GetSystem answer ResourceNotFoundException (404, `System not found: <systemId>`) afterwards, and delete the SCAFFOLDED marker line.
Traces: resiliencehubv2.deleting-the-associated-service-releases-the-system.

## Evidence
Cause 1 — check resiliencehubv2.associate-service-rejects-unknown-system:
  Expected (AWS): UpdateService(serviceArn=<svc>, associatedSystems=[{systemArn: <arn with no system>}]) -> HTTP 400 ValidationException, Message "System not found: <that systemArn>"; GetService(<svc>) then has no `associatedSystems` member; UpdateService with the real system ARN -> associatedSystems == [{systemArn: <real arn>}].
  Actual (emulator): HTTP 200 with the dangling association persisted:
    {'ResponseMetadata': {'HTTPStatusCode': 200, ...}, 'service': {'serviceArn': 'arn:aws:resiliencehub:us-west-2:000000000000:service/99fcdf53-...', 'associatedSystems': [{'systemArn': 'arn:aws:resiliencehub:us-west-2:000000000000:zzzzzz'}], ...}}
  Harness verdict: AssertionError: expected the call to be refused, but it succeeded.

Cause 2 — check resiliencehubv2.deleting-the-associated-service-releases-the-system:
  Expected (AWS): DeleteSystem on a system that still has an associated service -> HTTP 409, Error.Code "ConflictException"; after DeleteService, ListServices(systemArn) -> serviceSummaries == [] and DeleteSystem -> 200 echoing systemArn; GetSystem afterwards -> 404 ResourceNotFoundException "System not found: <systemId>".
  Actual (emulator): Error.Code "NotImplemented" (HTTP 501) from DeleteSystem.
  Harness verdict: AssertionError: the association must block DeleteSystem first, got NotImplemented.
  Source: services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/ops/delete_system.rs:161 returns `Scaffolded::not_implemented("DeleteSystem")`; marker at line 4.

## Steps to Reproduce
Cause 1:
1. iam.CreateRole with a trust policy allowing resiliencehub.amazonaws.com to assume it.
2. resiliencehubv2.CreateSystem(name=<unique>) -> note systemArn.
3. resiliencehubv2.CreateService(name=<unique>, regions=[<region>], permissionModel={invokerRoleName: <role>}) -> note serviceArn.
4. Build a syntactically valid but non-existent system ARN (e.g. replace the last ARN segment with a resource id no system has).
5. resiliencehubv2.UpdateService(serviceArn=<svc>, associatedSystems=[{systemArn: <missing>}]).
   Expected: HTTP 400 ValidationException "System not found: <missing>". Observed: HTTP 200, association stored.
6. resiliencehubv2.GetService(serviceArn=<svc>) — expected: no `associatedSystems` member; observed: the dangling association.

Cause 2:
1. Steps 1-3 above.
2. resiliencehubv2.UpdateService(serviceArn=<svc>, associatedSystems=[{systemArn: <system>}]) and confirm ListServices(systemArn=<system>) lists the service.
3. resiliencehubv2.DeleteSystem(systemArn=<system>).
   Expected: HTTP 409 ConflictException. Observed: HTTP 501 NotImplemented.
4. resiliencehubv2.DeleteService(serviceArn=<svc>), then DeleteSystem(systemArn=<system>) — expected 200 echoing systemArn, then GetSystem -> 404 ResourceNotFoundException "System not found: <systemId>"; observed 501 NotImplemented at every DeleteSystem call.
