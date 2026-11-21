Issue-Source: trace-sweep-divergence
Issue-Trace: asg-err-scheduled-action-validation
Issue-Step: task#10
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: asg-err-scheduled-action-validation autoscaling-policy-schedule-hook-notification-tag-lifecycle autoscaling-scheduled-actions-on-nonexistent-group autoscaling-group-and-policy-dependent-operations-on-nonexistent-group autoscaling-instance-level-operations-negative autoscaling-launch-config-and-account-level-policy-reads autoscaling-group-configuration-and-topology-negative autoscaling-group-dependent-operations-on-nonexistent-group autoscaling-pagination-invalid-next-token-errors
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling|PutScheduledUpdateGroupAction|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|ExecutePolicy|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|DeletePolicy|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|AttachTrafficSources|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|DescribeScheduledActions|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 57917fbf623074cc27ff2073937536de1df2e679

# autoscaling refusal messages and validation precedence diverge on scheduled-action, policy and traffic-source paths (5 causes)

> _Note: this issue was identified against the code base at commit `57917fbf623074cc27ff2073937536de1df2e679`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutScheduledUpdateGroupAction

## Description
Five net-new causes, all in the autoscaling Query-protocol refusal plane. Every one answers the correct HTTP status (400) and, where observable, the correct error code; what diverges is the <Message> text AWS fixes for that error class, and in one case which of two checks runs first. Each cause must be closed separately.

Cause 1 of 5 — PutScheduledUpdateGroupAction: invalid TimeZone message is emulator-authored.
AWS: Smithy: PutScheduledUpdateGroupAction.TimeZone -> XmlStringMaxLen255 (@length 1..255), no enum or pattern restricting it to IANA names, so the IANA check is a server-side validation whose refusal is recorded behavior only. The recording shows HTTP 400, awsQueryError code ValidationError, with the entirely fixed sentence 'Time zone specified does not conform to standardized time zones found in IANA Time Zone Database' — it has no variable parts at all: the rejected zone name is NOT interpolated.
Emulator: 400 with 'The TimeZone value <zone> is not valid: it must be a canonical name of the IANA time zone database' — different fixed text, and it interpolates the request's TimeZone value into a message AWS keeps value-free.
Why: a client that matches on AWS's sentence (or compares it against a recorded golden) does not recognize the refusal; a client that pattern-matches the zone out of the message gets a field AWS never emits.
Fix: the PutScheduledUpdateGroupAction handler's TimeZone validation — emit AWS's fixed sentence with no interpolation.
Traces: asg-err-scheduled-action-validation#task#10; also autoscaling-policy-schedule-hook-notification-tag-lifecycle#task#7, autoscaling-scheduled-actions-on-nonexistent-group#task#1.

Cause 2 of 5 — ExecutePolicy: a policy named by name with no AutoScalingGroupName is reported as not found.
AWS: Smithy: ExecutePolicy.PolicyName -> ResourceName, required; ExecutePolicy.AutoScalingGroupName -> XmlStringMaxLen255, optional. AWS accepts PolicyName as either a policy ARN or a bare policy name, and when a bare name is given the group must also be given; recorded behavior for the violation is HTTP 400 ValidationError with the fixed sentence 'The AutoScaling Group name is required when specifying a policy by its name' (no variable parts).
Emulator: 400 with 'Policy name not found - <policy-name>', i.e. it skips the argument-combination check, goes straight to a lookup it cannot perform without a group, and reports the request's policy name as nonexistent.
Why: the client is told the policy does not exist when the real fault is a missing AutoScalingGroupName, so a caller retries or creates a policy that already exists instead of adding the group argument; the message also leaks a not-found claim the service never makes.
Fix: ExecutePolicy's input validation, before the policy lookup in state.
Traces: autoscaling-group-and-policy-dependent-operations-on-nonexistent-group#task#11; also autoscaling-instance-level-operations-negative#task#7.

Cause 3 of 5 — DeletePolicy: same missing-group argument check, same wrong refusal.
AWS: Smithy: DeletePolicy.PolicyName -> ResourceName, required; DeletePolicy.AutoScalingGroupName optional. Recorded behavior: HTTP 400 ValidationError, fixed sentence 'The AutoScaling Group name is required when specifying a policy by its name'.
Emulator: 400 with 'Policy name not found - <policy-name>'.
Why: identical to cause 2 — a well-formed-but-incomplete request is misreported as a missing resource, so delete-if-exists logic treats the call as already satisfied and the policy survives.
Fix: DeletePolicy's input validation, before the policy lookup. (The open issue 20261111T093414Z-autoscaling-cloudwatch-ec2-deletepolicy-answers-http-501-notimplemented.md and the DeletePolicy stub issue 20261111T072954Z-autoscaling-deletepolicy-stub.md concern DeletePolicy answering 501 on other paths; this step reaches a real 400 handler and is a distinct, uncovered defect.)
Traces: autoscaling-launch-config-and-account-level-policy-reads#task#7; also autoscaling-policy-schedule-hook-notification-tag-lifecycle#task#6.

