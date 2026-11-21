Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.droute-deleted-peering-target-blackholes-the-route-and-drops-traffic ec2.droute-longest-prefix-match-decides-between-overlapping-route-targets ec2.srcdest-check-enforcement-gates-appliance-forwarded-traffic ec2.cpuopts-topology-survives-a-stop-start-cycle
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.droute-deleted-peering-target-blackholes-the-route-and-drops-traffic|_json_line#5|JSONDecodeError|return json.loads(lines[-1])
Issue-Key: ec2.droute-longest-prefix-match-decides-between-overlapping-route-targets|_json_line#5|JSONDecodeError|return json.loads(lines[-1])
Issue-Key: ec2.srcdest-check-enforcement-gates-appliance-forwarded-traffic|_payload_json#6|JSONDecodeError|return json.loads(lines[-1])
Issue-Key: ec2.cpuopts-topology-survives-a-stop-start-cycle|_poll#29|TimeoutError|raise TimeoutError(f"{what or 'condition'}: the run budget ended first; not graded")
Issue-Commit: bf168f2125883f6ad6bec616d1c212f3f21e9ddc

# ec2 guest data plane: AWS-RunShellScript stdout is not the script's output (JSON payload unparsable) and a restarted instance never becomes an Online SSM managed node (2 causes)

> _Note: this issue was identified against the code base at commit `bf168f2125883f6ad6bec616d1c212f3f21e9ddc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCommandInvocation

## Description
Two net-new root causes in the EC2 guest/data-plane surface that behavioral checks reach through SSM Run Command. Both are distinct from, and additional to, the open issues 20261115T003554Z-ec2-guests-observe-nothing-in-guest-commands-exit.md and 20261113T123420Z-ec2-data-plane-a-launched-guest-never-runs.md, which do not carry these checks' replay keys.

Cause 1 of 2: the stdout an AWS-RunShellScript invocation reports is not the output of the script that was sent, so a payload the script printed as a single JSON object cannot be parsed.
AWS: recorded behavior only — the EC2/SSM data plane is not described by the Smithy model beyond SSM's GetCommandInvocation.StandardOutputContent (a string). On AWS, a SendCommand of AWS-RunShellScript whose body runs an inline `python3` program that prints one JSON object on its last line ends Status=Success and StandardOutputContent's last non-blank line is exactly that JSON document; stderr carries only what the script wrote there.
Emulator: the invocation reaches a terminal status and StandardOutputContent has at least one non-blank line (the checks' `assert lines` precondition passes), but its last non-blank line is not JSON: `json.loads(lines[-1])` fails with `json.decoder.JSONDecodeError: Expecting value: line 1 column 1 (char 0)`. The guest therefore never reports the probe result the script computed (curl exit statuses, the body it fetched, the IMDS liveness control), so route-table and source/destination-check data-plane behavior cannot be observed at all.
Why: a client that drives a guest through Run Command and reads its result out of StandardOutputContent — the normal way to probe connectivity, read a config value or return a computed answer — gets text that is not what its own script printed. Every automation that parses command output (JSON, key=value, a single line) misreads or crashes, and the whole class of in-guest verification silently proves nothing.
Fix: the EC2 guest execution path that runs an SSM document's commands inside the launched instance and the SSM GetCommandInvocation serializer that reports their captured output — StandardOutputContent must be the byte-for-byte stdout of the commands as the guest ran them.
Traces: ec2.droute-deleted-peering-target-blackholes-the-route-and-drops-traffic, ec2.droute-longest-prefix-match-decides-between-overlapping-route-targets, ec2.srcdest-check-enforcement-gates-appliance-forwarded-traffic.

Cause 2 of 2: after a StopInstances/StartInstances cycle, the instance never reports an Online SSM agent, so no further in-guest step can run.
AWS: recorded behavior only — a running instance with an instance profile carrying AmazonSSMManagedInstanceCore registers as a managed node and SSM DescribeInstanceInformation reports it with PingStatus=Online; after a stop and a start, the same instance id becomes Online again within the normal boot time, and SendCommand against it is accepted.
Emulator: polling for the agent to come back Online never succeeds for the restarted instance; the check ends `TimeoutError: the SSM agent on i-... coming Online: the run budget ended first; not graded`, i.e. DescribeInstanceInformation never reports that instance Online (or omits it) for the whole budget. The instance id in that message is this run's value; the requirement is that a started instance returns to Online.
Why: a client that stops an instance to change its CPU options, instance type or volumes and then resumes managing it through Run Command/Session Manager finds the node permanently unreachable after the restart, with no error to act on — only a hang.
Fix: the EC2 instance lifecycle state machine's start path and the SSM managed-node registration it feeds (the agent-presence/ping state behind DescribeInstanceInformation), so a restarted instance re-registers and reports PingStatus=Online.
Traces: ec2.cpuopts-topology-survives-a-stop-start-cycle.

## Evidence
Cause 1 — Expected (AWS): GetCommandInvocation on a Success invocation of AWS-RunShellScript returns StandardOutputContent whose last non-blank line is the single JSON object the script printed; the check parses it with json.loads.
Actual (emulator): `json.decoder.JSONDecodeError: Expecting value: line 1 column 1 (char 0)` raised from `json.loads(lines[-1])` in the checks' `_json_line`/`_payload_json` helpers (check_ec2_droute_*.py line 87390 of the suite report, check_ec2_srcdest_*.py line 149280) — the non-blank stdout the emulator reports is not the script's JSON output. Status/stderr are whatever the emulator fabricated, not the script's.

Cause 2 — Expected (AWS): after StartInstances on a previously Online managed node, DescribeInstanceInformation lists that instance with PingStatus=Online again and SendCommand succeeds.
Actual (emulator): the poll never observes Online; `TimeoutError: the SSM agent on i-00000000000000001 coming Online: the run budget ended first; not graded` (check_ec2_cpuopts_topology_survives_a_stop_start_cycle.py line 271). The instance id is this run's value.

## Steps to Reproduce
Cause 1:
1. RunInstances an Amazon Linux 2023 t3.micro/m5.large in a VPC subnet with an instance profile that has arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore; wait for it to become an Online managed node.
2. ssm.SendCommand(DocumentName='AWS-RunShellScript', InstanceIds=[id], Parameters={'commands': ["python3 - <<'EOF'\nimport json\nprint(json.dumps({'ok': True}))\nEOF"]}).
3. Poll GetCommandInvocation until a terminal status, then take the last non-blank line of StandardOutputContent and json.loads it.
4. Expected: {'ok': True}. Actual: the last non-blank line is not JSON and json.loads raises JSONDecodeError('Expecting value: line 1 column 1 (char 0)').

Cause 2:
1. Launch the same kind of instance and wait until DescribeInstanceInformation reports PingStatus=Online.
2. StopInstances, wait for 'stopped', StartInstances, wait for 'running'.
3. Poll DescribeInstanceInformation (and/or SendCommand) for that instance id.
4. Expected: PingStatus returns to Online within the boot budget. Actual: it never does, and every in-guest step after the restart hangs until the budget ends.
