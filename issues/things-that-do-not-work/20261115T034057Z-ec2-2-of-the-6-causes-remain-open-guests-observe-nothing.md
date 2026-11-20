Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.shutdown-api-termination-disabled-rejects-terminate-but-still-permits-stop ec2.timesync-chrony-in-the-guest-locks-onto-the-link-local-amazon-time-sync-source ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds ec2.shutdown-in-guest-halt-with-stop-behavior-leaves-it-stopped-and-restartable ec2.shutdown-in-guest-halt-with-terminate-behavior-terminates-the-instance
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.shutdown-api-termination-disabled-rejects-terminate-but-still-permits-stop|run#24|ValueError|assert float(after_refusal["uptime"]) > float(before["uptime"]), (
Issue-Key: ec2.timesync-chrony-in-the-guest-locks-onto-the-link-local-amazon-time-sync-source|run#24|AssertionError|assert "waitsync_rc=0" in waitsync, (
Issue-Key: ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds|run#25|AssertionError|assert "waitsync_rc=0" in sec.get("WAITSYNC", ""), (
Issue-Key: ec2.shutdown-in-guest-halt-with-stop-behavior-leaves-it-stopped-and-restartable|_wait_stopped_without_terminating#24|Timeout|assert state not in ("shutting-down", "terminated", "__gone__"), (
Issue-Key: ec2.shutdown-in-guest-halt-with-terminate-behavior-terminates-the-instance|_wait_terminated_without_stopping#25|Timeout|return seen
Issue-Commit: 2bc939698b9a959721d42e691c2ca676c39ad8ba

# ec2: 2 of the 6 causes remain open — an in-guest command still returns no output, and a guest-initiated shutdown still moves no instance, both blocked on a guest runtime this platform does not provide

> _Split out of `issues/things-that-now-work/20261115T034057Z-ec2-6-net-new-causes-generic-maxresults-missing.md`, whose causes 1-4 are fixed. This report carries the replay keys that do NOT pass, re-filed here so they are not recorded as working. Everything below is what that fix established about them; a later fixer starts from here rather than from scratch._

## Operation
RunInstances

## Description
Two of the parent report's six causes are untouched, and both are BLOCKED rather than
unimplemented: they rest on ONE capability this emulator does not have — a guest operating
system inside a launched instance that runs a command and reports its own output — and the
second cause is downstream of the first, because the event it waits for is initiated inside
that guest. What the parent fix DID land, so a later fixer does not redo it: the per-operation
page-size refusal wording (`DescribeByoipCidrs` now answers `The MaxResults parameter cannot be
larger than 100.` through `crate::state::PageSizeWording::ParameterAgainstBound`), the
parameter-group-only `MissingParameter` refusal (`ModifyReservedInstances` answers
`TargetReservedInstancesConfigurationSet` through
`crate::state::RECORDED_PARAMETER_GROUP_ONLY_REFUSALS`), and the tag of a security-group rule a
VPC delete orphans now surviving that delete
(`ComAmazonawsEc2Amazonec2State::orphaned_security_group_rule_tags`). The parent's cause 4 (Auto
Scaling's above-max `SetDesiredCapacity` sentence) was already correct.

Cause 1 of 2: in-guest commands return no output, so nothing about a running guest can be
observed.
AWS: recorded behavior only. A launched instance runs a real guest; a command executed in it
(here `uptime`, and `chronyc waitsync` / `chronyc sources` against the link-local Amazon Time
Sync service at 169.254.169.123) writes its result to stdout, and that stdout is what the caller
reads back.
Emulator: every such command yields an empty string. `uptime` returns `''`, so the check
comparing uptime before and after a refused terminate fails with `ValueError: could not convert
string to float: ''`; `chronyc waitsync` returns `''`, reported as "`chronyc waitsync` did not
report success, so the guest never synchronised to any time source: ''". No non-empty guest
stdout is produced on any of these paths.
Why it is BLOCKED, and on what: the capability is RUNNING a command inside the instance and
reporting its real output, plus presenting the link-local time source the guest's NTP client
synchronises against. Served faithfully that needs a guest operating system with a command
channel and a running NTP client — a VM or container runtime this platform does not provide
("Containers and external processes are not available to a generated emulator yet",
`vendor/engine/src/lib.rs`). What was checked and ruled out: synthesising the output instead —
answering `uptime` from the instance's stored launch instant, or `chronyc waitsync` with
`waitsync_rc=0` — is output made to look computed for work nothing ran, and it answers every
other command wrongly while reporting success. So the honest exit for this cause is
`mark_wont_fix(platform_blocker)` naming that runtime, UNLESS a contained in-process guest
appears (a WASI runtime plus a userland image the tree may ship, or a core-provided guest
execution seam).
Traces: ec2.shutdown-api-termination-disabled-rejects-terminate-but-still-permits-stop,
ec2.timesync-chrony-in-the-guest-locks-onto-the-link-local-amazon-time-sync-source,
ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds.

