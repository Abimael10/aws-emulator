Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: guardduty.publishing-destination-lifecycle
Issue-Shape-Id: com.amazonaws.guardduty#GuardDutyAPIService
Issue-Key: guardduty|UpdatePublishingDestination|BadRequestException
Issue-Commit: cf9fc6ab8267f9d5f0acca19e6a4a41633dc20ed

# UpdatePublishingDestination refuses an absent/deleted DestinationId with the generic invalid-parameters sentence instead of AWS's publishingDestination-not-found message

> _Note: this issue was identified against the code base at commit `cf9fc6ab8267f9d5f0acca19e6a4a41633dc20ed`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdatePublishingDestination

## Description
This issue carries 1 root cause in the guardduty crate.

Cause 1 of 1: the publishing-destination family words "that destinationId does not resolve" with ONE sentence for the read and both writes, but AWS words the MUTATION differently from the read.

AWS: Smithy UpdatePublishingDestination (POST /detector/{DetectorId}/publishingDestination/{DestinationId}; DetectorId httpLabel+required len 1..300, DestinationId httpLabel+required, DestinationProperties optional) models exactly BadRequestException (httpError 400) and InternalServerErrorException (500); the model states no message text, so the wording is recorded behavior only. Recorded behavior: after DeletePublishingDestination, DescribePublishingDestination on the same DestinationId is refused HTTP 400 BadRequestException with the wholly fixed text 'The request is rejected because the one or more input parameters have invalid values.' (no variable part), while UpdatePublishingDestination on that same deleted DestinationId is refused HTTP 400 BadRequestException with the different, also wholly fixed text 'The request failed because no publishingDestination exists with the provided publishingDestinationId.' — the member name 'publishingDestinationId' is part of the fixed text; neither the detector id nor the destination id is interpolated. So the status (400) and the code (BadRequestException) are the same for both, and only the message distinguishes the mutation's refusal from the read's.

Emulator: the describe path is correct (that assertion passed), but UpdatePublishingDestination on a DestinationId the detector does not hold answers HTTP 400 / BadRequestException with the READ's sentence, 'The request is rejected because the one or more input parameters have invalid values.'. This is deliberate in the code: services/service-com-amazonaws-guardduty-guarddutyapiservice/src/ops/update_publishing_destination.rs returns bad_request(crate::state::DESTINATION_NOT_FOUND_MESSAGE) for the unresolved destination, and that constant is defined as an alias of INVALID_INPUT_PARAMETERS_MESSAGE in src/state.rs (with a comment asserting the whole family — read and both writes — shares one sentence). The same constant is used for the delete path, so a delete of an already-absent destination is very likely to carry the same wrong wording.

Why: a client that mutates a publishing destination concurrently with (or after) a delete cannot tell 'the destination is gone' from 'your DestinationProperties/DetectorId were malformed'. Both are 400/BadRequestException, so the message is the ONLY discriminator AWS gives. Retry and reconcile logic keyed to the real text (recreate the destination when it no longer exists, versus surface a validation bug to the caller and stop retrying) takes the wrong branch, and parity assertions on the documented wording fail.

Fix: the not-found refusal of the UpdatePublishingDestination handler and the shared refusal wording it draws on — the family must word the mutation's absent-destination refusal separately from the read's, rather than aliasing one constant for all three operations. The delete path should be corrected with it.

Traces: guardduty.publishing-destination-lifecycle (file and line citations above).

## Evidence
Failing check guardduty.publishing-destination-lifecycle, assertion at check_guardduty_publishing_destination_lifecycle.py:106 (expected = real AWS, got = emulator):
  AssertionError: expected message 'The request failed because no publishingDestination exists with the provided publishingDestinationId.', got 'The request is rejected because the one or more input parameters have invalid values.'

Preceding assertions in the same check that PASSED and bound the divergence:
- CreatePublishingDestination -> DestinationId, Status reaches PUBLISHING: ok.
- UpdatePublishingDestination rotating KmsKeyArn, read back via DescribePublishingDestination: ok.
- After DeletePublishingDestination, DescribePublishingDestination refused with Code 'BadRequestException' and Message 'The request is rejected because the one or more input parameters have invalid values.': ok — the emulator matches AWS on the READ.
- UpdatePublishingDestination on the deleted DestinationId: Code 'BadRequestException' matched; only the Message diverged, as quoted above.

Fixed text vs variable parts: both messages are entirely fixed text. The expected mutation message interpolates nothing — 'publishingDestinationId' is the literal member name AWS names, not the destination id from the request. HTTP status 400 and error code BadRequestException are unchanged in both AWS and the emulator.

Code the wording comes from: src/ops/update_publishing_destination.rs returns bad_request(crate::state::DESTINATION_NOT_FOUND_MESSAGE) when detector.publishing_destinations.get(&input.destination_id) is None; src/state.rs defines `pub const DESTINATION_NOT_FOUND_MESSAGE: &str = INVALID_INPUT_PARAMETERS_MESSAGE;` with a doc comment claiming the read and the two writes share one sentence.

## Steps to Reproduce
1. Boot the emulator and replay guardduty.publishing-destination-lifecycle.
2. Manual repro: ListDetectors (or CreateDetector(Enable=true)) to get a DetectorId; create an S3 bucket and a KMS key usable as a publishing target.
3. CreatePublishingDestination(DetectorId, DestinationType='S3', DestinationProperties={'DestinationArn': 'arn:aws:s3:::<bucket>', 'KmsKeyArn': '<key-arn>'}) and keep the returned DestinationId.
4. DeletePublishingDestination(DetectorId, DestinationId).
5. DescribePublishingDestination(DetectorId, DestinationId) -> confirm BadRequestException / 'The request is rejected because the one or more input parameters have invalid values.' (this already matches AWS).
6. UpdatePublishingDestination(DetectorId, DestinationId, DestinationProperties={'DestinationArn': 'arn:aws:s3:::<bucket>', 'KmsKeyArn': '<key-arn>'}) and compare Error.Message against 'The request failed because no publishingDestination exists with the provided publishingDestinationId.' — the emulator instead returns the describe sentence.
7. Same probe with a DestinationId that was never minted, and with DeletePublishingDestination on an already-deleted DestinationId, to confirm the mutation wording is used consistently for the absent case.
