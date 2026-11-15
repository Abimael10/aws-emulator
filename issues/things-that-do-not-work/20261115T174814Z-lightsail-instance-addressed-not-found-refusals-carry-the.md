Issue-Source: trace-sweep-divergence
Issue-Trace: lightsail-instance-error-paths
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lightsail-instance-error-paths lightsail-cloudformation-and-copy-error-paths lightsail-catalog-and-operations-lifecycle
Issue-Shape-Id: com.amazonaws.lightsail#Lightsail_20161128
Issue-Key: lightsail|GetInstance|4xx->4xx|$.message
Issue-Key: lightsail|DeleteInstance|4xx->4xx|$.message
Issue-Key: lightsail|RebootInstance|4xx->4xx|$.message
Issue-Key: lightsail|AttachDisk|4xx->4xx|$.message
Issue-Key: lightsail|OpenInstancePublicPorts|4xx->4xx|$.message
Issue-Key: lightsail|GetInstanceAccessDetails|4xx->4xx|$.message
Issue-Key: lightsail|DeleteKnownHostKeys|4xx->4xx|$.message
Issue-Key: lightsail|GetKeyPair|4xx->4xx|$.message
Issue-Commit: 342c23c982e836d88694781a4c8bb7d7dc4be12f

# Lightsail instance-addressed not-found refusals carry the generic type-list message instead of AWS's "The <Type> does not exist: <name>"

