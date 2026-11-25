Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.dasgrefresh-run-command-to-a-stopped-warm-pool-instance-rejected-then-that-same-guest-serves
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.dasgrefresh-run-command-to-a-stopped-warm-pool-instance-rejected-then-that-same-guest-serves|run#60|AssertionError|assert init_boot_id != boot_id and init_epoch <= boot_epoch, (
Issue-Commit: 36c0ec5dd403fd4e9beb4ab000e55c4d6e6ab9a1

# ec2: a warm-pool-promoted instance is a cold launch — the guest's first-initialisation epoch and boot id are the serving boot's, so no pre-initialised earlier boot ever existed

> _Note: this issue was identified against the code base at commit `36c0ec5dd403fd4e9beb4ab000e55c4d6e6ab9a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartInstances

## Description
Cause 1 of 1: an instance promoted out of an Auto Scaling warm pool serves from the SAME (and only) kernel boot in which its user data first initialised it, so from inside the guest it is indistinguishable from a cold launch. Failing check id: ec2.dasgrefresh-run-command-to-a-stopped-warm-pool-instance-rejected-then-that-same-guest-serves.

AWS: a warm pool prepares an instance (user data runs, the root filesystem is written), stops it, and later promotion STARTS that same stopped instance. Smithy (ec2) StartInstances documents exactly what that means for the guest: "Starts an Amazon EBS-backed instance that you've previously stopped... your root partition Amazon EBS volume remains and continues to persist your data" and "Stopping an instance does not preserve data stored in RAM". So a started instance is a NEW kernel boot on a PERSISTED root filesystem. Observably, in the promoted guest: (a) the marker written by the initialising boot still exists and carries that earlier boot's epoch, so init_epoch <= current_boot_epoch, where current_boot_epoch = date +%s minus /proc/uptime; and (b) /proc/sys/kernel/random/boot_id is regenerated on every boot, so the boot id recorded in the marker at initialisation differs from the running boot id. Beyond the StartInstances documentation this is recorded behaviour only — the model states no guest-visible field for it.

Emulator: the promoted guest reports its initialisation as happening INSIDE the boot that is serving, one second after that boot began, with one single boot id for both. The guest printed: `iid=i-032fd94022e1710b6 init=<init-epoch> boot=<boot-epoch> initboot=<boot-id> bootid=<boot-id>` with init-epoch one second AFTER boot-epoch and initboot == bootid. Two facts diverge: the instance was never prepared-and-stopped before promotion (user data runs in the only boot it ever has, i.e. a cold launch), and the boot id is not a fresh per-boot value — in the recording it was a zero-padded value derived from the instance's ordinal (Hypothesis: boot_id is synthesized per instance rather than per boot, which is why the initialising and serving boots cannot be told apart). Per-run values (the instance id, the two epochs, the particular id string) are the template's variable parts, not the requirement; the requirement is init_epoch <= boot_epoch AND initboot != bootid.

Why: the entire point of a warm pool is that the expensive initialisation happened earlier, in a previous boot; a client validating that its pool actually pre-bakes instances (image bake-out, cache warming, agent pre-registration, boot-time measurement) sees a cold launch and concludes the warm pool did nothing — or, worse, passes a test against the emulator that would fail on AWS. Any guest-side logic keyed on boot_id changing across stop/start (first-boot-only provisioning guards, per-boot nonce, dedup of boot events) also misbehaves, because the emulator's boot id never changes.

Fix: the instance lifecycle/guest-boot state for warm-pool promotion — the pool's prepared instance must be initialised and then stopped, and promotion must START that stopped instance (new boot, persisted root filesystem) rather than provisioning a fresh one; and the guest's /proc/sys/kernel/random/boot_id must be regenerated per boot instead of being derived from the instance identity.

Related open issues that do NOT cover this cause (the replay did not mark this check owned by them): 20261123T150355Z-ec2-the-instance-guest-has-no-date-program.md (a warm-pool probe failing because the guest lacks `date`) and 20261124T204233Z-ec2-3-net-new-causes-createnetworkinterfacepermission-grants-a.md (a warm-pool-promoted instance's lifecycle state). Here the guest DID run and DID report; the divergence is in which boot initialised it.

Traces: ec2.dasgrefresh-run-command-to-a-stopped-warm-pool-instance-rejected-then-that-same-guest-serves (verdict: mismatch).

## Evidence
Expected (AWS): for the instance promoted out of the warm pool, the in-guest report must satisfy init_epoch <= boot_epoch (the filesystem was initialised no later than the start of the boot now serving) AND initboot != bootid (the initialising boot is a different kernel boot from the serving one), because promotion is a StartInstances on a stopped, already-initialised instance whose root EBS volume persisted.

Actual (emulator): the assertion failed with
  AssertionError: the promoted guest must have been initialised in an EARLIER boot than the one now serving — that is what a warm pool prepares — but the host says it was initialised at 1795578924 in boot '00000000-0000-0000-0000-0000000000cd' and the serving boot '00000000-0000-0000-0000-0000000000cd' began at 1795578923, which is what a cold launch produces; the guest printed 'iid=i-032fd94022e1710b6 init=1795578924 boot=1795578923 initboot=00000000-0000-0000-0000-0000000000cd bootid=00000000-0000-0000-0000-0000000000cd'
i.e. init_epoch (1795578924) is one second AFTER boot_epoch (1795578923), and initboot == bootid. The instance id, the epochs and the id string are this run's values; the diverging invariants are init_epoch <= boot_epoch and initboot != bootid.

## Steps to Reproduce
1. Build a VPC (10.78.0.0/16) with a subnet (10.78.1.0/24) and an instance profile carrying AmazonSSMManagedInstanceCore.
2. Create a launch template for t3.micro on the AL2023 AMI resolved from SSM /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64, with user data that APPENDS one line to /etc/sim2real-warm-init: `$(date +%s) $(cat /proc/sys/kernel/random/boot_id)`.
3. Create an Auto Scaling group on that template and PutWarmPool so an instance is initialised and left Stopped in the pool.
4. Scale the group out so the stopped warm-pool instance is promoted (started) and becomes InService.
5. Via SSM AWS-RunShellScript on the promoted instance, read: the first line of /etc/sim2real-warm-init (init epoch and init boot id), `date +%s` minus the integer part of /proc/uptime (the serving boot's epoch) and /proc/sys/kernel/random/boot_id.
6. Expected: init epoch <= serving boot epoch and init boot id != running boot id. Observed: init epoch is one second after the serving boot epoch and both boot ids are the same value, i.e. a cold launch.
