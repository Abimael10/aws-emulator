Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.get-host-reservation-purchase-preview-invalid-host-rejected ec2.op-contract-purchase-host-reservation-rejected
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.get-host-reservation-purchase-preview-invalid-host-rejected|run#4|AssertionError|assert code == "InvalidHostId.Malformed", f"expected InvalidHostId.Malformed, got {code!r}"
Issue-Key: ec2.op-contract-purchase-host-reservation-rejected|run#6|AssertionError|assert err == "InvalidHostId.NotFound", (
Issue-Commit: bf168f2125883f6ad6bec616d1c212f3f21e9ddc

# ec2 Dedicated Host reservation refusals misclassify the host id: GetHostReservationPurchasePreview answers InvalidHostID.NotFound where AWS answers InvalidHostId.Malformed, and PurchaseHostReservation answers InvalidHostID.Malformed where AWS answers InvalidHostId.NotFound (2 causes)

> _Note: this issue was identified against the code base at commit `bf168f2125883f6ad6bec616d1c212f3f21e9ddc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetHostReservationPurchasePreview

## Description
Two net-new refusal-classification causes on the Dedicated Host reservation operations. The emulator's Malformed/NotFound verdict for a host id is inverted relative to AWS on both operations, and the error code's own spelling differs per operation on AWS.

Cause 1 of 2: GetHostReservationPurchasePreview with a made-up host id of the normal `h-` + 17 hex shape.
AWS: recorded behavior only — Smithy GetHostReservationPurchasePreview (input HostIdSet: RequestHostIdSet required, OfferingId required) declares no modeled errors (ec2Query, errors: []), so the refusal is known from the recording: HTTP 400 with Code `InvalidHostId.Malformed` and Message `The specified Dedicated host IDs ['<host-id>'] are not valid.` — AWS's own host-id validation for this operation runs before any existence lookup. Fixed text: `The specified Dedicated host IDs ['` ... `'] are not valid.`; the variable part is the host id list taken verbatim from the request's HostIdSet.
Emulator: answers Code `InvalidHostID.NotFound` for the same request — the wrong class (and the wrong casing of `Id`), as if it had performed an existence lookup instead of the shape check.
Why: a client that distinguishes "this id is not a host id" from "this host does not exist" — to decide between failing fast on bad input and retrying/provisioning — takes the wrong branch, and a client matching on the code string never matches at all.
Fix: the GetHostReservationPurchasePreview handler's HostIdSet validation and its error mapping (the refusal must be raised by the shape check before the host lookup, with AWS's code spelling and message template).
Traces: ec2.get-host-reservation-purchase-preview-invalid-host-rejected.

Cause 2 of 2: PurchaseHostReservation against a REAL offering id from DescribeHostReservationOfferings with a well-formed but nonexistent host id.
AWS: recorded behavior only — Smithy PurchaseHostReservation (input OfferingId required, HostIdSet required) declares no modeled errors, so the recording is the source: a well-formed unknown host id is refused HTTP 400 Code `InvalidHostId.NotFound` with Message `The specified Dedicated host IDs ['<host-id>'] are not valid.`, while a host id of the wrong shape is refused Code `InvalidHostID.Malformed` (note AWS's capital `ID` on this code, unlike the NotFound code beside it) with Message `The dedicated-host ID '<value>' is malformed`. Existence is checked against the host id before the offering, and shape before existence. Fixed text as quoted; the variable parts are the host id list / the offending value, taken from the request.
Emulator: answers Code `InvalidHostID.Malformed` for the well-formed unknown host id — it classifies a correctly shaped id as malformed, so the NotFound class is unreachable and the malformed leg of the contract is never exercised.
Why: the same wrong branch as Cause 1, mirrored: a caller purchasing a reservation for a host it believes it owns is told its request is syntactically wrong rather than that the host does not exist, so retry/reconciliation logic keyed on the code does the wrong thing.
Fix: the PurchaseHostReservation handler's HostIdSet validation and error mapping — shape check first (InvalidHostID.Malformed with the malformed message), then existence (InvalidHostId.NotFound with the not-valid message), both before the offering is resolved.
Traces: ec2.op-contract-purchase-host-reservation-rejected.

## Evidence
Cause 1 — Expected (AWS): ec2.get_host_reservation_purchase_preview(OfferingId='hro-…', HostIdSet=['h-0123456789abcdef0']) → ClientError Code 'InvalidHostId.Malformed', Message "The specified Dedicated host IDs ['h-0123456789abcdef0'] are not valid."
Actual (emulator): Code 'InvalidHostID.NotFound'. Replay: `AssertionError: expected InvalidHostId.Malformed, got 'InvalidHostID.NotFound'` (check_ec2_get_host_reservation_purchase_preview_invalid_host_rejected.py line 29). The host id shown is the check's input, not part of the requirement.

Cause 2 — Expected (AWS): with real_offering_id from DescribeHostReservationOfferings, ec2.purchase_host_reservation(OfferingId=real_offering_id, HostIdSet=['h-00000000000000000']) → Code 'InvalidHostId.NotFound', Message "The specified Dedicated host IDs ['h-00000000000000000'] are not valid."; the same call with HostIdSet=['not-a-host-id'] → Code 'InvalidHostID.Malformed', Message "The dedicated-host ID 'not-a-host-id' is malformed".
Actual (emulator): the first call returned Code 'InvalidHostID.Malformed'. Replay: `AssertionError: a well-formed but nonexistent host ID against a REAL offering: expected InvalidHostId.NotFound, got 'InvalidHostID.Malformed'` (check_ec2_op_contract_purchase_host_reservation_rejected.py line 27); the malformed leg was never reached.

## Steps to Reproduce
Cause 1:
1. ec2.get_host_reservation_purchase_preview(OfferingId='hro-0123456789abcdef0', HostIdSet=['h-0123456789abcdef0']).
2. Expected: 400 InvalidHostId.Malformed, "The specified Dedicated host IDs ['h-0123456789abcdef0'] are not valid." Actual: InvalidHostID.NotFound.

Cause 2:
1. offering = ec2.describe_host_reservation_offerings()['OfferingSet'][0]['OfferingId'] (a real catalog offering).
2. ec2.purchase_host_reservation(OfferingId=offering, HostIdSet=['h-00000000000000000']) → expected 400 InvalidHostId.NotFound with "The specified Dedicated host IDs ['h-00000000000000000'] are not valid."; actual InvalidHostID.Malformed.
3. ec2.purchase_host_reservation(OfferingId=offering, HostIdSet=['not-a-host-id']) → expected 400 InvalidHostID.Malformed with "The dedicated-host ID 'not-a-host-id' is malformed".
