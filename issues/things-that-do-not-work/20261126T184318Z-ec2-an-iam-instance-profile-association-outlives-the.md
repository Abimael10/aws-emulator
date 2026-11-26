Issue-Source: trace-sweep-divergence
Issue-Trace: iam-instance-profile-explicit-path-tags-launches-an-instance-delete-refused-with-role-and-not-found
Issue-Step: task#25
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iam-instance-profile-explicit-path-tags-launches-an-instance-delete-refused-with-role-and-not-found
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: ec2|DescribeIamInstanceProfileAssociations|2xx->2xx|$
Issue-Key: ec2|DescribeIamInstanceProfileAssociations|2xx->2xx|$.DescribeIamInstanceProfileAssociationsResponse.iamInstanceProfileAssociationSet
Issue-Commit: 3a71fbf666403ef651a16a3f891bc8ef488168a9

# ec2: an IAM instance profile association outlives the instance it names, so its association set never empties

> _Note: this issue was identified against the code base at commit `3a71fbf666403ef651a16a3f891bc8ef488168a9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeIamInstanceProfileAssociations

## Description
ONE net-new cause in the ec2 service (one unowned wire cluster, stub_path "", owned_by []).

=== Cause 1 of 1: an IAM instance profile association is never taken away except by an explicit DisassociateIamInstanceProfile, so it is still reported `associated` after the instance it names has ended ===

AWS: Smithy com.amazonaws.ec2#DescribeIamInstanceProfileAssociations -> input AssociationIds (AssociationIdList, xmlName AssociationId), Filters, MaxResults (@range 5-1000), NextToken; output IamInstanceProfileAssociations (xmlName iamInstanceProfileAssociationSet), NextToken; the operation models NO errors and is paginated by NextToken. The reported shape is com.amazonaws.ec2#IamInstanceProfileAssociation {associationId, instanceId, iamInstanceProfile, state, timestamp}, and com.amazonaws.ec2#IamInstanceProfileAssociationState enumerates `associating | associated | disassociating | disassociated`. The operation's own Filters documentation lists the selectable `state` values as only "associating | associated | disassociating" — i.e. an association that has finished its life is NOT reported as a terminal row at all; AWS reports it as ABSENT. An association's life therefore ends whenever the thing it binds ends, not only when DisassociateIamInstanceProfile is called: once the instance the association names has gone (terminated), AWS has no association to report and iamInstanceProfileAssociationSet for that instance becomes EMPTY. The requirement is the convergence, not any one id: after the association has ended, the set must be empty and stay empty. Hypothesis (not stated by the model): in this recording the association also names an instance profile the fixture tears down, and AWS likewise reports no association for a profile that no longer exists.
Emulator: status 200 (the status is right), but the set never empties. The step is a POLL and it ran out: "poll capped at 180s", "poll: not in [''] after 19 attempt(s)" — every attempt answered the same single association, still carrying state `associated`, with its associationId, instanceId and iamInstanceProfile members intact. The harness reports it as both a type_mismatch (expected an empty iamInstanceProfileAssociationSet, got a set with one item) and a poll timeout on $.DescribeIamInstanceProfileAssociationsResponse.iamInstanceProfileAssociationSet. Note the served state is `associated`, NOT `disassociating`: the association was never put onto any end-of-life path, so this is not a too-slow clock transition — nothing started one.
Why: a client relying on AWS's behavior uses this call as the convergence signal for instance-profile teardown — wait until the instance's association set is empty, then delete the instance profile and the role. Against the emulator that wait never returns, so every such teardown blocks until the caller's own timeout and then reports a resource it cannot clean up. The leaked association is also the record the instance report reads, so a terminated instance keeps advertising an IAM instance profile it is no longer running as, and a later AssociateIamInstanceProfile over a reused instance id is refused with `IncorrectState` ("There is an existing association for instance <instance-id>") for an association that should not exist.
Fix: the association store's end-of-life, in the ec2 crate's state rather than in a handler. The association record has exactly one removal seam today (DisassociateIamInstanceProfile -> start_iam_instance_profile_disassociation, settled by the clock) plus the atomic swap ReplaceIamInstanceProfileAssociation uses; the instance-termination writer makes no call into it at all, so no lifecycle event other than an explicit disassociation can retire an association. The instance-termination path must retire the association over that instance (and the by-instance index with it) through the same single seam, so the association report and the instance report cannot disagree about a terminated instance. If the recording's trigger is instead the teardown of the instance profile the association names, the same seam is where that retirement belongs. Whichever event ends it, the association must stop being reported rather than sit in `associated` forever.
Traces: iam-instance-profile-explicit-path-tags-launches-an-instance-delete-refused-with-role-and-not-found#task#25.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout: summary trace_set_id simparity-wire-aws-v1, emulator_up true, bootstrap_signal false, steps_total 1799, steps_passed 1749, steps_mismatched 49, steps_errored 0, steps_blocked 1, score_pct 97.2, divergence_count 30. This cluster: service ec2, operation DescribeIamInstanceProfileAssociations, verdict mismatch, expected_status 200 / actual_status 200, affected_count 1, stub_path "", owned_by [].

Cluster detail, verbatim:
- $: poll capped at 180s (expected {'DescribeIamInstanceProfileAssociationsResponse': {'requestId': '<shaped-not-asserted>', 'iamInstanceProfileAssociationSet': ''}}, got {'DescribeIamInstanceProfileAssociationsResponse': {'iamInstanceProfileAssociationSet': {'item': {'associationId': 'iip-assoc-000000000000000c8', 'instanceId': 'i-02adbc3b5b45a4da8', 'iamInstanceProfile': {'arn': 'arn:aws:iam::000000000000:instance-profile/sim2real-probe-iam-ip-dd49791e', 'id': 'AIPABGXEQES5NRCCJJWAN'}, 'state': 'associated', 'timestamp': '2026-11-26T18:34:30.683271Z'}}, 'requestId': '0ad7ffb5-f985-41f3-ae58-ae0d13b7f547'}})
- $.DescribeIamInstanceProfileAssociationsResponse.iamInstanceProfileAssociationSet: type_mismatch (expected '', got {'item': {...'state': 'associated'...}})
- $.DescribeIamInstanceProfileAssociationsResponse.iamInstanceProfileAssociationSet: poll: not in [''] after 19 attempt(s) (expected '', got {'item': {...'state': 'associated'...}})

The association id (iip-assoc-000000000000000c8), the instance id (i-02adbc3b5b45a4da8), the generated instance-profile name/ARN, the profile's IAM unique id (AIPABGXEQES5NRCCJJWAN), the timestamp and the requestId in that detail are all per-run values and are NOT the requirement; the recording does not assert the requestId either ('<shaped-not-asserted>'). The requirement is the EMPTY iamInstanceProfileAssociationSet the expected body carries. The retry count (19) and the 180s cap are the harness's, not AWS's.

Model grounding - smithy_query on services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json:
- DescribeIamInstanceProfileAssociations: input AssociationIds (xmlName AssociationId) / Filters / MaxResults (@range 5-1000) / NextToken; output IamInstanceProfileAssociations (xmlName iamInstanceProfileAssociationSet) + NextToken; errors []; paginated {inputToken NextToken, outputToken NextToken, items IamInstanceProfileAssociations, pageSize MaxResults}.
- DisassociateIamInstanceProfile: input AssociationId (required); output IamInstanceProfileAssociation (xmlName iamInstanceProfileAssociation); errors [].
- IamInstanceProfileAssociation: members associationId, instanceId, iamInstanceProfile, state, timestamp ("The time the IAM instance profile was associated with the instance").
- IamInstanceProfileAssociationState values: associating, associated, disassociating, disassociated. The operation's Filters doc lists only "associating | associated | disassociating" as selectable states.
- No error shapes and no message text are modelled for this operation, so the absence of the association after its life ends is recorded behavior plus the Filters documentation, not a modelled message.

Code grounding in this checkout (for the fixer's orientation only; the defect is the behavior above):
- services/service-com-amazonaws-ec2-amazonec2/src/state/iam_instance_profile_associations.rs - the store's only removal seams are start_iam_instance_profile_disassociation (sets `disassociating`, clock removes the row in settle_due_iam_instance_profile_associations) , remove_iam_instance_profile_association, and the atomic replace_iam_instance_profile_association. Nothing in the module is keyed to an instance's lifecycle.
- services/service-com-amazonaws-ec2-amazonec2/src/ops/terminate_instances.rs - the termination writer calls begin_instance_termination and publishes state-change notifications; it makes no call into the association store.
- services/service-com-amazonaws-ec2-amazonec2/src/state.rs - the only mentions of iam_instance_profile_associations are the child-module declaration and re-export, so no shared state path retires an association on instance termination.
These two together are consistent with the served state being `associated` rather than `disassociating`.

No open issue carries this replay key: a keyword search of the open issues for "InstanceProfile" and "Disassociate" returns only RunInstances-with-LaunchTemplate NotImplemented issues, a DescribeInstances IamInstanceProfile.Id omission, a CloudFormation AWS::IAM::InstanceProfile update issue, an IAM tagging-message issue, and unrelated Disassociate* stubs - none of them about the association's end of life.

## Steps to Reproduce
1. git_clone the repo, boot the emulator built from that checkout and replay simparity-wire-aws-v1 (replay_wire_corpus).
2. Fixture iam-instance-profile-explicit-path-tags-launches-an-instance-delete-refused-with-role-and-not-found, step task#25 - the step is a POLL of DescribeIamInstanceProfileAssociations that must settle on an EMPTY iamInstanceProfileAssociationSet. Observed: the same association is returned on every attempt with state 'associated' until the 180s cap (19 attempts).
3. Reproduce by hand against the emulator: CreateRole + CreateInstanceProfile (explicit Path and Tags) + AddRoleToInstanceProfile in iam; RunInstances naming that instance profile in ec2; DescribeIamInstanceProfileAssociations and confirm one association reaching state 'associated'. Then TerminateInstances on that instance and poll DescribeIamInstanceProfileAssociations (optionally with Filters.1.Name=instance-id, Filters.1.Value.1=<instance-id>). AWS reports an empty iamInstanceProfileAssociationSet once the instance is gone; the emulator keeps reporting the association in state 'associated' indefinitely.
4. Confirm the contrast with the path that DOES work: on a fresh association, DisassociateIamInstanceProfile(AssociationId=<id>) reports state 'disassociating' and the row disappears one transition span later - so the removal machinery exists and only the non-Disassociate lifecycle event is missing.
5. Also check the instance-side report while the leak is present: DescribeInstances on the terminated instance must not advertise an IamInstanceProfile it is no longer running as, and a later AssociateIamInstanceProfile must not be refused with IncorrectState "There is an existing association for instance <instance-id>" on account of the leaked association.
6. Re-replay iam-instance-profile-explicit-path-tags-launches-an-instance-delete-refused-with-role-and-not-found#task#25 and confirm the poll converges on an empty set before submitting.
