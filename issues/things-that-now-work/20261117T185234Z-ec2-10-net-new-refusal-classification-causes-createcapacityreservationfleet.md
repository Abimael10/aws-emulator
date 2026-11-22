Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.create-capacity-reservation-fleet-targeted-match-criteria-rejected ec2.create-managed-prefix-list-address-family-cidr-mismatch-rejected ec2.create-placement-group-partition-count-requires-partition-strategy-rejected ec2.create-vpc-endpoint-policy-character-quota-boundary ec2.get-managed-prefix-list-associations-max-results-below-minimum-rejected ec2.import-key-pair-malformed-public-key-material-rejected ec2.instance-event-window-lifecycle ec2.instance-task-error-semantics ec2.op-contract-delete-instance-event-window ec2.describe-prefix-lists-unknown-id-rejected ec2.lock-snapshot-lock-duration-past-the-maximum-rejected ec2.modify-vpc-attribute-no-attributes-specified-rejected
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.create-capacity-reservation-fleet-targeted-match-criteria-rejected|run#6|AssertionError|assert code == "InvalidParameterValue", f"expected InvalidParameterValue, got {code!r}"
Issue-Key: ec2.create-managed-prefix-list-address-family-cidr-mismatch-rejected|run#4|AssertionError|assert code == "InvalidParameterCombination", f"expected InvalidParameterCombination, got {code!r}"
Issue-Key: ec2.create-placement-group-partition-count-requires-partition-strategy-rejected|run#4|AssertionError|assert code == "InvalidParameterValue", f"expected InvalidParameterValue, got {code}: {message}"
Issue-Key: ec2.create-vpc-endpoint-policy-character-quota-boundary|run#18|AssertionError|assert (code, message) == _OVER_LIMIT, f"expected {_OVER_LIMIT}, got ({code!r}, {message!r})"
Issue-Key: ec2.get-managed-prefix-list-associations-max-results-below-minimum-rejected|run#8|AssertionError|assert code == "InvalidParameterValue", f"expected InvalidParameterValue, got {code!r}"
Issue-Key: ec2.import-key-pair-malformed-public-key-material-rejected|run#7|AssertionError|assert code == "InvalidKey.Format", f"expected InvalidKey.Format, got {code!r}"
Issue-Key: ec2.instance-event-window-lifecycle|run#22|AssertionError|assert redelete_code == "InvalidInstanceEventWindowIDNotFound", (
Issue-Key: ec2.instance-task-error-semantics|run#9|AssertionError|assert not problems, "unexpected instance-task error semantics:\n" + "\n".join(problems)
Issue-Key: ec2.op-contract-delete-instance-event-window|run#14|AssertionError|assert redelete_code == "InvalidInstanceEventWindowIDNotFound", f"unexpected error code {redelete_code!r}"
Issue-Key: ec2.describe-prefix-lists-unknown-id-rejected|run#7|AssertionError|assert code == "InvalidPrefixListId.NotFound", (
Issue-Key: ec2.lock-snapshot-lock-duration-past-the-maximum-rejected|run#10|AssertionError|assert code == "InvalidRequest", f"expected InvalidRequest, got {code!r}"
Issue-Key: ec2.modify-vpc-attribute-no-attributes-specified-rejected|run#7|AssertionError|assert code == "InvalidParameterCombination", f"unexpected error code: {code}"
Issue-Commit: 85ee122b95e0a025d1600c3d9babe834193f0ff3

# ec2: 10 net-new refusal-classification causes — CreateCapacityReservationFleet, CreateManagedPrefixList, CreatePlacementGroup, CreateVpcEndpoint, GetManagedPrefixListAssociations, ImportKeyPair, instance-event-window not-found, DescribePrefixLists, LockSnapshot and ModifyVpcAttribute answer the wrong error code

