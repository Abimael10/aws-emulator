Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ssm.ssm-run-command-executes-on-a-real-managed-instance-and-returns-its-stdout ec2__ssm.guest-user-data-does-not-rerun-after-a-stop-start-cycle ec2__ssm.guest-user-data-output-read-back-through-run-command
Issue-Key: ec2__ssm.ssm-run-command-executes-on-a-real-managed-instance-and-returns-its-stdout|run#19|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2__ssm.guest-user-data-does-not-rerun-after-a-stop-start-cycle|run#16|AssertionError|assert "status: done" in (first["cloud_init"] or ""), (
Issue-Key: ec2__ssm.guest-user-data-output-read-back-through-run-command|run#22|AssertionError|assert inv["StandardErrorContent"] == "", (
Issue-Commit: 2c1da22bfb6e6388a411330b9028a26373fd79c1

# ec2__ssm: 3 net-new causes — the AL2023 managed-node guest has no `aws` CLI, no `cloud-init`, and a shell with neither coreutils (`seq`) nor POSIX builtins (`break`), so AWS-RunShellScript invocations end Failed or return empty markers

> _Note: this issue was identified against the code base at commit `2c1da22bfb6e6388a411330b9028a26373fd79c1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCommandInvocation

## Description
Three net-new root causes, all in the in-guest execution environment the emulator presents to SSM Run Command (`AWS-RunShellScript`) on an EC2 instance launched from the Amazon Linux 2023 AMI resolved through `/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64`. In each case the SSM control plane answers and the invocation reaches a terminal status, so these are NOT the "never comes Online" / "invocation never executes" family already open as 20261115T003554Z-ec2-ssm-ssm-managed-nodes-execute-nothing-invocations-end-failed.md, 20261115T000000Z-ec2-ssm-a-managed-nodes-guest-has-no-network-programs.md and 20261119T000635Z-ec2-ssm-an-aws-runshellscript-invocation-s-standardoutputcontent.md; the guest here DOES run the script and DOES return per-line stderr, it is the AL2023 userland that is missing. Those issues are named here only for context — none of them covers the three programs below, and the suite did not mark these checks as owned by any of them.

Cause 1 of 3: the AWS CLI is not present in the guest.
AWS: the Amazon Linux 2023 AMI ships AWS CLI v2 on PATH as `aws`, and the SSM agent runs `AWS-RunShellScript` commands as root with that PATH, so a script line invoking `aws ...` resolves. Smithy (com-amazonaws-ssm-amazonssm): GetCommandInvocation output carries `Status` (CommandInvocationStatus), `ResponseCode` (default 0), `StandardOutputContent` (@length 0..24000) and `StandardErrorContent` (@length 0..8000); a script whose every line exits 0 yields Status `Success` with ResponseCode 0 and empty StandardErrorContent. Recorded behavior: the check's script shells out to the CLI and expects Success.
Emulator: GetCommandInvocation reports `Status` = `Failed` with `StandardErrorContent` = "sh: aws: command not found\n" (the fixed text is the shell's `<shell>: <name>: command not found` form; `aws` is the program name taken from the script).
Why: any guest-side workflow that uses the instance's own credentials through the CLI — the single most common pattern in user data, bootstrap scripts, Run Command runbooks and CodeDeploy/CDK hooks — fails on the emulator with a shell error instead of running. A client cannot distinguish "the command is absent" from "the API denied me", and every scenario that reads back a value the CLI produced in-guest is unreachable.
Fix: the managed-node guest image/userland that backs the AL2023 AMI used by the EC2 instance runtime, and the PATH the SSM agent's `AWS-RunShellScript` plugin executes with — not the SSM API handlers, whose statuses are being reported faithfully.
Traces: ec2__ssm.ssm-run-command-executes-on-a-real-managed-instance-and-returns-its-stdout.

Cause 2 of 3: `cloud-init` is not present in the guest, so its status can never be read.
AWS: Amazon Linux 2023 boots under cloud-init, which is what executes EC2 user data. `cloud-init status` prints a line of the form `status: <state>` and, once the boot's modules have finished, `status: done`. recorded behavior only (the Smithy model describes no guest state): the recording shows the guest answering `cloud-init status` with a `status: ...` line.
Emulator: the probe line `echo "GUEST-CLOUD-INIT $(cloud-init status || true)"` produces an empty value — the command substitution yields nothing and the guest's stderr carries "sh: cloud-init: command not found\n" (fixed text `<shell>: <name>: command not found`, with `cloud-init` the program name from the script). The user-data ledger itself is written correctly (one line, carrying the current boot id), so user data IS being executed; only cloud-init as an observable program is absent.
Why: a client cannot determine whether instance initialization has completed. Every guest bootstrap gate that waits on `cloud-init status --wait` / `status: done` before declaring an instance ready — the standard way to know user data has finished — hangs or misreads the instance as never-initialized, and the once-per-instance-vs-per-boot semantics of user data cannot be observed at all.
Fix: the managed-node guest image/userland backing the AL2023 AMI (install/emulate cloud-init and have it report a `status:` line that reaches `done` once first-boot user data has run).
Traces: ec2__ssm.guest-user-data-does-not-rerun-after-a-stop-start-cycle, ec2__ssm.guest-user-data-output-read-back-through-run-command.

Cause 3 of 3: the guest shell has no coreutils and does not implement POSIX shell builtins.
AWS: on Amazon Linux 2023 `AWS-RunShellScript` runs the supplied lines through a POSIX shell. `seq` is part of coreutils and is on PATH; `break` is a shell BUILTIN defined by POSIX and is always available inside a loop — neither can produce "command not found". Smithy: with every line succeeding, GetCommandInvocation returns Status `Success`, ResponseCode 0 and `StandardErrorContent` of length 0.
Emulator: the read-back invocation's `StandardErrorContent` is "sh: seq: command not found\nsh: break: command not found\nsh: cloud-init: command not found\n" — three lines of the shell's fixed `<shell>: <name>: command not found` text, with `seq`, `break` and `cloud-init` being the names taken from the submitted script. That `break` — a builtin, not a PATH lookup — is reported not found shows the guest shell is a minimal command dispatcher rather than a POSIX shell, so loops and other control flow in a submitted script are silently not executed.
Why: scripts are not run with the semantics AWS documents. Loop and control-flow constructs are dropped rather than executed, so a client's Run Command script produces partial or empty results while the emulator may still report a terminal status, and any script using ordinary coreutils (`seq`, and by the same cause every other coreutils program) writes shell errors instead of output.
Fix: the guest shell/userland of the managed-node runtime — the `AWS-RunShellScript` plugin must execute the script body in a POSIX shell (so builtins such as `break`, `continue`, `for`, `while` work) over a userland that provides coreutils.
Traces: ec2__ssm.guest-user-data-output-read-back-through-run-command.

## Evidence
Suite: simparity-behavioral-aws-v1, 410/639 passed. Service heading `### ec2__ssm`.

