Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: imagebuilder.list-images-pagination-invalid-token-rejected
Issue-Shape-Id: com.amazonaws.imagebuilder#imagebuilder
Issue-Key: imagebuilder.list-images-pagination-invalid-token-rejected|run#3|AssertionError|assert code == "InvalidParameterValueException", f"expected InvalidParameterValueException, got {code!r}"
Issue-Commit: d1107ef412a33c5fd1cd764c3d91be4521975a1a

# imagebuilder ListImages does not reject a malformed nextToken: a bad pagination token is answered with 501 NotImplemented instead of a 400 InvalidParameterValueException

> _Note: this issue was identified against the code base at commit `d1107ef412a33c5fd1cd764c3d91be4521975a1a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListImages

## Description
One net-new root cause in the Image Builder image-inventory read path: ListImages has no pagination-token validation, so a malformed `nextToken` is not classified at all.

Cause 1 of 1: imagebuilder ListImages does not validate the format of an inbound `nextToken` and therefore never produces the refusal AWS produces for a token it did not mint. Failing check: imagebuilder.list-images-pagination-invalid-token-rejected.

AWS: Smithy (services/service-com-amazonaws-imagebuilder-imagebuilder/smithy-model/com-amazonaws-imagebuilder-imagebuilder.json) — ListImages is bound to POST /ListImages, success status 200; input member `nextToken` targets PaginationToken with @length min 1 / max 65535; the operation is @paginated (inputToken/outputToken `nextToken`, items `imageVersionList`, pageSize `maxResults`). Its modeled errors are CallRateLimitExceededException (429), ClientException (400), ForbiddenException (403), InvalidPaginationTokenException (400), InvalidRequestException (400), ServiceException (500), ServiceUnavailableException (503). The model does not name the code real AWS actually returns for a syntactically valid but un-mintable token, so for the code and message this is recorded behavior only: the recording shows the service refusing the call with HTTP 400, Error.Code `InvalidParameterValueException` and the message "The value supplied for parameter 'nextToken' is not valid. The provided token does not meet the required format." In that message everything is fixed text except the quoted member name, which is the input member the bad value arrived in (`'nextToken'` here); the token value itself is NOT echoed. The positive control in the same recording shows the identical call with no `nextToken` answering HTTP 200 with `requestId` and an `imageVersionList` that is empty for a name the account never created — so an empty inventory is a 200, never an error.

Emulator: the call is refused with HTTP 501 and Error.Code `NotImplemented` ("operation ListImages is scaffolded but not implemented in this emulator"), because services/service-com-amazonaws-imagebuilder-imagebuilder/src/ops/list_images.rs is still the generated scaffold. Replay assertion: `AssertionError: expected InvalidParameterValueException, got 'NotImplemented'`. The request's `nextToken` is never examined, so even after the handler is stood up there is no code path that distinguishes a token the emulator minted from an arbitrary string: implementing the happy path alone would make a bad token silently read as "start from the beginning" and answer 200, which is a second divergence in the same place. `NotImplemented` is not a code this operation models.

Why: a client that walks pages of ListImages relies on the service telling it when a token is stale or corrupt. On AWS the SDK sees a 400 client error it can surface as a parameter-validation failure and stop; against the emulator it either sees an unmodelled 501 (treated as a service failure, not a client mistake, and not retry-classified by any SDK error path) or — once the happy path lands without token validation — a 200 first page, which makes a corrupted-token loop restart silently and re-enumerate images forever instead of failing fast.

Fix: the operation's input validation in its handler — services/service-com-amazonaws-imagebuilder-imagebuilder/src/ops/list_images.rs. The handler must parse/verify the inbound `nextToken` against the emulator's own token encoding before serving a page and, when it does not decode, refuse with HTTP 400, Error.Code `InvalidParameterValueException` and the message above with the offending member name filled in; a well-formed or absent token continues to the paginated read of stored images. Related open issues cover only making the operation answer at all and do not name token validation: 20261114T011705Z-ec2-iam-imagebuilder-implement-imagebuilder-listimages-the-operation-answers-501-notimplemented.md, and the auto-filed ListImages scaffold-stub issue for this suite. This cause survives both of those fixes and must be closed on its own.

Traces: imagebuilder.list-images-pagination-invalid-token-rejected.

## Evidence
Check imagebuilder.list-images-pagination-invalid-token-rejected [mismatch].

Call: imagebuilder:ListImages with owner="Self", nextToken="not-a-real-pagination-token==".

Expected (AWS, from the recording): HTTP 400, Error.Code "InvalidParameterValueException", Error.Message "The value supplied for parameter 'nextToken' is not valid. The provided token does not meet the required format." — fixed text, with the quoted member name taken from the request ('nextToken').

Actual (emulator): HTTP 501, Error.Code "NotImplemented", message "operation ListImages is scaffolded but not implemented in this emulator".

Replay output:
  File "check_imagebuilder_list_images_pagination_invalid_token_rejected.py", line 18, in run
    assert code == "InvalidParameterValueException", f"expected InvalidParameterValueException, got {code!r}"
  AssertionError: expected InvalidParameterValueException, got 'NotImplemented'

The check's second half (the positive control: the same ListImages call with no nextToken and filters=[{"name":"name","values":[<unique name>]}] must answer 200 with imageVersionList == []) is never reached, so the no-token path is unverified as well.

Modeled surface for reference: POST /ListImages, code 200; nextToken: PaginationToken @length 1..65535; modeled 400-class errors ClientException, InvalidPaginationTokenException, InvalidRequestException — `NotImplemented` is not among them.

Handler state in the checkout: services/service-com-amazonaws-imagebuilder-imagebuilder/src/ops/list_images.rs is the generated scaffold and ends its handle body in a not_implemented("ListImages") refusal.

## Steps to Reproduce
1. Call imagebuilder:ListImages with owner="Self" and nextToken="not-a-real-pagination-token==" (any string the emulator did not mint).
2. EXPECT HTTP 400 with Error.Code "InvalidParameterValueException" and Error.Message "The value supplied for parameter 'nextToken' is not valid. The provided token does not meet the required format." OBSERVED HTTP 501 with Error.Code "NotImplemented" and message "operation ListImages is scaffolded but not implemented in this emulator".
3. Positive control, which must keep passing after the fix: call imagebuilder:ListImages with owner="Self", no nextToken, and filters=[{"name": "name", "values": ["<a name this account never created>"]}]. EXPECT HTTP 200 with requestId and imageVersionList == [].
4. Regression guard for the same cause: ListImages with no nextToken, take the nextToken it returns, truncate or mutate one character of it, and re-call — EXPECT the same 400 InvalidParameterValueException refusal, not a 200 first page.
