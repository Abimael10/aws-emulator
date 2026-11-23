Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2__ssm.deploy-ec2-3-instance-type-update-reboots-in-place-tags-do-not-drift-tracks-out-of-band-ingress
Issue-Key: cloudformation__ec2__ssm.deploy-ec2-3-instance-type-update-reboots-in-place-tags-do-not-drift-tracks-out-of-band-ingress|_read_boot#18|ValueError|"boot_time": datetime.strptime(fields["BOOTTIME"].strip(), "%Y-%m-%d %H:%M:%S"),
Issue-Commit: 42819415083a81c3cc8c8415cafe0862fc134a09

# Run Command guest has no `uptime` program, so a script reading the guest boot time gets an empty value

> _Note: this issue was identified against the code base at commit `42819415083a81c3cc8c8415cafe0862fc134a09`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCommandInvocation

## Description
Cause 1 of 1: the emulated instance guest provides no `uptime` program, so `uptime -s` inside an AWS-RunShellScript document produces no output and the invocation reports a script whose boot-time field is empty.

AWS: an AWS-RunShellScript invocation performs the document's commands with the guest's shell and GetCommandInvocation reports that script's StandardOutputContent, StandardErrorContent and ResponseCode (Smithy: GetCommandInvocation output members StandardOutputContent/StandardErrorContent/ResponseCode; the script itself is the aws:runShellScript plugin's `commands` input — recorded behavior only for the in-guest program set, which AWS's API model says nothing about). On a Linux managed node (Amazon Linux 2/2023) `uptime` is part of the base image, and `uptime -s` prints the guest's boot instant as a single line in the fixed format `YYYY-MM-DD HH:MM:SS` and exits 0 — so `echo BOOTTIME=$(uptime -s)` writes `BOOTTIME=<that timestamp>` to standard output. The timestamp value itself is per-run and is not part of the requirement; its presence and `%Y-%m-%d %H:%M:%S` shape are.

Emulator: the invocation succeeds and the companion command in the same script works — `echo BOOTID=$(cat /proc/sys/kernel/random/boot_id)` yields a boot id — but `uptime -s` names a program the in-process guest session does not install, so it runs nothing, answers the shell's could-not-find status on standard error, and the command substitution expands to the empty string. StandardOutputContent therefore contains the line `BOOTTIME=` with no value, and a client parsing it with `%Y-%m-%d %H:%M:%S` fails (`ValueError: time data '' does not match format '%Y-%m-%d %H:%M:%S'`). The guest already HOLDS the fact the program should print: the current boot's `booted_at` is the instant the session's own tree is built from (see Fix citations).

Why: a client that uses Run Command to establish whether a guest rebooted — the standard way to confirm that a CloudFormation/EC2 InstanceType update stopped and started the SAME instance, and that a tags-only update did not reboot it — reads an empty boot time instead of a timestamp. Any in-guest script that reads uptime or boot time (health probes, idempotent bootstrap scripts, drift checks) silently sees an empty value rather than data, and because the failure is an empty substitution rather than a non-zero script exit, the invocation still reports success, so the client cannot even detect the missing program from the invocation status.

Fix: the guest's installed-program set — `installed_programs` in services/service-com-amazonaws-ec2-amazonec2/src/state/guest_services.rs / the machine's own programs in src/state/guest_metadata.rs, which is what src/state/guest_commands.rs layers into the session — needs an `uptime` program answering from the current boot's `booted_at` (`-s` printing the boot instant as `YYYY-MM-DD HH:MM:SS`, bare `uptime` printing the load-average line), not the Systems Manager side, which only delivers the script.

Traces: cloudformation__ec2__ssm.deploy-ec2-3-instance-type-update-reboots-in-place-tags-do-not-drift-tracks-out-of-band-ingress (the check's `_read_boot` helper, which runs `echo BOOTID=$(cat /proc/sys/kernel/random/boot_id)` and `echo BOOTTIME=$(uptime -s)` through SendCommand and parses BOOTTIME). Code citations: services/service-com-amazonaws-ec2-amazonec2/src/state/guest_commands.rs lines 25-31 (a command the session provides no program for answers the could-not-find status) and 247-292 (the session's program layers and the boot whose `booted_at` is read); services/service-com-amazonaws-ssm-amazonssm/src/state/guest_execution.rs lines 14-24.

## Evidence
Expected (AWS): GetCommandInvocation StandardOutputContent for the two-line script contains `BOOTID=<uuid>` and `BOOTTIME=<YYYY-MM-DD HH:MM:SS>` — a parseable boot instant, which the check compares across an InstanceType update (boot id changes) and across a tags-only update (boot id and boot time unchanged within 5 s).

Actual (emulator): StandardOutputContent contains the BOOTID line but `BOOTTIME=` with an empty value, because `uptime` is not a program of the emulated guest; the replay fails with:
  File "_strptime.py", line 432, in _strptime
  ValueError: time data '' does not match format '%Y-%m-%d %H:%M:%S'
raised at check line 8508 equivalent: `datetime.strptime(fields["BOOTTIME"].strip(), "%Y-%m-%d %H:%M:%S")`. The script still reports success, so the missing program is invisible in the invocation status.

## Steps to Reproduce
1. Launch an EC2 instance with an instance profile granting AmazonSSMManagedInstanceCore and wait until it is an Online managed node.
2. ssm:SendCommand with DocumentName=AWS-RunShellScript and Parameters.commands=["echo BOOTID=$(cat /proc/sys/kernel/random/boot_id)", "echo BOOTTIME=$(uptime -s)"].
3. Poll ssm:GetCommandInvocation until Status=Success and read StandardOutputContent.
4. Observe `BOOTID=<uuid>` present but `BOOTTIME=` empty (and the shell's could-not-find diagnostic for `uptime` on StandardErrorContent); on a real managed node BOOTTIME carries the boot instant as YYYY-MM-DD HH:MM:SS.