> _Note: this issue was identified against the code base at commit `85ee122b95e0a025d1600c3d9babe834193f0ff3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCapacityReservationFleet

## Description
Ten independent causes, all of the same shape: the emulator DOES refuse the request, but classifies the refusal with an error code real EC2 never uses for that condition. A client that branches on Error.Code (every SDK retry/handling policy does) takes the wrong branch. The EC2 Smithy model declares no modeled errors on any of these operations (errors: [] for all of them), so the required code is the one real EC2 returns on the ec2Query error envelope — recorded behavior, cited per cause.

Cause 1 of 10: CreateCapacityReservationFleet with a `targeted` InstanceMatchCriteria.
AWS: refuses the targeted match criteria with error code `InvalidParameterValue` (recorded behavior only; the op declares no modeled errors).
Emulator: answers `ValidationError`.
Why: a client catching InvalidParameterValue to report a bad fleet spec sees an unrecognised code and treats a deterministic input error as a generic/retryable failure.
Fix: the CreateCapacityReservationFleet handler's validation — classify the match-criteria refusal as InvalidParameterValue instead of emitting the generic ValidationError used for shape validation.
Traces: ec2.create-capacity-reservation-fleet-targeted-match-criteria-rejected.

Cause 2 of 10: CreateManagedPrefixList whose AddressFamily and entry CIDRs disagree.
AWS: `InvalidParameterCombination` — the condition is a conflict BETWEEN two parameters (recorded behavior only).
Emulator: `InvalidParameterValue`.
Why: callers distinguishing "one value is bad" from "these two parameters cannot be combined" mis-report the fault and retry with the wrong field corrected.
Fix: CreateManagedPrefixList validation — raise the combination class for the family/CIDR cross-check.
Traces: ec2.create-managed-prefix-list-address-family-cidr-mismatch-rejected.

Cause 3 of 10: CreatePlacementGroup with PartitionCount while Strategy is not `partition`.
AWS: `InvalidParameterValue`.
Emulator: `InvalidParameterCombination`, message "PartitionCount (<n>) is valid only when Strategy is '<strategy>', not 'partition'" (fixed text emulator-authored; <n> and <strategy> come from the request).
Why: the mirror image of cause 2 — the same client branch is wrong in the other direction.
Fix: CreatePlacementGroup validation.
Traces: ec2.create-placement-group-partition-count-requires-partition-strategy-rejected.

Cause 4 of 10: CreateVpcEndpoint with a PolicyDocument past the character quota.
AWS: code `InvalidPolicyDocument`, message exactly "Please provide a valid VPC Endpoint policy" (no variable part).
Emulator: code `InvalidParameterValue`, message "Invalid value for policyDocument: a VPC endpoint policy may carry at most 20480 characters" — an emulator-authored sentence that also leaks the internal limit.
Why: a client that special-cases InvalidPolicyDocument to re-author the policy instead sees a generic parameter error.
Fix: CreateVpcEndpoint policy validation — error class and message.
Traces: ec2.create-vpc-endpoint-policy-character-quota-boundary.

Cause 5 of 10: GetManagedPrefixListAssociations with MaxResults below the minimum.
AWS: `InvalidParameterValue` for this operation.
Emulator: `InvalidMaxResults`.
Why: EC2 is not uniform here — some operations use InvalidMaxResults and this one does not; a client keyed on the per-operation code mis-handles the page-size error.
Fix: GetManagedPrefixListAssociations MaxResults validation.
Traces: ec2.get-managed-prefix-list-associations-max-results-below-minimum-rejected.

Cause 6 of 10: ImportKeyPair with malformed public key material.
AWS: `InvalidKey.Format`.
Emulator: `InvalidKeyPair.Format`.
Why: the code names a different error family; retry/alert rules keyed on InvalidKey.Format never fire.
Fix: ImportKeyPair validation.
Traces: ec2.import-key-pair-malformed-public-key-material-rejected.

Cause 7 of 10: the instance-event-window not-found code.
AWS: DeleteInstanceEventWindow (and the other instance-event-window reads) on an absent or already-deleted window answers `InvalidInstanceEventWindowIDNotFound` — EC2 spells this one WITHOUT the dot separator.
Emulator: `InvalidInstanceEventWindowId.NotFound`.
Why: idempotent-delete handling ("already gone is success") is written against the exact code and therefore fails the second delete.
Fix: the instance-event-window handlers' not-found error construction.
Traces: ec2.instance-event-window-lifecycle, ec2.instance-task-error-semantics, ec2.op-contract-delete-instance-event-window.

Cause 8 of 10: DescribePrefixLists with an unknown prefix-list id.
AWS: `InvalidPrefixListId.NotFound` (lower-case `d` in `Id`).
Emulator: `InvalidPrefixListID.NotFound`, message "The prefix list (<id>) does not exist." — <id> from the request.
Why: codes are compared exactly; the capitalised variant is a different code to every client.
Fix: the DescribePrefixLists not-found error construction.
Traces: ec2.describe-prefix-lists-unknown-id-rejected.

Cause 9 of 10: LockSnapshot with a LockDuration past the maximum.
AWS: `InvalidRequest`.
Emulator: `ValidationError`.
Why: as cause 1 — a modelled-shape error class is substituted for the service's business refusal.
Fix: LockSnapshot validation.
Traces: ec2.lock-snapshot-lock-duration-past-the-maximum-rejected.

Cause 10 of 10: ModifyVpcAttribute with no attribute parameter at all.
AWS: `InvalidParameterCombination` — the request names a VPC but no attribute to change.
Emulator: `MissingParameter`.
Why: callers that treat MissingParameter as a client-library bug and InvalidParameterCombination as a request-shape error log the wrong defect.
Fix: ModifyVpcAttribute validation.
Traces: ec2.modify-vpc-attribute-no-attributes-specified-rejected.

## Evidence
Replay output, one line per cause (expected = real AWS, got = emulator):
- create-capacity-reservation-fleet-targeted-match-criteria-rejected: "expected InvalidParameterValue, got 'ValidationError'"
- create-managed-prefix-list-address-family-cidr-mismatch-rejected: "expected InvalidParameterCombination, got 'InvalidParameterValue'"
- create-placement-group-partition-count-requires-partition-strategy-rejected: "expected InvalidParameterValue, got InvalidParameterCombination: PartitionCount (3) is valid only when Strategy is 'partition', not 'cluster'"
- create-vpc-endpoint-policy-character-quota-boundary: "expected ('InvalidPolicyDocument', 'Please provide a valid VPC Endpoint policy'), got ('InvalidParameterValue', 'Invalid value for policyDocument: a VPC endpoint policy may carry at most 20480 characters')"
- get-managed-prefix-list-associations-max-results-below-minimum-rejected: "expected InvalidParameterValue, got 'InvalidMaxResults'"
- import-key-pair-malformed-public-key-material-rejected: "expected InvalidKey.Format, got 'InvalidKeyPair.Format'"
- instance-event-window-lifecycle: "expected InvalidInstanceEventWindowIDNotFound deleting an already-deleted window, got 'InvalidInstanceEventWindowId.NotFound'"; instance-task-error-semantics: "delete_instance_event_window: expected ['InvalidInstanceEventWindowIDNotFound'], got InvalidInstanceEventWindowId.NotFound"; op-contract-delete-instance-event-window: "unexpected error code 'InvalidInstanceEventWindowId.NotFound'"
- describe-prefix-lists-unknown-id-rejected: "expected InvalidPrefixListId.NotFound, got InvalidPrefixListID.NotFound: The prefix list (pl-00000000) does not exist."
- lock-snapshot-lock-duration-past-the-maximum-rejected: "expected InvalidRequest, got 'ValidationError'"
- modify-vpc-attribute-no-attributes-specified-rejected: "unexpected error code: MissingParameter" (assert code == "InvalidParameterCombination")

Smithy (services/service-com-amazonaws-ec2-amazonec2): CreateTags, UnlockSnapshot, ModifyVpcTenancy, CreateSubnetCidrReservation, DescribeLockedSnapshots and the other ops here all declare errors: [] — EC2 carries its error codes on the ec2Query envelope only, so the recorded code is the requirement.

## Steps to Reproduce
Boot the emulator and replay the ec2 behavioral checks listed in trace_ids, e.g.:
1. CreateCapacityReservationFleet with InstanceMatchCriteria=targeted -> read Error.Code (expect InvalidParameterValue).
2. CreateManagedPrefixList AddressFamily=IPv4 with an IPv6 entry -> expect InvalidParameterCombination.
3. CreatePlacementGroup Strategy=cluster PartitionCount=3 -> expect InvalidParameterValue.
4. CreateVpcEndpoint with a PolicyDocument over the quota -> expect InvalidPolicyDocument / "Please provide a valid VPC Endpoint policy".
5. GetManagedPrefixListAssociations MaxResults=1 -> expect InvalidParameterValue.
6. ImportKeyPair with garbage PublicKeyMaterial -> expect InvalidKey.Format.
7. CreateInstanceEventWindow, DeleteInstanceEventWindow twice -> second delete must answer InvalidInstanceEventWindowIDNotFound.
8. DescribePrefixLists PrefixListIds=[pl-00000000] -> expect InvalidPrefixListId.NotFound.
9. LockSnapshot with LockDuration above the maximum -> expect InvalidRequest.
10. ModifyVpcAttribute with only VpcId -> expect InvalidParameterCombination.