Cause 1 — ec2__ssm.ssm-run-command-executes-on-a-real-managed-instance-and-returns-its-stdout [mismatch]:
  Expected: GetCommandInvocation Status == 'Success' (ResponseCode 0, empty StandardErrorContent) for a script that invokes the AWS CLI in-guest.
  Actual:   AssertionError: the invocation was 'Failed': 'Failed' stderr='sh: aws: command not found\n'

Cause 2 — ec2__ssm.guest-user-data-does-not-rerun-after-a-stop-start-cycle [mismatch]:
  Expected: the guest's `cloud-init status` output contains 'status: done' after first boot (and again after the stop/start cycle).
  Actual:   AssertionError: cloud-init must have finished before the rule is meaningful, it reported ''
  (the ledger assertions before it passed: runs == 1 and the ledger line carried the running kernel's boot id, so user data did execute.)

Cause 3 — ec2__ssm.guest-user-data-output-read-back-through-run-command [mismatch]:
  Expected: inv['StandardErrorContent'] == '' for the read-back script.
  Actual:   AssertionError: the read-back script wrote to stderr: 'sh: seq: command not found\nsh: break: command not found\nsh: cloud-init: command not found\n'
  `break` is a POSIX shell builtin and can never be 'command not found' on a real guest; `seq` is coreutils and is present on Amazon Linux 2023.

Note on message wording: the fixed text in each of these is the shell's own `<shell>: <name>: command not found`; the variable part is the program name taken from the submitted script (`aws`, `seq`, `break`, `cloud-init`). The correct behavior is that no such line is produced at all.

## Steps to Reproduce
Common setup (all three): create a VPC 10.173.0.0/16 with an internet gateway, a default route, one public subnet with MapPublicIpOnLaunch, and an egress-only security group; create an IAM role trusted by ec2.amazonaws.com with arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore attached and an instance profile holding it; resolve ImageId from SSM parameter /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 and RunInstances a t3.micro into the subnet with that profile; poll DescribeInstances to 'running' and DescribeInstanceInformation to PingStatus 'Online'.

Cause 1: ssm.send_command(InstanceIds=[id], DocumentName='AWS-RunShellScript', Parameters={'commands': ['aws sts get-caller-identity']}); poll ssm.get_command_invocation(CommandId, InstanceId) to a terminal Status. Observe Status 'Failed' and StandardErrorContent 'sh: aws: command not found\n'; AWS returns Status 'Success', ResponseCode 0, empty StandardErrorContent.

Cause 2: launch with UserData '#!/bin/bash\nmkdir -p /var/lib/sim2real-guest\ncat /proc/sys/kernel/random/boot_id >> /var/lib/sim2real-guest/userdata-runs\n'. Then send_command AWS-RunShellScript with the single line: echo "GUEST-CLOUD-INIT $(cloud-init status || true)". Observe the marker value is empty and stderr carries 'sh: cloud-init: command not found'; AWS prints a 'status: ...' line that reads 'status: done' once first-boot modules have finished (and again after a stop/start).

Cause 3: send_command AWS-RunShellScript with a script that uses coreutils and a loop builtin, e.g. ['for i in $(seq 1 3); do echo $i; break; done']; read StandardErrorContent via get_command_invocation. Observe 'sh: seq: command not found' and 'sh: break: command not found'; AWS runs the loop and returns empty StandardErrorContent with ResponseCode 0.
