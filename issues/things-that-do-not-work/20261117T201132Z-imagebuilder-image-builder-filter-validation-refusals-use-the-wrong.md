Issue-Source: trace-sweep-divergence
Issue-Trace: imagebuilder-list-images-and-list-image-pipelines-filter-validation
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: imagebuilder-list-images-and-list-image-pipelines-filter-validation
Issue-Shape-Id: com.amazonaws.imagebuilder#imagebuilder
Issue-Key: imagebuilder|ListImagePipelines|4xx->4xx|$.message
Issue-Key: imagebuilder|ListImages|4xx->4xx|$.message
Issue-Key: imagebuilder|ListImages|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: imagebuilder|ListImagePipelines|4xx->4xx|$.__type
Issue-Commit: 738532f931c20a228b1fc1d492f033ad05ac6ae5

# Image Builder filter validation refusals use the wrong error class and wording on ListImages/ListImagePipelines

> _Note: this issue was identified against the code base at commit `738532f931c20a228b1fc1d492f033ad05ac6ae5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListImagePipelines

## Description
Image Builder's `filters` input validation on the list operations is implemented by the emulator with its own error class and its own sentence structure instead of Image Builder's. Three distinct causes, all on the `filters` member of the ListImages / ListImagePipelines family. All three are net-new: no open issue carries these replay keys (the nearest open imagebuilder issues — 20261117T151752Z-imagebuilder-listimages-stub.md and 20261117T151752Z-imagebuilder-listimages-does-not-reject-a-malformed-nexttoken.md — concern the unimplemented ListImages body and its nextToken handling, not filter validation, and neither covers these steps).

Cause 1 of 3: ListImagePipelines — unsupported filter NAME refusal does not identify the offending list element and does not use Image Builder's allowed-name sentence.
  AWS: Smithy: ListImagePipelines.filters -> FilterList (@length min 1, max 10) of Filter; Filter.name -> FilterName (@pattern ^[a-zA-Z]{1,1024}$); modeled client errors InvalidRequestException (400) / ClientException (400); InvalidParameterValueException (400, "The value that you provided for the specified parameter is invalid.") is also modeled in the service. The model does not enumerate the legal filter names, so for the name set this is recorded behavior only: the recording shows AWS answering HTTP 400 with a message that (a) names the offending element by its INDEXED member path, and (b) states the rule and then lists the allowed names in the service's own order. Fixed text: "The value supplied for parameter '" <member path> "' is not valid. Unsupported filter name. Allowed: " <allowed names>. Variable parts: the member path, which is taken from the request and must carry the index of the failing filter (here filters[0].name, i.e. the first list element's name), and the allowed-name list, which is the operation's own supported filter set (for ListImagePipelines: name, description, status, imageRecipeArn, infrastructureConfigurationArn, distributionConfigurationArn). Nothing else in the message varies.
  Emulator: answers 400 but with "The value supplied for parameter 'filters' is not valid. Allowed filters for ListImagePipelines are: description, distributionConfigurationArn, imageRecipeArn, infrastructureConfigurationArn, name, status." — it reports the whole `filters` member rather than the indexed element that failed, replaces the "Unsupported filter name. Allowed: " sentence with an emulator-authored "Allowed filters for <Operation> are: ..." sentence, re-sorts the allowed names alphabetically, and appends a trailing period AWS does not emit. Reproduced identically for three request shapes in the fixture (task#4, task#6, task#8).
  Why: a client that sends several filters cannot tell WHICH one AWS rejected, because the index is gone — error-path code that highlights the bad filter or retries with that element removed has nothing to key on. Message-matching clients and docs/assertions keyed on "Unsupported filter name" never match.
  Fix: the ListImagePipelines handler's `filters` validation — the filter-name check and the message it formats.
  Traces: imagebuilder-list-images-and-list-image-pipelines-filter-validation#task#4 (also task#6, task#8).

Cause 2 of 3: ListImages — the over-length `filters` refusal is emitted as a generic Smithy constraint violation instead of Image Builder's parameter-value refusal.
  AWS: Smithy: ListImages.filters -> FilterList with @length min 1, max 10. The recording shows AWS refusing an 11-element list with HTTP 400 and the message "The value supplied for parameter 'filters' is not valid. filters must contain less than or equal to 10 items". Fixed text: "The value supplied for parameter '" <member> "' is not valid. " <member> " must contain less than or equal to " <limit> " items". Variable parts: the member name (filters) taken from the request, and the limit (10) taken from the member's modeled @length max. The recorded x-amzn-errortype is not asserted in the fixture, but it is NOT the Smithy framework's ValidationException: Image Builder models no ValidationException, so the type must be one of the operation's modeled client errors (InvalidParameterValueException / InvalidRequestException — see Cause 3, where the recording does pin __type to InvalidParameterValueException for the same rule).
  Emulator: answers 400 with the Smithy framework constraint message "1 validation error detected: Value '11 items' at 'filters' failed to satisfy constraint: Member must have length less than or equal to 10" and the header x-amzn-errortype: ValidationException — an error type this service does not model at all.
  Why: an SDK deserializes an unmodelled x-amzn-errortype as a generic/unknown service error, so `except InvalidParameterValueException` (or the equivalent typed catch in any SDK) never fires and the refusal surfaces as an unknown error; retry classification and user-facing messaging both go wrong. The message is also in a different grammar from every other Image Builder refusal, so message-matching breaks.
  Fix: ListImages' input validation — the FilterList @length check must be raised as the service's own parameter-value error with the service's message template, not left to the generic constraint-violation serializer.
  Traces: imagebuilder-list-images-and-list-image-pipelines-filter-validation#task#9.

Cause 3 of 3: ListImagePipelines — the over-length `filters` refusal carries the wrong error CODE (`__type`) as well as the wrong message.
  AWS: Smithy: ListImagePipelines.filters -> FilterList with @length min 1, max 10; InvalidParameterValueException is modeled (client, httpError 400, "The value that you provided for the specified parameter is invalid."). The recording pins the response body's __type to InvalidParameterValueException and the message to "The value supplied for parameter 'filters' is not valid. filters length must satisfy max_items 10". Fixed text: "The value supplied for parameter '" <member> "' is not valid. " <member> " length must satisfy max_items " <limit>. Variable parts: the member name (filters) from the request and the limit (10) from the member's modeled @length max.
  Emulator: answers 400 with __type "ValidationException" and the generic message "1 validation error detected: Value '11 items' at 'filters' failed to satisfy constraint: Member must have length less than or equal to 10". Both the error code and the message diverge. Reproduced for two request shapes (task#10, task#12).
  Why: ValidationException is not in this service's error set, so no typed catch matches and the client sees an unknown error for a plainly-invalid request; code that distinguishes a bad parameter value (fix the request) from a throttle or a server fault mis-classifies it.
  Fix: ListImagePipelines' input validation and the error serializer that maps a constraint violation to a wire error — the FilterList length violation must serialize as InvalidParameterValueException with the service's message template.
  Traces: imagebuilder-list-images-and-list-image-pipelines-filter-validation#task#10 (also task#12).

## Evidence
Replay of fixture imagebuilder-list-images-and-list-image-pipelines-filter-validation (trace set simparity-wire-aws-v1), 3 divergence clusters covering 6 steps, all status 400 on both sides:

Cause 1 — step task#4 (ListImagePipelines), cluster also covers task#6, task#8:
  expected $.message: "The value supplied for parameter 'filters[0].name' is not valid. Unsupported filter name. Allowed: name, description, status, imageRecipeArn, infrastructureConfigurationArn, distributionConfigurationArn"
  actual   $.message: "The value supplied for parameter 'filters' is not valid. Allowed filters for ListImagePipelines are: description, distributionConfigurationArn, imageRecipeArn, infrastructureConfigurationArn, name, status."

Cause 2 — step task#9 (ListImages):
  expected $.message: "The value supplied for parameter 'filters' is not valid. filters must contain less than or equal to 10 items"
  actual   $.message: "1 validation error detected: Value '11 items' at 'filters' failed to satisfy constraint: Member must have length less than or equal to 10"
  expected $headers.x-amzn-errortype: <shaped-not-asserted>; actual: "ValidationException" (reported shape_mismatch; ValidationException is not a modeled imagebuilder error)

Cause 3 — step task#10 (ListImagePipelines), cluster also covers task#12:
  expected $.__type: "InvalidParameterValueException"; actual $.__type: "ValidationException"
  expected $.message: "The value supplied for parameter 'filters' is not valid. filters length must satisfy max_items 10"
  actual   $.message: "1 validation error detected: Value '11 items' at 'filters' failed to satisfy constraint: Member must have length less than or equal to 10"

Model evidence (services/service-com-amazonaws-imagebuilder-imagebuilder/smithy-model/com-amazonaws-imagebuilder-imagebuilder.json): ListImages.filters and ListImagePipelines.filters both target FilterList with @length {min: 1, max: 10}; Filter.name targets FilterName with @pattern ^[a-zA-Z]{1,1024}$; the operations' modeled client errors are CallRateLimitExceededException, ClientException, ForbiddenException, InvalidPaginationTokenException, InvalidRequestException; InvalidParameterValueException is modeled as a 400 client error on the service. No ValidationException shape is modeled.

## Steps to Reproduce
1. Boot the emulator and replay fixture imagebuilder-list-images-and-list-image-pipelines-filter-validation from trace set simparity-wire-aws-v1.
2. Cause 1 — POST /ListImagePipelines with filters=[{name: "<name not in the supported set>", values: ["x"]}] (step task#4; task#6 and task#8 are the same cluster). Observe 400 whose $.message names 'filters' rather than 'filters[0].name' and uses "Allowed filters for ListImagePipelines are: ..." instead of "Unsupported filter name. Allowed: name, description, status, imageRecipeArn, infrastructureConfigurationArn, distributionConfigurationArn".
3. Cause 2 — POST /ListImages with an 11-element filters list (step task#9). Observe 400 with the generic "1 validation error detected: ..." message and header x-amzn-errortype: ValidationException, instead of "The value supplied for parameter 'filters' is not valid. filters must contain less than or equal to 10 items" under one of the service's modeled error types.
4. Cause 3 — POST /ListImagePipelines with an 11-element filters list (step task#10; task#12 is the same cluster). Observe 400 with $.__type "ValidationException" and the generic constraint message, instead of $.__type "InvalidParameterValueException" with "The value supplied for parameter 'filters' is not valid. filters length must satisfy max_items 10".