Cause 4 of 5 — AttachTrafficSources: target-source validation pre-empts the group-not-found check.
AWS: Smithy: AttachTrafficSources.AutoScalingGroupName and .TrafficSources are both required. AWS validates the group reference first: for a request naming a group that does not exist, recorded behavior is HTTP 400 ValidationError with 'AutoScalingGroup name not found - null' — fixed text whose trailing token is literally 'null' on this operation (AWS does not substitute the requested group name here).
Emulator: 400 with 'Provided Target Groups may not be valid. Please ensure they exist and try again.' — it validates the traffic sources first and never reports the missing group, so the refusal names the wrong subject.
Why: a caller is pointed at its target-group ARNs when the actual fault is the group name, and error-handling that keys off AWS's group-not-found sentence (e.g. create-group-then-retry) never fires.
Fix: AttachTrafficSources' handler — order the group lookup before traffic-source validation, and emit AWS's group-not-found sentence. Related but non-covering open issues: 20261114T180324Z-autoscaling-group-scoped-not-found-refusals-re-word.md and 20261116T071726Z-autoscaling-attachinstances-group-not-found-refusal-emits-null.md (AttachInstances, not AttachTrafficSources).
Traces: autoscaling-group-configuration-and-topology-negative#task#13; also autoscaling-group-dependent-operations-on-nonexistent-group#task#15.

Cause 5 of 5 — DescribeScheduledActions: InvalidNextToken message uses the wrong tense.
AWS: Smithy: DescribeScheduledActions errors include InvalidNextToken (errorKind client, awsQueryError code InvalidNextToken, httpResponseCode 400); input NextToken -> XmlString. Recorded behavior: 400 with the message "The token '<token>' is invalid" — fixed text 'The token ' + the NextToken value from the request, quoted, + "' is invalid".
Emulator: 400 with "The token '<token>' was invalid" — same variable part, wrong fixed text ('was' for 'is').
Why: pagination clients and recorded-golden comparisons that match AWS's sentence do not recognize the refusal.
Fix: the shared NextToken validation used by the autoscaling describe family (its message template), not this one reader. A sibling defect on DescribeWarmPool's invalid-NextToken path is noted in 20261113T130916Z-autoscaling-2-net-new-causes-a-scale-out.md; that issue does not carry this step's replay key.
Traces: autoscaling-pagination-invalid-next-token-errors#task#5.

## Evidence
Cause 1 — fixture asg-err-scheduled-action-validation step task#8/task#10, PutScheduledUpdateGroupAction, status 400 expected and 400 got; $.ErrorResponse.Error.Message expected 'Time zone specified does not conform to standardized time zones found in IANA Time Zone Database', got 'The TimeZone value Mars/Olympus_Mons is not valid: it must be a canonical name of the IANA time zone database' (3 steps in cluster).
Cause 2 — fixture autoscaling-group-and-policy-dependent-operations-on-nonexistent-group step task#11, ExecutePolicy, 400 vs 400; Message expected 'The AutoScaling Group name is required when specifying a policy by its name', got 'Policy name not found - sim2real-probe-autoscaling-policy-f393fafd' (2 steps).
Cause 3 — fixture autoscaling-launch-config-and-account-level-policy-reads step task#7, DeletePolicy, 400 vs 400; Message expected 'The AutoScaling Group name is required when specifying a policy by its name', got 'Policy name not found - sim2real-probe-autoscaling-policy-a52781fa' (2 steps).
Cause 4 — fixture autoscaling-group-configuration-and-topology-negative step task#13, AttachTrafficSources, 400 vs 400; Message expected 'AutoScalingGroup name not found - null', got 'Provided Target Groups may not be valid. Please ensure they exist and try again.' (2 steps).
Cause 5 — fixture autoscaling-pagination-invalid-next-token-errors step task#5, DescribeScheduledActions, 400 vs 400; Message expected "The token 'sim2real-probe-invalid-next-token' is invalid", got "The token 'sim2real-probe-invalid-next-token' was invalid".
The quoted zone names, policy names and token values above are one recording's request values filling the template; only the fixed text and the templated positions are the requirement.

## Steps to Reproduce
1. Replay the wire corpus (trace set simparity-wire-aws-v1) against the emulator.
2. Cause 1: fixture asg-err-scheduled-action-validation, step task#10 — PutScheduledUpdateGroupAction with TimeZone set to a non-IANA zone; read ErrorResponse.Error.Message.
3. Cause 2: fixture autoscaling-group-and-policy-dependent-operations-on-nonexistent-group, step task#11 — ExecutePolicy with PolicyName only (a bare name, no AutoScalingGroupName).
4. Cause 3: fixture autoscaling-launch-config-and-account-level-policy-reads, step task#7 — DeletePolicy with PolicyName only.
5. Cause 4: fixture autoscaling-group-configuration-and-topology-negative, step task#13 — AttachTrafficSources naming a group that does not exist plus traffic sources.
6. Cause 5: fixture autoscaling-pagination-invalid-next-token-errors, step task#5 — DescribeScheduledActions with a syntactically bogus NextToken.
In each case compare ErrorResponse.Error.Message against the fixture's expected message.
