Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.attach-instance-wrong-availability-zone-rejected autoscaling.attach-instances-exceeding-max-size-rejected autoscaling.attach-nonexistent-instance-fails
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.attach-instance-wrong-availability-zone-rejected|run#15|AssertionError|assert message == (
Issue-Key: autoscaling.attach-instances-exceeding-max-size-rejected|run#18|AssertionError|assert message == (
Issue-Key: autoscaling.attach-nonexistent-instance-fails|run#14|AssertionError|assert message == "Instance [i-0123456789abcdef0] is invalid.", f"expected the exact refusal message, got {message!r}"
Issue-Commit: 240175430862b0b5864a4ab51e154eb7375de82b

# autoscaling AttachInstances: all three ValidationError refusal messages are emulator-authored prose instead of AWS's fixed text (3 net-new causes)

> _Note: this issue was identified against the code base at commit `240175430862b0b5864a4ab51e154eb7375de82b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AttachInstances

## Description
AttachInstances refuses the three negative classes below with the correct error code (ValidationError) but with message text this emulator authored itself. AWS's AttachInstances refusal messages are fixed templates whose only variable parts come from the request and from the group's stored state; a message that differs from AWS's wording is a defect, and all three differ in fixed text, not just in values.

Three distinct causes are bundled here. They are all net-new: the one already-open AttachInstances message issue, 20261116T071726Z-autoscaling-attachinstances-group-not-found-refusal-emits-null.md, covers only the group-NOT-FOUND clause (ABSENT_GROUP_MESSAGE_WITHOUT_DETAIL) and none of the three below. A fixer must close all 3 before submitting.

--- Cause 1 of 3: Availability-Zone-mismatch refusal is re-worded ---
AWS:      recorded behavior only (the Smithy model declares no ValidationError for AttachInstances — only ResourceContentionFault (HTTP 500) and ServiceLinkedRoleFailure (HTTP 500) — and the operation's doc prose does not state the AZ rule, so the refusal class and wording are established by the recording). AttachInstances refuses an instance whose Availability Zone is not one of the target group's configured AvailabilityZones with HTTP 400, error code `ValidationError`, and the message template:
            `The list of availability zones [<group AvailabilityZones, comma-joined>] for the group <AutoScalingGroupName> does not contain the zone occupied by the instance <InstanceId> [<instance's AZ>]`
          Fixed text: "The list of availability zones [", "] for the group ", " does not contain the zone occupied by the instance ", " [", "]". Variable parts: the group's stored AvailabilityZones, the AutoScalingGroupName from the request, the InstanceId from the request, and the instance's AZ read from EC2 state. The instance is not added to the group.
Emulator: refuses with ValidationError but emits its own sentence, e.g. `Instance i-00000000000000001 is in Availability Zone us-west-2b, which is not one of the Availability Zones of Auto Scaling group sim2real-behav-asg-95fa274cacde.` — a different sentence structure, a trailing period AWS does not emit, and it never names the group's zone list at all.
Why:      clients and support tooling that match on this refusal (log scrapers, retry/remediation wrappers, test assertions on the exact string, and operators reading the zone list out of the message to pick a correct instance) see a string AWS never produces, and the group's configured zone list — the actionable half of AWS's message — is missing.
Fix:      the AttachInstances refusal-message rendering, i.e. the AZ-mismatch variant of the refusal type returned by `crate::state::attach_instances` and formatted by `refusal.message(&name)`.
Traces:   autoscaling.attach-instance-wrong-availability-zone-rejected

--- Cause 2 of 3: the capacity-bound refusal uses SetDesiredCapacity's wording, not AttachInstances' ---
AWS:      recorded behavior only for the wording; the operation's modeled doc states the rule ("If the number of instances being attached plus the desired capacity of the group exceeds the maximum size of the group, the operation fails"). The refusal is HTTP 400, error code `ValidationError`, message template:
            `AutoScalingGroup <AutoScalingGroupName> has min-size=<MinSize>, max-size=<MaxSize>, and desired-size=<DesiredCapacity>. To attach <count of InstanceIds in the request> instance, please update the AutoScalingGroup sizes appropriately.`
          Fixed text: "AutoScalingGroup ", " has min-size=", ", max-size=", ", and desired-size=", ". To attach ", " instance, please update the AutoScalingGroup sizes appropriately.". Variable parts: the group name from the request, the group's stored MinSize/MaxSize/DesiredCapacity, and the number of instance ids the request names. No instance is attached.
Emulator: answers ValidationError with `AutoScalingGroup sim2real-behav-asg-2789f8b8bd73 has min-size=0, max-size=0, desired-size=0. New desired-size=1 must be between min-size and max-size.` — it drops the ", and" before desired-size and replaces AWS's second sentence with the group-capacity-change wording ("New desired-size=N must be between min-size and max-size."), which is the SetDesiredCapacity/UpdateAutoScalingGroup refusal, not the attach refusal. The attach-specific remediation clause and the attached-instance count are absent.
Why:      the message no longer tells a client that an ATTACH was refused or how many instances it tried to attach; automation that distinguishes an attach-capacity refusal from a desired-capacity refusal by message text mis-classifies the failure, and the emulator's string cannot be matched by callers written against AWS.
Fix:      the AttachInstances refusal-message rendering — the capacity-bound variant must render the attach template rather than reusing the group-capacity-change message; same component as Cause 1.
Traces:   autoscaling.attach-instances-exceeding-max-size-rejected

--- Cause 3 of 3: the nonexistent-instance refusal uses an EC2-style sentence instead of AWS's "Instance [<id>] is invalid." ---
AWS:      recorded behavior only (no modeled ValidationError; see Cause 1). AttachInstances refuses an instance id that does not exist with HTTP 400, error code `ValidationError`, message template:
            `Instance [<InstanceId>] is invalid.`
          Fixed text: "Instance [", "] is invalid." — including the square brackets and the trailing period. Variable part: the instance id taken from the request's InstanceIds. The group is left unchanged.
Emulator: answers ValidationError with `The instance ID 'i-0123456789abcdef0' does not exist` — EC2's `InvalidInstanceID.NotFound` phrasing (single quotes, no brackets, no trailing period) rather than Auto Scaling's own fixed text.
Why:      a client that matches Auto Scaling's refusal text, or that routes on it to distinguish "bad id handed to Auto Scaling" from an EC2-side failure, never sees the string AWS returns; the two services' refusals become indistinguishable.
Fix:      the AttachInstances refusal-message rendering — the unknown-instance variant must render Auto Scaling's own fixed text instead of forwarding the EC2 store's not-found prose; same component as Causes 1 and 2.
Traces:   autoscaling.attach-nonexistent-instance-fails

All three refusals are produced on the same path: services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/ops/attach_instances.rs lines 217-221 map every failure of `crate::state::attach_instances` through `crate::scaffolded::Scaffolded::validation_error(refusal.message(&name))`, so the message templates live in that refusal type's `message` rendering in the crate's `state` module.

## Evidence
Cause 1 — autoscaling.attach-instance-wrong-availability-zone-rejected (verdict: mismatch)
  code: ValidationError (matches AWS)
  expected message: "The list of availability zones [<az_in>] for the group <asg_name> does not contain the zone occupied by the instance <instance_id> [<az_out>]"
  actual message:   'Instance i-00000000000000001 is in Availability Zone us-west-2b, which is not one of the Availability Zones of Auto Scaling group sim2real-behav-asg-95fa274cacde.'
  assertion: AssertionError: expected the exact refusal message, got 'Instance i-00000000000000001 is in Availability Zone us-west-2b, which is not one of the Availability Zones of Auto Scaling group sim2real-behav-asg-95fa274cacde.'

Cause 2 — autoscaling.attach-instances-exceeding-max-size-rejected (verdict: mismatch)
  code: ValidationError (matches AWS)
  expected message: "AutoScalingGroup <asg_name> has min-size=0, max-size=0, and desired-size=0. To attach 1 instance, please update the AutoScalingGroup sizes appropriately."
  actual message:   'AutoScalingGroup sim2real-behav-asg-2789f8b8bd73 has min-size=0, max-size=0, desired-size=0. New desired-size=1 must be between min-size and max-size.'
  assertion: AssertionError: expected the exact refusal message, got 'AutoScalingGroup sim2real-behav-asg-2789f8b8bd73 has min-size=0, max-size=0, desired-size=0. New desired-size=1 must be between min-size and max-size.'

Cause 3 — autoscaling.attach-nonexistent-instance-fails (verdict: mismatch)
  code: ValidationError (matches AWS)
  expected message: "Instance [i-0123456789abcdef0] is invalid."
  actual message:   "The instance ID 'i-0123456789abcdef0' does not exist"
  assertion: AssertionError: expected the exact refusal message, got "The instance ID 'i-0123456789abcdef0' does not exist"

(The group names and instance ids above are per-run values that fill the templates; only the fixed text and the template's variable slots are the requirement.)

## Steps to Reproduce
Cause 1:
1. Create a launch template, then CreateAutoScalingGroup with MinSize=0, MaxSize=2, DesiredCapacity=0 and AvailabilityZones=[AZ-A] (VPCZoneIdentifier = the subnet in AZ-A).
2. RunInstances one instance in AZ-B (a different available AZ) and wait for it to be running.
3. Call AttachInstances(AutoScalingGroupName=<group>, InstanceIds=[<AZ-B instance>]).
4. Observe: ValidationError with the emulator's own sentence instead of "The list of availability zones [AZ-A] for the group <group> does not contain the zone occupied by the instance <id> [AZ-B]".

Cause 2:
1. CreateAutoScalingGroup with MinSize=0, MaxSize=0, DesiredCapacity=0 and a valid launch template.
2. RunInstances one instance in the group's subnet and wait for it to be running.
3. Call AttachInstances(AutoScalingGroupName=<group>, InstanceIds=[<id>]).
4. Observe: ValidationError whose second sentence is "New desired-size=1 must be between min-size and max-size." instead of "To attach 1 instance, please update the AutoScalingGroup sizes appropriately.", and whose first sentence omits the ", and" before desired-size.

Cause 3:
1. CreateAutoScalingGroup with MinSize=0, MaxSize=1, DesiredCapacity=0 and a valid launch template.
2. Call AttachInstances(AutoScalingGroupName=<group>, InstanceIds=["i-0123456789abcdef0"]) (an id no instance has).
3. Observe: ValidationError with "The instance ID 'i-0123456789abcdef0' does not exist" instead of "Instance [i-0123456789abcdef0] is invalid.".
