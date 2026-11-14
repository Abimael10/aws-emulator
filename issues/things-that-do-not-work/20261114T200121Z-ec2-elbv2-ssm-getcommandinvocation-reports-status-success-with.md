Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__elbv2__ssm.dtgthealth-guest-health-endpoint-decides-alb-target-rotation ec2__elbv2__ssm.dtgthealth-guest-stalled-health-response-times-out-and-withdraws-the-target
Issue-Key: ec2__elbv2__ssm.dtgthealth-guest-health-endpoint-decides-alb-target-rotation|run#15|AssertionError|assert deployed.get("UNIT-A") == "active" and deployed.get("UNIT-B") == "active", (
Issue-Key: ec2__elbv2__ssm.dtgthealth-guest-stalled-health-response-times-out-and-withdraws-the-target|run#15|AssertionError|assert deployed.get("UNIT-A") == "active" and deployed.get("UNIT-B") == "active", (
Issue-Commit: 6b20299dda36b610e0eb6266197358af63fda05b

# ec2__elbv2__ssm: GetCommandInvocation reports Status 'Success' with an empty StandardOutputContent — an AWS-RunShellScript script never runs in the guest, so both guest-driven ALB target-health checks are blocked

> _Note: this issue was identified against the code base at commit `6b20299dda36b610e0eb6266197358af63fda05b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCommandInvocation

## Description
Cause 1 of 1: an SSM Run Command invocation of AWS-RunShellScript on a running instance now reaches a terminal Status of 'Success' but carries no StandardOutputContent, so nothing the script echoes is observable and the script evidently never executes in the guest. Both guest-driven ALB target-rotation checks stop at the first assertion that reads the script's output.
AWS:      recorded behavior only for the guest leg. ssm SendCommand(DocumentName='AWS-RunShellScript', InstanceIds=[i-...], Parameters={'commands': [...]}) on an Online managed node is accepted, and GetCommandInvocation(CommandId, InstanceId) reaches Status 'Success' with ResponseCode 0 and StandardOutputContent equal to the script's stdout — here the lines the script echoes, 'UNIT-A=active', 'UNIT-B=active', 'LOCAL-A=200', 'LOCAL-B=200', produced by `systemctl enable --now`, `systemctl is-active` and a loopback `curl -o /dev/null -w '%{http_code}' http://127.0.0.1:<port>/health`. Status 'Success' is defined by the fact the commands ran and exited 0; a Success with no output for a script whose last statements are `echo` is not a state AWS produces.
Emulator: GetCommandInvocation reaches Status 'Success' (the check's `inv['Status'] == 'Success'` assertion passes) but StandardOutputContent contains none of the labelled lines — the parsed label dict is empty ({}), so the guest's two systemd units and its own loopback /health are unobservable. The checks fail at `assert deployed.get('UNIT-A') == 'active' and deployed.get('UNIT-B') == 'active'` with "the guest servers did not come up: {}".
Why:      a client cannot run anything inside an instance: a configuration push, an in-guest probe, a deployment step. Worse, the emulator reports SUCCESS for work it did not do, so automation does not retry or fail — it proceeds on the assumption that the guest is configured. Everything downstream (here: the ALB target-health verdicts that must follow from what the guest serves) is then built on a guest that was never touched.
Fix:      the SSM Run Command execution path — the SendCommand/GetCommandInvocation handlers and the instance-side execution that must actually run the document's commands and capture stdout/stderr into StandardOutputContent/StandardErrorContent (and report a non-Success Status with a ResponseCode when it cannot).
This is a NEW symptom, not the one already filed: open issue 20261110T143849Z-ec2-elbv2-ssm-the-guest-driven-alb-target.md records these two checks timing out at 600 s because the invocation never reached a terminal status; it is keyed on that timeout and the replay report leaves these failures unowned. The invocation now terminates — as Success with no output — which is a different divergence in the same leg.
Traces:   ec2__elbv2__ssm.dtgthealth-guest-health-endpoint-decides-alb-target-rotation, ec2__elbv2__ssm.dtgthealth-guest-stalled-health-response-times-out-and-withdraws-the-target.

## Evidence
#### [mismatch] ec2__elbv2__ssm.dtgthealth-guest-health-endpoint-decides-alb-target-rotation
  File "check_ec2__elbv2__ssm_dtgthealth_guest_health_endpoint_decides_alb_target_rotation.py", line 601, in run
    assert deployed.get("UNIT-A") == "active" and deployed.get("UNIT-B") == "active"
  AssertionError: the guest servers did not come up: {}; both systemd units must be 'active' before any load-balancer observation can be attributed to the guest

#### [mismatch] ec2__elbv2__ssm.dtgthealth-guest-stalled-health-response-times-out-and-withdraws-the-target
  File "check_ec2__elbv2__ssm_dtgthealth_guest_stalled_health_response_times_out_and_withdraws_the_target.py", line 611, in run
    assert deployed.get("UNIT-A") == "active" and deployed.get("UNIT-B") == "active"
  AssertionError: the guest servers did not come up: {}

The helper that produced the empty dict asserts `inv['Status'] == 'Success'` first and returns `inv['StandardOutputContent']`, so the invocation DID report Success — with no stdout. Expected: StandardOutputContent containing the script's echoed 'UNIT-A=active', 'UNIT-B=active', 'LOCAL-A=200', 'LOCAL-B=200' lines. Actual: Status 'Success', no such lines (empty parse).

## Steps to Reproduce
1. ssm GetParameter(/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64) and RunInstances a t3.micro from it with an instance profile granting AmazonSSMManagedInstanceCore; wait for instance state 'running'.
2. ssm SendCommand(DocumentName='AWS-RunShellScript', InstanceIds=[<id>], Parameters={'commands': ['echo <b64> | base64 -d > /tmp/step.sh', 'bash /tmp/step.sh']}) where the script writes two systemd units, runs `systemctl daemon-reload` and `systemctl enable --now`, polls loopback /health on both ports, then echoes `UNIT-A=$(systemctl is-active ...)`, `UNIT-B=...`, and `LOCAL-A=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:<port>/health)`.
3. Poll ssm GetCommandInvocation(CommandId, InstanceId) to a terminal Status.
4. Observe Status 'Success' with an empty StandardOutputContent — none of the echoed KEY=VALUE lines is present, so the guest script was never executed.
