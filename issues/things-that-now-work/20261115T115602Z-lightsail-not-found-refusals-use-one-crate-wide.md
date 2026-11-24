Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lightsail.keypair-delete-fingerprint-mismatch-rejected lightsail.keypair-duplicate-name-rejected lightsail.disk-delete-while-attached-rejected
Issue-Shape-Id: com.amazonaws.lightsail#Lightsail_20161128
Issue-Key: lightsail.keypair-delete-fingerprint-mismatch-rejected|run#6|AssertionError|assert message == f"The KeyPair does not exist: {ghost_name}", f"unexpected message: {message!r}"
Issue-Key: lightsail.keypair-duplicate-name-rejected|run#17|AssertionError|assert gone_message == f"The KeyPair does not exist: {name}", f"unexpected message: {gone_message!r}"
Issue-Key: lightsail.disk-delete-while-attached-rejected|run#30|AssertionError|assert second_delete_message == f"The Disk does not exist: {disk_name}", (
Issue-Commit: 2258f924775e20f7bfc7db5498fbab3354a8d6a7

# Lightsail not-found refusals use one crate-wide "specified resource ... within types [...]" message where AWS words it "The <Type> does not exist: <name>" (GetKeyPair, DeleteKeyPair, DeleteDisk)

> _Note: this issue was identified against the code base at commit `2258f924775e20f7bfc7db5498fbab3354a8d6a7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteKeyPair

## Description
Two net-new root causes in one family: the Lightsail crate serves exactly ONE not-found message for every resource-addressed lookup miss, but AWS does not. For a resource the caller named by its own identifier and type (a key pair, a disk), AWS words the refusal "The <ResourceType> does not exist: <name>"; the generic "The specified resource <name> does not exist within types [<types>]. Please provide a valid resource name." wording is what AWS uses where the operation had to search SEVERAL types for an identifier (PutAlarm's monitoredResourceName, DeleteAlarm). The emulator applies the generic template everywhere, so every resource-addressed not-found carries the wrong message text. The HTTP status (400) and the wire error code (NotFoundException in __type) already match and are NOT part of these causes.

Note for whoever fixes this: open issue 20261029T234852Z-lightsail-client-error-bodies-carry-the-wrong-code.md requires the generic "within types [Alarm]" template for PutAlarm/DeleteAlarm. That issue is NOT a duplicate of this one and must not be undone by this fix: the correct end state is a per-operation choice of wording, not one crate-wide template. The crate currently documents the opposite in services/service-com-amazonaws-lightsail-lightsail-20161128/src/scaffolded.rs ("the crate's ONE not-found wording", Scaffolded::resource_does_not_exist / resource_does_not_exist_within), which is the mechanism both causes below come through.

Cause 1 of 2: key-pair not-found (DeleteKeyPair and GetKeyPair) serves the generic type-list message instead of "The KeyPair does not exist: <keyPairName>".
AWS: Smithy (com.amazonaws.lightsail#DeleteKeyPair, com.amazonaws.lightsail#GetKeyPair) — input keyPairName (ResourceName, @required, pattern ^\w[\w\-]*\w$); both model NotFoundException (@httpError 404, members code/docs/message/tip), documented "Lightsail throws this exception when it cannot find a resource". The message wording itself is recorded behavior only: against real Lightsail, DeleteKeyPair on a name that was never created, GetKeyPair on a deleted name, and a second DeleteKeyPair of an already-deleted name all answer the client error NotFoundException with message "The KeyPair does not exist: <keyPairName>". Fixed text: "The KeyPair does not exist: " — the resource type word "KeyPair" is fixed for these operations; the only variable part is the keyPairName copied verbatim from the request. (The surrounding assertions in these checks pass, so the status and the NotFoundException code the emulator serves are already right.)
Emulator: the same status and code, but message "The specified resource <keyPairName> does not exist within types [KeyPair]. Please provide a valid resource name." — a different template, with the name embedded mid-sentence and a bracketed single-element type list AWS does not emit here.
Why: a client or operator that matches Lightsail's documented not-found text (log scraping, idempotent-teardown guards that whitelist the exact message, test assertions, console-style surfacing of `message`) never matches, and the message it shows a user is not the message AWS shows. Any tooling that parses the identifier out of the message also mis-parses it, since the name sits in a different position.
Fix: the not-found path of the GetKeyPair and DeleteKeyPair handlers — they must construct the modelled NotFoundException with the per-resource "The KeyPair does not exist: <name>" wording instead of calling the crate's shared type-list builder. Because the shared builder is documented as the crate's only not-found wording, the fix belongs in the crate's error-construction seam as well: it needs a second, per-resource template that resource-addressed handlers select, with the type-list template reserved for the multi-type searches that actually use it.
Traces: lightsail.keypair-delete-fingerprint-mismatch-rejected (DeleteKeyPair on a never-created name), lightsail.keypair-duplicate-name-rejected (GetKeyPair on a just-deleted name).

Cause 2 of 2: disk not-found (DeleteDisk) serves the generic type-list message instead of "The Disk does not exist: <diskName>".
AWS: Smithy (com.amazonaws.lightsail#DeleteDisk) — input diskName (ResourceName, @required, pattern ^\w[\w\-]*\w$), forceDeleteAddOns (boolean); models NotFoundException alongside InvalidInputException and OperationFailureException. Message wording is recorded behavior only: DeleteDisk for a disk that was already deleted answers NotFoundException with message "The Disk does not exist: <diskName>". Fixed text: "The Disk does not exist: "; the only variable part is the diskName from the request. The same check's earlier refusal (deleting a still-attached disk -> OperationFailureException "You can't perform this operation because the disk is still attached to a Lightsail instance: <instanceName>.") passes, so only the not-found wording diverges.
Emulator: message "The specified resource <diskName> does not exist within types [Disk]. Please provide a valid resource name." — same crate-wide template as cause 1.
Why: deleting a disk twice is the normal shape of idempotent cleanup; a caller that recognises "already gone" from AWS's message does not recognise the emulator's, so cleanup paths that are correct against AWS surface an unexpected error against the emulator. GetDisk's not-found message is on the same path and is very likely wrong the same way, even though the check only asserts the code there.
Fix: the DeleteDisk handler's lookup-miss path (and the sibling disk readers that share it), using the per-resource template introduced for cause 1.
Traces: lightsail.disk-delete-while-attached-rejected (second DeleteDisk of the already-deleted disk, and the same assertion for the control disk).

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout: 20/27 checks passed; lightsail 20/24 with 4 failures (one of which is already owned by 20261022T170908Z). The three failures below are this issue.

Cause 1 — lightsail.keypair-delete-fingerprint-mismatch-rejected, verdict mismatch, DeleteKeyPair on a name never created:
  assert message == f"The KeyPair does not exist: {ghost_name}"
  AssertionError: unexpected message: 'The specified resource <keyPairName> does not exist within types [KeyPair]. Please provide a valid resource name.'
  (code was NotFoundException, as expected — only `message` diverges.)

Cause 1 — lightsail.keypair-duplicate-name-rejected, verdict mismatch, GetKeyPair on a key pair just deleted:
  assert gone_message == f"The KeyPair does not exist: {name}"
  AssertionError: unexpected message: 'The specified resource <keyPairName> does not exist within types [KeyPair]. Please provide a valid resource name.'
  (the preceding duplicate-create assertions — code InvalidInputException, message "Some names are already in use: <name>" — passed.)

Cause 2 — lightsail.disk-delete-while-attached-rejected, verdict mismatch, DeleteDisk of an already-deleted disk:
  assert second_delete_message == f"The Disk does not exist: {disk_name}"
  AssertionError: unexpected message re-deleting disk: 'The specified resource <diskName> does not exist within types [Disk]. Please provide a valid resource name.'
  (second_delete_code was NotFoundException, as expected; the earlier attached-disk OperationFailureException code and message both matched.)

The key pair and disk names above are request-supplied values generated per run; the template around them is the requirement. Source of the single shared template: services/service-com-amazonaws-lightsail-lightsail-20161128/src/scaffolded.rs lines 244-282 (Scaffolded::resource_does_not_exist / resource_does_not_exist_within / does_not_exist), reached from services/service-com-amazonaws-lightsail-lightsail-20161128/src/ops/delete_key_pair.rs lines 185 and 246-251.

## Steps to Reproduce
Boot the emulator and point a botocore lightsail client at its data listener in the emulator's region.

Cause 1a: ls.delete_key_pair(keyPairName="<a-name-never-created>"). Expect a ClientError with Error.Code == "NotFoundException" and Error.Message == "The KeyPair does not exist: <that name>". Observed the right code but message "The specified resource <that name> does not exist within types [KeyPair]. Please provide a valid resource name.".

Cause 1b: ls.create_key_pair(keyPairName="k1"); ls.delete_key_pair(keyPairName="k1"); then ls.get_key_pair(keyPairName="k1"). Expect NotFoundException with message "The KeyPair does not exist: k1". Same divergence. A second ls.delete_key_pair(keyPairName="k1") must answer the same message.

Cause 2: ls.create_disk(diskName="d1", availabilityZone=<az>, sizeInGb=8); wait for state "available"; ls.delete_disk(diskName="d1"); then ls.delete_disk(diskName="d1") again. Expect NotFoundException with message "The Disk does not exist: d1". Observed "The specified resource d1 does not exist within types [Disk]. Please provide a valid resource name.".

Re-replay to confirm: lightsail.keypair-delete-fingerprint-mismatch-rejected, lightsail.keypair-duplicate-name-rejected, lightsail.disk-delete-while-attached-rejected. Do not regress the alarm-path wording required by 20261029T234852Z-lightsail-client-error-bodies-carry-the-wrong-code.md.
