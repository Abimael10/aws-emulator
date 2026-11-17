Issue-Source: trace-sweep-divergence
Issue-Trace: cfnarch-stale-create-complete-after-backing-record-deleted
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cfnarch-stale-create-complete-after-backing-record-deleted
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: events|DeleteArchive|2xx->4xx|$
Issue-Key: events|DeleteArchive|2xx->4xx|status
Issue-Commit: 0d074a71eaca712c4eb2c65274b3985903118640

# EventBridge DeleteArchive refuses a CloudFormation-created archive as nonexistent

> _Note: this issue was identified against the code base at commit `0d074a71eaca712c4eb2c65274b3985903118640`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteArchive

## Description
This issue carries 1 CAUSE.

=== Cause 1 of 1: DeleteArchive answers ResourceNotFoundException for an archive that exists ===
Fixture: cfnarch-stale-create-complete-after-backing-record-deleted, step task#4.
AWS: Smithy (com.amazonaws.eventbridge#DeleteArchive) models DeleteArchive with a single required input member ArchiveName (@length 1..48, @pattern ^[\.\-_A-Za-z0-9]+$) and an EMPTY output shape; its modeled errors are ConcurrentModificationException (client, 400), InternalException (server, 500) and ResourceNotFoundException (client, 400). Deleting an archive that exists therefore answers HTTP 200 with an empty body, and ResourceNotFoundException is reserved for an ArchiveName no archive in the account/region carries. An archive provisioned by a stack resource (AWS::Events::Archive) is an ordinary EventBridge archive and is addressable by the EventBridge API like any other, so DeleteArchive on it must succeed. The ResourceNotFoundException message's fixed text is 'Archive ' + ' does not exist.' with the ArchiveName taken from the request — correct wording, but wrong to emit here at all.
Emulator: HTTP 400 with body {'__type': 'ResourceNotFoundException', 'message': 'Archive <archiveName> does not exist.'} where HTTP 200 and an empty body are expected. The archive had just been reported as provisioned by the stack, so the emulator's EventBridge archive store does not contain the archive the CloudFormation resource claims to have created (equivalently, the stack-created archive is registered somewhere the events data plane does not read).
Why: the archive is unreachable through the EventBridge API that owns it. A client cannot delete, describe, disable or replay a stack-provisioned archive, and out-of-band cleanup and drift workflows that delete the backing resource and then re-read the stack cannot run at all — every such call is refused as if the resource were never created, while CloudFormation simultaneously reports it present. Callers that treat ResourceNotFoundException as idempotent success will also silently skip real cleanup.
Fix: the archive lookup in the events state used by DeleteArchive (and the archive-creation path that the AWS::Events::Archive resource provider drives) — the stack-provisioned archive must be inserted into the same account/region-scoped archive store DeleteArchive resolves ArchiveName against, so the API-visible archive set and the stack's resource set agree. Related but distinct open issue: 20261117T115655Z-cloudformation-events-a-stack-reaches-create-complete-without.md covers AWS::Events::Connection / AWS::Events::ApiDestination not being materialized; it does not cover AWS::Events::Archive or the DeleteArchive refusal, so this is net-new.
Traces: cfnarch-stale-create-complete-after-backing-record-deleted#task#4.

## Evidence
Fixture cfnarch-stale-create-complete-after-backing-record-deleted#task#4, service 'events', operation DeleteArchive:
  status: expected 200, got 400
  body: value_mismatch (expected {} — the modeled empty DeleteArchiveResponse; got {'__type': 'ResourceNotFoundException', 'message': 'Archive sim2real-probe-cfnarch-arc-0c7a4fa8 does not exist.'})
  verdict mismatch, affected_count 1, stub_path empty, owned_by empty (no open issue carries this replay key).

The archive name in the recorded message is a per-run generated name and is not itself a requirement; the requirement is HTTP 200 with an empty body for an archive that exists, and ResourceNotFoundException only for an ArchiveName that names no archive.

Model citation: Smithy DeleteArchive — input {ArchiveName: required, @length 1..48, @pattern ^[\.\-_A-Za-z0-9]+$}, output {} (empty), errors {ConcurrentModificationException 400, InternalException 500, ResourceNotFoundException 400}.

## Steps to Reproduce
Boot the emulator from this checkout and replay the simparity-wire-aws-v1 corpus (replay_wire_corpus), then read fixture 'cfnarch-stale-create-complete-after-backing-record-deleted' step task#4.

Equivalently, by hand:
1. CreateStack with a template containing an AWS::Events::Archive resource; wait for CREATE_COMPLETE.
2. Read the archive's name (the resource's PhysicalResourceId / Ref).
3. Call EventBridge DeleteArchive with that ArchiveName.
Expect HTTP 200 with an empty body. Observe HTTP 400 {'__type':'ResourceNotFoundException','message':'Archive <name> does not exist.'}.
(Calling DescribeArchive / ListArchives for the same name shows the same store gap.)
