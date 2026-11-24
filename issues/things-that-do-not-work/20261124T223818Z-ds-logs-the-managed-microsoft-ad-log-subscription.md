Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ds__logs.log-subscription-delivers-domain-controller-security-events-and-stops-when-deleted
Issue-Key: ds__logs.log-subscription-delivers-domain-controller-security-events-and-stops-when-deleted|_outlast_lookback#29|Timeout|time.sleep(_POLL_INTERVAL_S)
Issue-Commit: 9bfcdac351b9a65b625c204ecd025e93d3476b59

# ds__logs: the Managed Microsoft AD log-subscription forwarding scenario reaches no verdict — the check is killed at the 60 s cap before any directory/data-access/forwarding beat is graded

> _Note: this issue was identified against the code base at commit `9bfcdac351b9a65b625c204ecd025e93d3476b59`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateLogSubscription

## Description
Cause 1 of 1: the Directory Service log-forwarding trajectory (directory -> data access -> log subscription -> forwarded DC security events -> withhold on delete -> restore) produces no graded observation at all; the check is killed at the harness cap.

AWS: Smithy (directoryservice): CreateLogSubscription takes DirectoryId (required, pattern ^d-[0-9a-f]{10}$) and LogGroupName (required, length 1..512, pattern ^[-._/#A-Za-z0-9]+$) and returns an empty result; it refuses with EntityDoesNotExistException / InsufficientPermissionsException / UnsupportedOperationException (all HTTP 400) and ServiceException (500). ListLogSubscriptions returns LogSubscriptions, one entry per association, each naming the LogGroupName; DeleteLogSubscription removes it (empty result). EnableDirectoryDataAccess returns an empty result and DescribeDirectoryDataAccess then reports DataAccessStatus "Enabled". The forwarding behavior itself is not modelled — recorded behavior only: after CreateMicrosoftAD the directory settles at Stage "Active"; once a log subscription exists, domain-controller security events (the "Audit User Account Management" category, which covers a ds-data CreateUser) are delivered into the subscribed log group with the affected SAM account name in the message; events produced BEFORE the subscription existed are not transferred; after DeleteLogSubscription no further events are delivered and the group's storedBytes does not advance; re-creating the subscription resumes delivery.

Emulator: nothing in that chain is graded. The suite reports only `Error: Check timed out after 60.0s` for ds__logs.log-subscription-delivers-domain-controller-security-events-and-stops-when-deleted — no directory Stage, no DataAccessStatus, no ListLogSubscriptions content, no forwarded event, no withhold window and no restore leg. The first beats the check waits on, in order, are the directory reaching a terminal Stage, DataAccessStatus settling to Enabled, and then a forwarded DC security event naming the newly created user. Hypothesis: at least one of those states is never reached, so the run makes no progress — on an emulator CreateMicrosoftAD should settle to Active and forwarding should be observable within seconds rather than over AWS's real multi-minute provisioning window.

Why: a client that uses ds log forwarding as its audit path gets no observable: it cannot tell a directory that is still provisioning from one that will never go Active, and it cannot tell "subscription withholds events" from "there is no delivery path at all". Both the positive (delivery under a subscription) and negative (no delivery before/after the subscription) guarantees are unverifiable.

Fix: the directoryservice service state machine and its log-forwarding path — CreateMicrosoftAD/DescribeDirectories reaching a terminal Active Stage, EnableDirectoryDataAccess/DescribeDirectoryDataAccess reaching Enabled, and the CreateLogSubscription/DeleteLogSubscription handlers' delivery of domain-controller security events into the subscribed CloudWatch Logs group (gated on the group's resource policy for ds.amazonaws.com).

Traces: ds__logs.log-subscription-delivers-domain-controller-security-events-and-stops-when-deleted.

## Evidence
Expected (AWS): CreateMicrosoftAD -> DescribeDirectories Stage "Active"; EnableDirectoryDataAccess -> DescribeDirectoryDataAccess DataAccessStatus "Enabled"; CreateLogSubscription(DirectoryId, LogGroupName) -> empty 200 and ListLogSubscriptions returning exactly that LogGroupName; a ds-data CreateUser performed after subscribing appears as a DC security event naming the SAM account in the subscribed log group; a user created before subscribing never appears; after DeleteLogSubscription a newly created user never appears and storedBytes does not advance; after re-creating the subscription a new user appears again.

Actual (emulator): no verdict at all. Suite output for ds__logs.log-subscription-delivers-domain-controller-security-events-and-stops-when-deleted is exactly `[mismatch] ... Error: Check timed out after 60.0s`; no directory stage, data-access status, subscription listing or forwarded event was observed.

## Steps to Reproduce
1. ec2 DescribeVpcs/DescribeSubnets to pick a VPC with two subnets in distinct AZs.
2. ds CreateMicrosoftAD(Name=<fqdn>, ShortName=SIM2REAL, Password=..., Edition=Standard, VpcSettings={VpcId, SubnetIds}) and poll DescribeDirectories until Stage is terminal — expect "Active".
3. ds EnableDirectoryDataAccess(DirectoryId) and poll DescribeDirectoryDataAccess until DataAccessStatus == "Enabled".
4. logs CreateLogGroup(/aws/directoryservice/<name>) and PutResourcePolicy allowing Principal {"Service":"ds.amazonaws.com"} logs:CreateLogStream/logs:PutLogEvents on that group.
5. ds-data CreateUser(<preUser>) BEFORE subscribing; then ds CreateLogSubscription(DirectoryId, LogGroupName) and ListLogSubscriptions -> exactly one entry naming the group.
6. ds-data CreateUser(<postUser>) and FilterLogEvents on the group -> expect an event whose message names <postUser> and none naming <preUser>.
7. ds DeleteLogSubscription(DirectoryId); ds-data CreateUser(<heldUser>) -> expect no event naming it and DescribeLogGroups storedBytes unchanged; then CreateLogSubscription again and ds-data CreateUser(<resumedUser>) -> expect delivery to resume.
Observed: the run is killed at the 60 s harness cap with no verdict; none of steps 2-7 produce a graded observation.
