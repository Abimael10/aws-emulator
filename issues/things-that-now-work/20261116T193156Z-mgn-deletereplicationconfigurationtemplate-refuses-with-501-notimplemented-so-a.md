Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: mgn.concurrent-idempotent-delete-replication-configuration-template
Issue-Shape-Id: com.amazonaws.mgn#ApplicationMigrationService
Issue-Key: mgn.concurrent-idempotent-delete-replication-configuration-template|run#13|AssertionError|assert outcomes.count("ok") >= 1, f"at least one concurrent delete must succeed, got {outcomes}"
Issue-Commit: 95ee4d05f76278f15dd45811b926fa4843973dc6

# mgn DeleteReplicationConfigurationTemplate refuses with 501 NotImplemented, so a concurrent delete race never resolves to idempotent success or ResourceNotFoundException

> _Note: this issue was identified against the code base at commit `95ee4d05f76278f15dd45811b926fa4843973dc6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteReplicationConfigurationTemplate

## Description
Cause 1 of 1: mgn `DeleteReplicationConfigurationTemplate` never deletes a replication configuration template, so neither the winner nor the loser of two racing deletes gets a modeled outcome, and a deleted template is never removed from the describe view.

Note on related issues: open issue `20261116T153206Z-mgn-deletereplicationconfigurationtemplate-stub.md` records the same operation as a scaffold stub, but its `Issue-Trace-Ids` are only `mgn.delete-nonexistent-replication-configuration-template-rejected` and `mgn.replication-configuration-template-delete-removes-from-list`; it does not carry this check's replay key and says nothing about the concurrent/idempotent-delete contract below, so this failure is not covered by it. Fixing that stub is a precondition for this issue, not a discharge of it.

AWS: `DeleteReplicationConfigurationTemplate` (Smithy: input `replicationConfigurationTemplateID`, required, @length min 21 max 21, pattern `^rct-[0-9a-zA-Z]{17}$`; http POST /DeleteReplicationConfigurationTemplate with success code 204 and an empty output shape; marked @idempotent; modeled errors ConflictException 409, ResourceNotFoundException 404, UninitializedAccountException 400). Because the operation is idempotent and the only modeled not-found failure is ResourceNotFoundException, two concurrent deletes of the same template resolve in exactly one of two ways: both return 204 with an empty body, or one returns 204 and the loser returns HTTP 404 `ResourceNotFoundException`. No other error code is permitted for that race. The loser's message is, as AWS words it for this error class, the fixed text `Replication Configuration Template with Id: '` + the request's `replicationConfigurationTemplateID` + `' was not found` — only the quoted id is a value taken from the request; the rest is fixed text. After the race resolves, `DescribeReplicationConfigurationTemplates` with `replicationConfigurationTemplateIDs=[<that id>]` answers HTTP 200 with `items: []` (Smithy: output `items` is a list and `ResourceNotFoundException` is modeled but is not used for a filter that matches nothing), not an error.

Emulator: every call to `DeleteReplicationConfigurationTemplate` is refused with error code `NotImplemented` and message `operation DeleteReplicationConfigurationTemplate is scaffolded but not implemented in this emulator` (HTTP 501). In the replay both racing deletes returned `NotImplemented`, so the outcome set was `['NotImplemented', 'NotImplemented']`: zero deletes succeeded, no loser ever saw `ResourceNotFoundException`, no not-found message was produced, and the template remains visible to `DescribeReplicationConfigurationTemplates` afterwards.

Why: a client that deletes a replication configuration template — including the retry a SDK performs on an @idempotent operation, and any teardown path — gets a 501 with an unmodeled error code its error handling does not recognise, so the delete is treated as a transport/server fault rather than as success or as an already-deleted resource. Cleanup loops that swallow only `ResourceNotFoundException` fail; templates accumulate and the next create then trips the per-account template quota, cascading into unrelated failures.

Fix: implement the operation's handler, `services/service-com-amazonaws-mgn-applicationmigrationservice/src/ops/delete_replication_configuration_template.rs`, replacing the scaffold refusal with a real handler over the service state: remove the named template under a single atomic state mutation so that concurrent callers serialise, answer 204 with an empty body for the caller that removed it, and answer the modeled `ResourceNotFoundException` (404, message as worded above) for a caller whose id is absent. Delete that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line when, and only when, the handler is real. The describe side belongs in the `DescribeReplicationConfigurationTemplates` handler/state read: an id filter that matches nothing must yield `items: []` with HTTP 200 rather than an error.

Traces: mgn.concurrent-idempotent-delete-replication-configuration-template.

## Evidence
Check `mgn.concurrent-idempotent-delete-replication-configuration-template` creates a template, then fires two `DeleteReplicationConfigurationTemplate` calls from a 2-worker ThreadPoolExecutor.

Expected (AWS): outcomes drawn only from {204 success, HTTP 404 `ResourceNotFoundException`}, with at least one success; a losing caller's message is `Replication Configuration Template with Id: '<replicationConfigurationTemplateID>' was not found` (fixed text, with the id taken from the request); afterwards `DescribeReplicationConfigurationTemplates(replicationConfigurationTemplateIDs=[<id>])` returns 200 with `items: []`.

Actual (emulator): both calls were refused with HTTP 501, error code `NotImplemented`, message `operation DeleteReplicationConfigurationTemplate is scaffolded but not implemented in this emulator`. Assertion raised:
  AssertionError: at least one concurrent delete must succeed, got ['NotImplemented', 'NotImplemented']
  (check_mgn_concurrent_idempotent_delete_replication_configuration_template.py, line 57)
No loser message was produced and the template was never removed from state, so the subsequent empty-items describe assertion is unreachable.

Smithy cited: DeleteReplicationConfigurationTemplate — @idempotent; http {method: POST, uri: /DeleteReplicationConfigurationTemplate, code: 204}; errors ConflictException(409), ResourceNotFoundException(404), UninitializedAccountException(400); input replicationConfigurationTemplateID required, @length 21/21, pattern ^rct-[0-9a-zA-Z]{17}$. DescribeReplicationConfigurationTemplates — http code 200, output member `items` (list).

## Steps to Reproduce
1. Boot the mgn emulator and call InitializeService.
2. Call CreateReplicationConfigurationTemplate with a valid staging-area subnet to obtain a replicationConfigurationTemplateID (rct-...).
3. From two threads, call DeleteReplicationConfigurationTemplate concurrently with that same replicationConfigurationTemplateID.
4. Observe both calls return HTTP 501 with error code NotImplemented, instead of one (or both) returning 204 and any loser returning HTTP 404 ResourceNotFoundException with message "Replication Configuration Template with Id: '<id>' was not found".
5. Call DescribeReplicationConfigurationTemplates with replicationConfigurationTemplateIDs=[<that id>] and observe the template is still listed instead of items: [].
