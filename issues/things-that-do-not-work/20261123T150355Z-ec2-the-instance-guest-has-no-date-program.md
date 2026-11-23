Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.dasgrefresh-run-command-to-a-stopped-warm-pool-instance-rejected-then-that-same-guest-serves
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.dasgrefresh-run-command-to-a-stopped-warm-pool-instance-rejected-then-that-same-guest-serves|run#52|AssertionError|assert inv["Status"] == "Success", (
Issue-Commit: 3ae1846d4c1ce91fa3a8a650fb4cf01548153921

# ec2: the instance guest has no `date` program, so an AWS-RunShellScript probe of a warm-pool-promoted instance ends Failed with 'sh: date: command not found'

> _Note: this issue was identified against the code base at commit `3ae1846d4c1ce91fa3a8a650fb4cf01548153921`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCommandInvocation

## Description
One net-new root cause in the emulated EC2 guest's installed-program set.

Cause 1 of 1: the guest provides no `date` program, so any in-guest script that reads the current wall-clock epoch dies on it and the Run Command invocation reports Failed instead of Success.

AWS: recorded behavior only for the in-guest program set — AWS's API model says nothing about which programs a machine image installs. On a Linux managed node (Amazon Linux 2 / AL2023, the AMI this scenario resolves from /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64) `date` is part of the base image's coreutils, so `date +%s` prints the current Unix epoch as a single integer line and exits 0, both in cloud-init user data and in an aws:runShellScript document. Consequently an AWS-RunShellScript invocation that assigns `NOW=$(date +%s)` under `set -euo pipefail` runs to completion, and GetCommandInvocation reports Status=Success with ResponseCode 0, the script's line on StandardOutputContent and an empty StandardErrorContent (Smithy: GetCommandInvocation output members Status/StandardOutputContent/StandardErrorContent/ResponseCode). The epoch value itself is per-run and is not the requirement; that `date +%s` resolves to a program and prints an integer is.

Emulator: the in-process guest session installs no `date`, so the command is not found. The shell answers its could-not-find diagnostic on standard error, `set -e` aborts the script, and GetCommandInvocation reports Status='Failed' with StandardErrorContent 'sh: date: command not found\n'. The same guest DOES answer the sibling reads in the same script (/proc/uptime, /proc/sys/kernel/random/boot_id, the IMDSv2 token and instance-id), so only the clock program is missing — and the guest already holds the fact it should print, since its own boot/clock instants are what the session's tree is built from. The identical gap also breaks the user data this scenario writes, which uses `date +%s` to stamp its initialisation marker.

Why: reading the guest's current epoch is how a client establishes, from inside the machine and without trusting any control-plane record, when the filesystem was first initialised versus when the running boot started — exactly the evidence that an Auto Scaling warm-pool instance was pre-initialised in an EARLIER boot than the one now serving. Against the emulator every such probe ends Failed, so the whole class of in-guest timing checks (bootstrap idempotence, drift detection, pre-initialisation proofs, log timestamping) cannot run, and a client sees a managed node that is Online yet cannot execute a one-line POSIX script.

Fix: the guest's installed-program set — `installed_programs` in services/service-com-amazonaws-ec2-amazonec2/src/state/guest_services.rs and the machine's own programs in src/state/guest_metadata.rs, which src/state/guest_commands.rs layers into the session — needs a `date` program answering from the session's own clock (`+%s` printing the current Unix epoch as an integer, exit 0). It is not the Systems Manager side, which only delivers and reports the script.

Related but not covering this check: open issues 20261123T021656Z-cloudformation-ec2-ssm-run-command-guest-has-no-uptime-program-so, 20261121T165349Z-ec2-ssm-3-net-new-causes-the-al2023 and 20261121T213740Z-ec2-sts-the-instance-profile-guest-has-no name OTHER missing guest programs (`uptime`, `aws`, `cloud-init`, `seq`) under other service headings; none names `date` and none carries this check's replay key, and the report lists this check as neither already filed nor owned.

Traces: ec2.dasgrefresh-run-command-to-a-stopped-warm-pool-instance-rejected-then-that-same-guest-serves.

## Evidence
Expected (AWS): the AWS-RunShellScript probe on the promoted warm-pool instance completes — GetCommandInvocation Status='Success', empty StandardErrorContent, and StandardOutputContent carrying the single line `iid=<instance id> init=<epoch> boot=<epoch> initboot=<boot id> bootid=<boot id>` (the ids and epochs are per-run values; the line's shape is the requirement).

Actual (emulator):
  [mismatch] ec2.dasgrefresh-run-command-to-a-stopped-warm-pool-instance-rejected-then-that-same-guest-serves
  File "check_ec2_dasgrefresh_run_command_to_a_stopped_warm_pool_instance_rejected_then_that_same_guest_serves.py", line 669, in run
    assert inv["Status"] == "Success", (
  AssertionError: the in-guest probe on the promoted instance was 'Failed' ('Failed'), stderr='sh: date: command not found\n'

The failing step is the script's `NOW=$(date +%s)` line; the preceding lines of the same script (`for _ in $(seq 1 90); ...`, the IMDSv2 token and instance-id curls, `head`/`cut` of the marker file) produced no diagnostic, and `cut -d. -f1 /proc/uptime` / `cat /proc/sys/kernel/random/boot_id` are reachable in the same guest.

## Steps to Reproduce
1. Launch an EC2 instance from the AL2023 AMI with an instance profile granting AmazonSSMManagedInstanceCore, and user data that appends `"$(date +%s) $(cat /proc/sys/kernel/random/boot_id)"` to a marker file.
2. Wait until it is an Online managed node.
3. ssm:SendCommand with DocumentName=AWS-RunShellScript and Parameters.commands containing `set -euo pipefail` followed by `NOW=$(date +%s)` and `echo "boot=$NOW"`.
4. Poll ssm:GetCommandInvocation until terminal.
5. Observe Status='Failed' and StandardErrorContent 'sh: date: command not found'; on a real managed node the invocation reaches Success, ResponseCode 0, and standard output carries the epoch.