> _Note: this issue was identified against the code base at commit `342c23c982e836d88694781a4c8bb7d7dc4be12f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetInstance

## Description
EIGHT net-new root causes in one family (Cause 1 of 8 .. Cause 8 of 8), all on the lightsail service. Every Lightsail operation that addresses ONE resource by its own identifier and type answers a lookup miss with the crate's single shared sentence "The specified resource <name> does not exist within types [<Type>]. Please provide a valid resource name." AWS reserves that type-list wording for the operations that have to SEARCH several resource types for an identifier (PutAlarm$monitoredResourceName, DeleteAlarm); for a resource the caller named by type it words the refusal "The <ResourceType> does not exist: <name>". The HTTP status (400) and the body's error `code` the emulator serves already match the recording in every cause below and are NOT part of any of them: only `message` diverges (and, in Cause 4, which resource the refusal is about at all).

Related open issues, neither of which covers these steps (the replay reports no owner for any cluster below). 20261115T115602Z-lightsail-not-found-refusals-use-one-crate-wide.md is the BEHAVIORAL issue for the same family, but it is grounded in three behavioral check ids and covers only DeleteKeyPair/GetKeyPair and DeleteDisk; it carries no replay key for any wire fixture step here, and six of the eight causes below (the instance-addressed operations and DeleteKnownHostKeys) are outside it entirely. 20261029T234852Z-lightsail-client-error-bodies-carry-the-wrong-code.md requires the GENERIC "within types [Alarm]" template for PutAlarm/DeleteAlarm and must NOT be undone by this fix: the correct end state is a per-operation choice of wording, not one crate-wide template. The mechanism all eight causes come through is documented in the crate as "the crate's ONE not-found wording".

Cause 1 of 8: GetInstance on an instance name the account does not hold.
AWS: Smithy com.amazonaws.lightsail#GetInstance — input instanceName (ResourceName, @required, pattern ^\w[\w\-]*\w$); models NotFoundException ("Lightsail throws this exception when it cannot find a resource", members code/docs/message/tip). The message wording itself is recorded behavior only: the recorded response is HTTP 400 with the body code the emulator already serves and message "The Instance does not exist: <instanceName>". Fixed text: "The Instance does not exist: " — the resource-type word "Instance" is fixed for this operation; the ONLY variable part is the instanceName copied verbatim from the request.
Emulator: same status and code, message "The specified resource <instanceName> does not exist within types [Instance]. Please provide a valid resource name." — a different template, with the name mid-sentence and a bracketed one-element type list AWS does not emit here.
Why: a client that recognises Lightsail's documented not-found text (idempotent-teardown guards that whitelist the exact message, log scraping, console-style surfacing of `message`, tooling that parses the identifier out of the message) never matches, and the sentence shown to a user is not the sentence AWS shows; the name also sits in a different position, so message parsing mis-extracts it.
Fix: the lookup-miss path of the GetInstance handler, and the crate's error-construction seam it calls — it needs a second, per-resource template that resource-addressed handlers select, with the type-list template reserved for the multi-type searches that actually use it.
Traces: lightsail-instance-error-paths#task#1.

Cause 2 of 8: DeleteInstance on an instance name the account does not hold.
AWS: Smithy com.amazonaws.lightsail#DeleteInstance — input instanceName (ResourceName, @required, same pattern), forceDeleteAddOns; models NotFoundException. Recorded behavior only: HTTP 400, message "The Instance does not exist: <instanceName>" — same fixed text, same single variable part.
Emulator: the generic "The specified resource <instanceName> does not exist within types [Instance]. Please provide a valid resource name."
Why: deleting an instance twice is the normal shape of idempotent cleanup; a caller that recognises "already gone" from AWS's message does not recognise the emulator's, so cleanup that is correct against AWS surfaces an unexpected error here.
Fix: the DeleteInstance handler's lookup-miss path, via the per-resource template of Cause 1.
Traces: lightsail-instance-error-paths#task#2.

Cause 3 of 8: RebootInstance on an instance name the account does not hold.
AWS: Smithy com.amazonaws.lightsail#RebootInstance — input instanceName (ResourceName, @required); models NotFoundException. Recorded behavior only: HTTP 400, message "The Instance does not exist: <instanceName>".
Emulator: the generic type-list sentence with [Instance].
Why: as Cause 1 — a control-plane action on a vanished instance reports a message the caller cannot match.
Fix: the RebootInstance handler's lookup-miss path.
Traces: lightsail-instance-error-paths#task#3.

Cause 4 of 8: AttachDisk refuses the request about the WRONG operand, and with the wrong wording. This cause has two aspects.
AWS: Smithy com.amazonaws.lightsail#AttachDisk — input diskName, instanceName, diskPath all @required, autoMounting optional; models NotFoundException. Recorded behavior only: with BOTH a disk name and an instance name the account does not hold, AWS answers HTTP 400 with message "The Instance does not exist: <instanceName>" — i.e. the INSTANCE operand is resolved first, and the refusal names the instance from the request. Fixed text "The Instance does not exist: "; the variable part is the instanceName.
Emulator: message "The specified resource <diskName> does not exist within types [Disk]. Please provide a valid resource name." — both the wrong template AND the wrong operand: it names the diskName and resource type Disk, because the handler resolves the disk before the instance (services/service-com-amazonaws-lightsail-lightsail-20161128/src/ops/attach_disk.rs lines 199-213 look up store.disk(&disk_name) first and only then store.instance(&instance_name)).
Why: the caller is told the wrong resource is missing. A client repairing the request creates or re-points the disk and retries, and is refused again for the instance it was never told about; any automation that keys on the resource type/name in the refusal takes the wrong corrective action.
Fix: the AttachDisk handler's validation order — resolve the instance before the disk so the refusal names the operand AWS names — plus the per-resource message template of Cause 1 for both operands.
Traces: lightsail-instance-error-paths#task#4.

Cause 5 of 8: OpenInstancePublicPorts on an instance name the account does not hold.
AWS: Smithy com.amazonaws.lightsail#OpenInstancePublicPorts — input portInfo (@required) and instanceName (ResourceName, @required); models NotFoundException. Recorded behavior only: HTTP 400, message "The Instance does not exist: <instanceName>".
Emulator: the generic type-list sentence with [Instance].
Why: as Cause 1; firewall automation that distinguishes "instance gone" from other 400s by message text cannot.
Fix: the OpenInstancePublicPorts handler's lookup-miss path.
Traces: lightsail-instance-error-paths#task#5.

Cause 6 of 8: GetInstanceAccessDetails on an instance name the account does not hold.
AWS: Smithy com.amazonaws.lightsail#GetInstanceAccessDetails — input instanceName (ResourceName, @required), protocol (InstanceAccessProtocol enum); models NotFoundException. Recorded behavior only: HTTP 400, message "The Instance does not exist: <instanceName>".
Emulator: the generic type-list sentence with [Instance].
Why: as Cause 1; this is the call an SSH/RDP client front-end makes, so the message it shows an operator is the emulator's rather than AWS's.
Fix: the GetInstanceAccessDetails handler's lookup-miss path.
Traces: lightsail-instance-error-paths#task#6.

Cause 7 of 8: DeleteKnownHostKeys on an instance name the account does not hold.
AWS: Smithy com.amazonaws.lightsail#DeleteKnownHostKeys — input instanceName (ResourceName, @required); models NotFoundException. Recorded behavior only: HTTP 400, message "The Instance does not exist: <instanceName>".
Emulator: the generic type-list sentence with [Instance].
Why: as Cause 1.
Fix: the DeleteKnownHostKeys handler's lookup-miss path.
Traces: lightsail-cloudformation-and-copy-error-paths#task#5.

Cause 8 of 8: GetKeyPair on a key pair the account no longer holds (after the pair is deleted).
AWS: Smithy com.amazonaws.lightsail#GetKeyPair — input keyPairName (ResourceName, @required, pattern ^\w[\w\-]*\w$); models NotFoundException. Recorded behavior only: HTTP 400, message "The KeyPair does not exist: <keyPairName>". Fixed text "The KeyPair does not exist: "; the only variable part is the keyPairName from the request.
Emulator: message "The specified resource <keyPairName> does not exist within types [KeyPair]. Please provide a valid resource name."
Why: as Cause 1, on the teardown path a client runs after deleting a pair.
Fix: the GetKeyPair handler's lookup-miss path, via the per-resource template of Cause 1. Note: the same wording for GetKeyPair/DeleteKeyPair is described in behavioral issue 20261115T115602Z, but that issue carries no replay key for this fixture step, so this wire step must be re-replayed here.
Traces: lightsail-catalog-and-operations-lifecycle#teardown#9.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 34 steps, 16 passed, 18 mismatched, score 47.1%. The eight clusters below are reported with owned_by empty and stub_path empty — no open issue carries their replay key. In each, expected_status == actual_status == 400 and the body `code` compared equal; the diverging key is $.message (plus the resource the message is about, in Cause 4). The resource names are per-run request-supplied values; the TEMPLATE around them is the requirement.

Cause 1 — lightsail-instance-error-paths#task#1, GetInstance, verdict mismatch:
  $.message: value_mismatch (expected 'The Instance does not exist: sim2real-probe-lightsail-instance-missing-f0ca242d', got 'The specified resource sim2real-probe-lightsail-instance-missing-f0ca242d does not exist within types [Instance]. Please provide a valid resource name.')

Cause 2 — lightsail-instance-error-paths#task#2, DeleteInstance, verdict mismatch: identical expected/actual pair of messages.

Cause 3 — lightsail-instance-error-paths#task#3, RebootInstance, verdict mismatch: identical expected/actual pair of messages.

Cause 4 — lightsail-instance-error-paths#task#4, AttachDisk, verdict mismatch:
  $.message: value_mismatch (expected 'The Instance does not exist: sim2real-probe-lightsail-instance-missing-f0ca242d', got 'The specified resource sim2real-probe-lightsail-disk-missing-f0ca242d does not exist within types [Disk]. Please provide a valid resource name.')
  Note the actual names the DISK operand, not the instance: the handler resolves the disk first (src/ops/attach_disk.rs lines 204-213).

Cause 5 — lightsail-instance-error-paths#task#5, OpenInstancePublicPorts, verdict mismatch: expected 'The Instance does not exist: <instanceName>', got the generic 'within types [Instance]' sentence.

Cause 6 — lightsail-instance-error-paths#task#6, GetInstanceAccessDetails, verdict mismatch: expected 'The Instance does not exist: <instanceName>', got the generic 'within types [Instance]' sentence.

Cause 7 — lightsail-cloudformation-and-copy-error-paths#task#5, DeleteKnownHostKeys, verdict mismatch:
  $.message: value_mismatch (expected 'The Instance does not exist: sim2real-probe-lightsail-known-hosts-target-2599359f', got 'The specified resource sim2real-probe-lightsail-known-hosts-target-2599359f does not exist within types [Instance]. Please provide a valid resource name.')

Cause 8 — lightsail-catalog-and-operations-lifecycle#teardown#9, GetKeyPair, verdict mismatch:
  $.message: value_mismatch (expected 'The KeyPair does not exist: sim2real-probe-lightsail-ops-keypair-56f3cae5', got 'The specified resource sim2real-probe-lightsail-ops-keypair-56f3cae5 does not exist within types [KeyPair]. Please provide a valid resource name.')

Source of the one shared template: services/service-com-amazonaws-lightsail-lightsail-20161128/src/scaffolded.rs lines 244-282 (Scaffolded::resource_does_not_exist -> resource_does_not_exist_within -> does_not_exist), whose own doc comment calls it "the crate's ONE not-found wording"; reached from each operation's local not_found() helper, e.g. src/ops/attach_disk.rs lines 342-344.

## Steps to Reproduce
Boot the emulator and point a botocore/boto3 lightsail client at its data listener in the emulator's region. In each step the expected Error.Code and HTTP 400 already match; only Error.Message is wrong.

Cause 1: ls.get_instance(instanceName="ghost-instance") -> expect Error.Message == "The Instance does not exist: ghost-instance"; observed "The specified resource ghost-instance does not exist within types [Instance]. Please provide a valid resource name.".
Cause 2: ls.delete_instance(instanceName="ghost-instance") -> same expectation, same divergence.
Cause 3: ls.reboot_instance(instanceName="ghost-instance") -> same.
Cause 4: ls.attach_disk(diskName="ghost-disk", instanceName="ghost-instance", diskPath="/dev/xvdf") with NEITHER resource existing -> expect "The Instance does not exist: ghost-instance"; observed "The specified resource ghost-disk does not exist within types [Disk]. Please provide a valid resource name." (wrong operand and wrong template).
Cause 5: ls.open_instance_public_ports(instanceName="ghost-instance", portInfo={"fromPort":22,"toPort":22,"protocol":"tcp"}) -> expect "The Instance does not exist: ghost-instance".
Cause 6: ls.get_instance_access_details(instanceName="ghost-instance") -> expect "The Instance does not exist: ghost-instance".
Cause 7: ls.delete_known_host_keys(instanceName="ghost-instance") -> expect "The Instance does not exist: ghost-instance".
Cause 8: ls.create_key_pair(keyPairName="k1"); ls.delete_key_pair(keyPairName="k1"); ls.get_key_pair(keyPairName="k1") -> expect "The KeyPair does not exist: k1".

Re-replay to confirm the fix: lightsail-instance-error-paths#task#1, #task#2, #task#3, #task#4, #task#5, #task#6; lightsail-cloudformation-and-copy-error-paths#task#5; lightsail-catalog-and-operations-lifecycle#teardown#9. Do NOT regress the generic alarm-path wording required by 20261029T234852Z-lightsail-client-error-bodies-carry-the-wrong-code.md (PutAlarm/DeleteAlarm keep the "within types [...]" template).