Cause 2 of 2: a guest-initiated shutdown never moves the instance per
`InstanceInitiatedShutdownBehavior`.
AWS: recorded behavior only. When the guest halts itself, the instance follows the
`InstanceInitiatedShutdownBehavior` attribute: with `stop` it settles in `stopped` and can be
started again; with `terminate` it settles in `terminated`. `DescribeInstances` reflects that
final state within a client's polling budget.
Emulator: neither transition is ever produced — both checks are killed with `Check timed out
after 60.0s` without reaching a verdict, so the instance stays in its pre-shutdown state and the
attribute has no effect.
Why it is BLOCKED, and on what: the attribute is stored and read, but the EVENT that triggers it
is a halt issued INSIDE the guest — there is no API call to act on, so the lifecycle has nothing
to act on until cause 1's guest exists. Moving the instance on a timer instead (a stop or a
terminate owed by the clock rather than by a halt) is a status for work nothing performed, and it
would fire for every instance whether or not its guest ever halted. The fix therefore follows
cause 1: once a guest runs and can halt itself, that halt drives
`ComAmazonawsEc2Amazonec2State`'s instance lifecycle through the stored
`InstanceInitiatedShutdownBehavior` to `stopped` or `terminated`.
Traces: ec2.shutdown-in-guest-halt-with-stop-behavior-leaves-it-stopped-and-restartable,
ec2.shutdown-in-guest-halt-with-terminate-behavior-terminates-the-instance.

## Related
`issues/things-that-do-not-work/20261115T003554Z-ec2-guests-observe-nothing-in-guest-commands-exit.md`
records the same guest surface (in-guest execution, IMDS, guest block devices) for other checks;
its causes 1-3 are the same blocker as cause 1 here. A fix that builds the guest runtime closes
both reports' guest causes at once.

## Evidence
Replay of the parent issue's 11 units after its fix: 6 passed (its causes 1-4 —
`ec2.describe-managed-prefix-lists-contract`, `ec2.describe-network-insights-paths-contract`,
`ec2.op-contract-describe-byoip-cidrs-duplicate`,
`ec2.op-contract-modify-reserved-instances-required-params-rejected`,
`ec2.ec2-delete-vpc-removes-own-and-default-resource-tags`, and the Auto Scaling refusal unit),
5 failed, and the 5 keys above are still open. `other_causes` was empty: the parent fix caused
none of these.
* Cause 1: `ValueError: could not convert string to float: ''` at `assert
  float(after_refusal["uptime"]) > float(before["uptime"])`; "`chronyc waitsync` did not report
  success, so the guest never synchronised to any time source: ''" (twice, once with "and its
  clock agreeing with anything would be luck").
* Cause 2: `Error: Check timed out after 60.0s`, no assertion reached.

## Steps to Reproduce
1. Boot the emulator and replay the check ids in Issue-Trace-Ids.
2. Cause 1: launch an instance, wait for `running`, then run `uptime` and `chronyc waitsync` in
   the guest — stdout is empty both times, where AWS returns the command's own output.
3. Cause 2: launch an instance with `InstanceInitiatedShutdownBehavior=stop` (then
   `=terminate`), halt it from inside the guest and poll `DescribeInstances` — it never reaches
   `stopped` / `terminated`.
