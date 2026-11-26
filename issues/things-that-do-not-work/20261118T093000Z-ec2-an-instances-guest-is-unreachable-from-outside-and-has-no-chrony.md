Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.ec2-elastic-ip-association-moves-which-address-serves-the-instance ec2.ec2-security-group-ingress-decides-whether-the-instance-answers ec2.timesync-chrony-in-the-guest-locks-onto-the-link-local-amazon-time-sync-source ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds ec2.hibernate-restores-the-running-process-and-a-plain-stop-does-not
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.ec2-elastic-ip-association-moves-which-address-serves-the-instance|_poll_until#13|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Key: ec2.ec2-security-group-ingress-decides-whether-the-instance-answers|_poll_until#15|Timeout|time.sleep(interval_s)
Issue-Key: ec2.timesync-chrony-in-the-guest-locks-onto-the-link-local-amazon-time-sync-source|_poll_until#13|Timeout|time.sleep(interval_s)
Issue-Key: ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds|_poll_until#13|Timeout|time.sleep(interval_s)
Issue-Commit: cdd06381ed07ce191d4321618e22d6aefb363f32

# ec2: an instance's guest cannot be reached at the address the control plane publishes for it, and the guest has no chrony, so the Elastic-IP, security-group-ingress and Amazon-Time-Sync scenarios never reach a verdict

> _Note: these causes were identified against the code base at commit `cdd06381ed07ce191d4321618e22d6aefb363f32`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

> _Provenance: these were causes 5, 6 and 7 of
> `20261111T171305Z-ec2-7-net-new-causes-a-stopped-instance.md`. Causes 1, 2, 3 and 4 of that
> report are fixed (a stopped instance releases its mapped public IPv4, `PublicDnsName` is gated
> on the VPC's `enableDnsHostnames`, `RunInstances` carries `Ebs.Encrypted` into the created
> volume, and `GetDeclarativePoliciesReportSummary`'s not-found refusal carries AWS's fixed text)
> and that report now sits under `issues/things-that-now-work/`; these three causes are unfixed
> and are re-filed here so they are not recorded as working. Their `Issue-Key`s moved with them._

> _`ec2.hibernate-restores-the-running-process-and-a-plain-stop-does-not` is listed in the trace
> ids but carries no `Issue-Key` here: the cause that report filed against it (the root volume
> reporting `Encrypted: false`) is FIXED, and the unit now diverges later, on cause 1 below — it
> reads the hibernated process back through a guest agent at `<the instance's public IPv4>:8080`.
> It is named so whoever fixes cause 1 knows a third check depends on the same capability._

## Operation
RunInstances, DescribeInstances, AssociateAddress, AuthorizeSecurityGroupIngress (the instance data plane behind them), and ssm SendCommand / GetCommandInvocation against an instance's guest

## Description
Two net-new root causes, numbered below.

=== Cause 1 of 2: nothing of this emulator answers a connection to the address an instance publishes, on the port its guest listens on ===
AWS: recorded behavior only. A launched instance's user data brings up a listener (TCP 80 in both scenarios, 8080 for the hibernation check's agent), and a client outside the instance then connects to the address the control plane reports for it — `Instance$publicIpAddress`, or the `Address$publicIp` of an Elastic IP associated with it — and is served by the program inside the guest. Which address serves the instance is exactly what `AssociateAddress`/`DisassociateAddress` move, and whether the connection completes at all is what a security group's ingress rules decide.
Emulator: the control plane is served and the guest data plane EXISTS in process — the boot program is performed, the server it starts is recorded, and what that server answers is computed from the guest's own tree (`services/service-com-amazonaws-ec2-amazonec2/src/state/guest_console.rs`, `guest_services.rs`, `guest_http_client.rs`), with security-group admission already modelled (`.../src/state/security_group_admission.rs`). What is missing is an ADDRESS a client can dial: the address published is a minted public IPv4 out of `54.0.0.0/8` (`.../src/state/elastic_ips_and_transit_gateway_route_tables.rs`, `mint_elastic_ip`; `.../src/state/launched_interfaces.rs`, `assign_amazon_public_ipv4`) that nothing listens on and nothing resolves to this emulator, so the replay refuses it before any packet is sent: `PublishedUrlRejected: endpoint_host was given 54.0.0.2:80 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]: the endpoint host, a documented own host or, for a loopback endpoint, a loopback address`. The `54.0.0.2` and the ports of any one run are per-run values, not the requirement; the requirement is that the published address be one this emulator serves.
Why: a client cannot verify that associating or re-associating an Elastic IP changes which address serves the instance — the behaviour Elastic IP exists for — and cannot confirm that authorizing or revoking ingress changes whether the instance answers, which is the single most relied-on EC2 network behaviour. Every data-plane consequence of `AssociateAddress`, `DisassociateAddress`, `AuthorizeSecurityGroupIngress` and `RevokeSecurityGroupIngress` is unobservable, and a scenario that depends on a group actually blocking traffic silently proves nothing.
THE ONE BLOCKER, for whoever picks this up: the core's listener API vends a socket on the operator's ONE bind address with a port the OS or `EMULATOR_LISTENER_PORT_RANGE` picks (`vendor/server/src/listeners.rs`, `bind_std`/`bind_tokio` lines 67-107 and `Listeners::tcp_plaintext` line 230), so no service can ask to answer at a given address on a given port — and the modelled members an instance's address is published in (`Instance$publicIpAddress`, `Address$publicIp`) are bare IPv4 addresses with no port member to report a core-assigned port in, so the address cannot carry one either (and gluing `host:port` into them could not represent a guest's second port). The core change this needs is a listener entry point taking a caller-named bind address and port — a loopback alias at the guest's own listening port — after which this crate can open one per listening guest port, publish that address as the instance's and gate the connection on the security groups it already models. `vendor/` is read-only from inside the emulator, so this is a vendor blocker until that entry point exists.
Traces: ec2.ec2-elastic-ip-association-moves-which-address-serves-the-instance, ec2.ec2-security-group-ingress-decides-whether-the-instance-answers (and the remaining leg of ec2.hibernate-restores-the-running-process-and-a-plain-stop-does-not).

