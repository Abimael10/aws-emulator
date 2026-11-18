Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ds__logs.log-subscription-delivers-domain-controller-security-events-and-stops-when-deleted
Issue-Key: ds__logs.log-subscription-delivers-domain-controller-security-events-and-stops-when-deleted|_outlast_lookback#29|Timeout|time.sleep(_POLL_INTERVAL_S)
Issue-Commit: c208dffe0dd114dceb7c782194c5df9ad164c45a

# ds__logs: the Directory Service log-subscription delivery scenario reaches no verdict — the run is killed at its budget with no assertion reached

> _Note: this issue was identified against the code base at commit `c208dffe0dd114dceb7c782194c5df9ad164c45a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMicrosoftAD

## Description
One root cause (Cause 1 of 1), grounded in the one failing ds__logs check the replay report binds to no open issue.

Cause 1 of 1: the Directory Service log-subscription scenario never becomes observable — the run exhausts its whole per-check budget with no assertion reached, so nothing about CreateLogSubscription forwarding domain-controller security events to CloudWatch Logs (or stopping when the subscription is deleted, or resuming when it is re-created) is measured.

AWS: recorded behavior only for the timing (the Smithy model declares no latency). Against real AWS the scenario completes: CreateMicrosoftAD answers a DirectoryId, DescribeDirectories then reports a terminal Stage ('Active', or one of Inoperable/Impaired/Failed/Deleted), CreateLogSubscription(DirectoryId, LogGroupName) succeeds and DescribeLogSubscriptions lists the association, a ds-data CreateUser against that directory produces a domain-controller security event that arrives in the subscribed log group, users created before subscribing or while the subscription is deleted never arrive, and re-subscribing restores delivery.
Emulator: the check is terminated by the harness with `Error: Check timed out after 60.0s` — the only detail the replay carries. No step-level assertion was reached. Hypothesis (neither the model nor the recording states where it stalls): the run is consumed by its first observable wait, the DescribeDirectories poll for a terminal Stage after CreateMicrosoftAD (the check's own ceiling is 3600 s at a 2 s interval), so the log subscription is never created and no delivery is ever read. An emulator must settle a directory's creation Stage inside a client's budget rather than in AWS's 20-45 minute provisioning time.
Why: a client cannot verify that Directory Service forwards domain-controller security events to the log group it subscribed, nor that deleting the subscription stops delivery — the whole logging feature is unobservable, and a directory stuck short of a terminal Stage is indistinguishable from a log subscription that silently delivers nothing.
Fix: the ds (Directory Service) state machine — the CreateMicrosoftAD lifecycle that must reach a terminal Stage promptly — and the log-subscription delivery path that writes domain-controller security events into the subscribed CloudWatch Logs group and stops on DeleteLogSubscription.
Traces: ds__logs.log-subscription-delivers-domain-controller-security-events-and-stops-when-deleted.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout: ds__logs 0/1 (1 failed).

ds__logs.log-subscription-delivers-domain-controller-security-events-and-stops-when-deleted [mismatch]
Error: Check timed out after 60.0s

Expected (AWS): CreateMicrosoftAD -> DescribeDirectories reports a terminal Stage; CreateLogSubscription succeeds and DescribeLogSubscriptions lists it; a ds-data CreateUser then appears as a domain-controller security event in the subscribed log group (readable with FilterLogEvents), a user created before the subscription does not, DeleteLogSubscription stops delivery, and a re-created subscription resumes it.
Actual (emulator): none of those observations is made — the run is killed before any of them. The 60 s figure is this run's harness budget and the directory FQDN, user names and log-group name are per-run values; the requirement is that the directory reaches a terminal Stage and the subscription's delivery is observable within a client's budget.

## Steps to Reproduce
1. Boot the emulator from this checkout and run suite simparity-behavioral-aws-v1; observe ds__logs.log-subscription-delivers-domain-controller-security-events-and-stops-when-deleted end as 'Check timed out'.
2. Minimal reproduction: pick a VPC with two subnets; CreateMicrosoftAD(Name=<fqdn>, ShortName, Password, Edition='Standard', VpcSettings) and poll DescribeDirectories for DirectoryDescriptions[0].Stage — expect a terminal Stage ('Active') within seconds on an emulator, not after a provisioning window.
3. CreateLogGroup and PutResourcePolicy granting ds delivery; CreateLogSubscription(DirectoryId, LogGroupName); DescribeLogSubscriptions — expect the association.
4. Create a user through the ds-data endpoint, then FilterLogEvents on the group — expect a domain-controller security event naming that user; DeleteLogSubscription and create another user — expect no new event; CreateLogSubscription again — expect delivery to resume.
5. Re-replay the check; it must reach those assertions instead of being killed at its budget.