=== Cause 2 of 2: the guest has no chrony, so the Amazon Time Sync scenarios never reach a verdict ===
AWS: documented behavior — the Amazon Time Sync Service is reachable from every instance at the link-local address `169.254.169.123`, chrony in an AL2023 guest selects it as its source (`chronyc` reports the peer and the reference id `A9FEA97B`, the hex form of that address), and the guest's clock tracks NTP time closely enough that it agrees with the `Date` header of an AWS API response to within a couple of seconds.
Emulator: both checks run entirely through Systems Manager Run Command over the in-process guest session (`.../src/state/guest_commands.rs`), and every line of the script they deliver that reads the guest's time state runs nothing: the session has no `chronyc` and no `awk`, so the invocation answers with the diagnostic a shell writes for a utility it cannot find and the checks' `StandardErrorContent == ""` assertion fails — `sh: chronyc: command not found`, `sh: awk: command not found`. The second check additionally needs a `date` form that PARSES an HTTP date (the guest's `date` performs `-d @<epoch>` only — `.../src/state/guest_clock.rs`, `epoch_secs_of`) and a `curl` of an AWS endpoint (the guest's client performs the metadata address and the guest's own loopback only — `.../src/state/guest_http_client.rs`, `::service_common::instance_metadata_service`).
Why: a client that relies on the link-local time source — certificate validity windows, SigV4 request signing, any clock-sensitive workload inside the guest — has no way to see that the emulator's guests get time at all, and a guest whose clock was wrong would skew SigV4 signing without the suite ever noticing.
This one is NOT blocked by the core: it is in-process work. What it needs is (a) a chrony state for the guest, computed from the clock this emulator already serves it (`.../src/state/guest_clock.rs` reads the process clock), reported in `chronyc`'s own output forms for the source at `169.254.169.123`; (b) `awk` — through a maintained crate rather than a hand-written subset — beside the other text filters in `services/common/src/posix_shell_text_filters.rs`, which today performs none (its table says so at the `"a utility this module has none of"` row); and (c) for the second check, a `date -d` that parses the date forms `date(1)` documents and a guest `curl` that can reach this emulator's own endpoint.
Traces: ec2.timesync-chrony-in-the-guest-locks-onto-the-link-local-amazon-time-sync-source, ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds

## Evidence
Cause 1 — expected: a connection to the address the instance publishes, on the port its guest listens on, is served by the guest's program (and refused while no ingress rule admits it). actual, for all three checks: `PublishedUrlRejected: endpoint_host was given <published address>:<port> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]: the endpoint host, a documented own host or, for a loopback endpoint, a loopback address` — raised before any connection is attempted, so neither the admitted nor the refused leg is ever graded.

Cause 2 — expected: the guest script writes nothing to standard error, `chronyc` reports the source `169.254.169.123` (reference id `A9FEA97B`) and the guest clock agrees with an API response's `Date` header within 2 s. actual: `AssertionError: the guest script wrote to stderr: 'sh: chronyc: command not found\nsh: chronyc: command not found\nsh: awk: command not found\nsh: chronyc: command not found\nsh: chronyc: command not found\n'` for the chrony check, and the same with `sh: curl: command not found\nsh: date: command not found` added for the clock check.

## Steps to Reproduce
Replay the behavioral suite and re-run exactly the checks in the trace ids, or by hand:
1. ec2.ec2-elastic-ip-association-moves-which-address-serves-the-instance — launch an instance whose user data brings up a listener, poll its console output for the listener state, then associate and re-associate an Elastic IP and probe which address serves it.
2. ec2.ec2-security-group-ingress-decides-whether-the-instance-answers — launch a t3.micro from the AL2023 SSM AMI with a listener on TCP 80 and probe that port from outside with and without an authorizing ingress rule.
3. ec2.timesync-chrony-in-the-guest-locks-onto-the-link-local-amazon-time-sync-source and ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds — launch an AL2023 guest with the SSM core profile, read `chronyc sources`/`tracking` through Run Command and assert the source `169.254.169.123` / reference id `A9FEA97B`, then compare the guest clock to an API response's `Date` header.
