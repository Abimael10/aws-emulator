Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.put-parameter-typed-data-types-round-trip ssm.put-parameter-rejects-malformed-typed-values ssm.put-parameter-advanced-only-data-types-rejected-on-standard-tier ssm.put-resource-policy-second-policy-without-policy-id-rejected ssm.put-resource-policy-update-with-stale-hash-rejected-not-found ssm.update-document-default-version-nonexistent-version-rejected ssm.update-ops-metadata-invalid-argument-rejected
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|PutParameter|ValidationException
Issue-Key: ssm.put-parameter-advanced-only-data-types-rejected-on-standard-tier|expect_error#2|AssertionError|assert got_message == message, f"expected message {message!r}, got {got_message!r}"
Issue-Key: ssm.put-resource-policy-second-policy-without-policy-id-rejected|_error#10|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: ssm.put-resource-policy-update-with-stale-hash-rejected-not-found|run#21|AssertionError|assert err["Code"] == "ResourcePolicyNotFoundException", err
Issue-Key: ssm.update-document-default-version-nonexistent-version-rejected|run#8|AssertionError|assert got == expected, f"expected {expected!r}, got {got!r}"
Issue-Key: ssm.update-ops-metadata-invalid-argument-rejected|run#10|AssertionError|assert error.get("Message") == (
Issue-Commit: 845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab

# ssm: 6 net-new behavioral causes — PutParameter refuses the documented typed data types, both PutResourcePolicy single-policy rules are wrong, and two refusal messages are emulator-authored

> _Note: this issue was identified against the code base at commit `845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutParameter

## Description
Six independent net-new root causes in the ssm service, each grounded in its own failing behavioral check. A fixer must close all six before submitting.

Cause 1 of 6: PutParameter refuses the typed Parameter Store data types, so no typed parameter can be created at all.
AWS: Smithy com.amazonaws.ssm#PutParameter, input member DataType targets ParameterDataType, a plain string with @length 0-128 and NO enum constraint; the member's modeled documentation lists text, aws:ec2:image and aws:ssm:integration but is not exhaustive, so this is recorded behavior: AWS accepts DataType values aws:region, aws:account:id, boolean, integer, float, binary, hostname, email, ipv4-address and ipv6-address on the Standard tier (HTTP 200 with Version/Tier) and validates the VALUE against the type, refusing a malformed value with a per-type message such as "The parameter value is not a valid AWS Region. Please check https://docs.aws.amazon.com/global-infrastructure/latest/regions/aws-regions.html for the list of valid regions.", "The parameter value is not a valid AWS account ID." or "The parameter value is not a valid boolean. Accepted values are true, True, TRUE, false, False, FALSE, 1 and 0." — each entirely fixed text for its type.
Emulator: PutParameter with DataType="aws:region" (and every other typed value) is refused HTTP 400 ValidationException "DataType 'aws:region' isn't supported; the supported data type values are text aws:ec2:image aws:ssm:integration". The emulator therefore implements a closed three-value allow-list where AWS has an open data-type set, and never reaches any value validation.
Why: a client that types its parameters — the documented way to get Parameter Store to validate regions, account ids, hostnames, emails and addresses — cannot write a single one, and its own negative paths (a malformed value being rejected) are unobservable.
Fix: the PutParameter handler's DataType validation and the per-type value validators in the ssm service crate — accept the full documented data-type set and validate the value per type instead of rejecting the type name.
Traces: ssm.put-parameter-typed-data-types-round-trip, ssm.put-parameter-rejects-malformed-typed-values.

Cause 2 of 6: the advanced-only data-type refusal on the Standard tier uses the wrong message.
AWS: recorded behavior: url, json and jsonc are Advanced-tier-only data types; PutParameter with Tier Standard and one of them is refused HTTP 400 ValidationException with "The <data type> data type is only supported for advanced parameters." — fixed text "The " + " data type is only supported for advanced parameters." with the data-type name taken from the request.
Emulator: the same request is refused with the closed-allow-list message "DataType 'url' isn't supported; the supported data type values are text aws:ec2:image aws:ssm:integration", which states the type does not exist rather than that it needs the Advanced tier.
Why: a client cannot distinguish "unknown data type" from "upgrade the parameter tier", which is the actionable remedy AWS names.
Fix: the PutParameter handler's tier/data-type gate in the ssm service crate.
Traces: ssm.put-parameter-advanced-only-data-types-rejected-on-standard-tier.

Cause 3 of 6: PutResourcePolicy attaches a second policy to a parameter that already has one.
AWS: Smithy com.amazonaws.ssm#PutResourcePolicy, optional PolicyId/PolicyHash, errors including ResourcePolicyConflictException, ResourcePolicyLimitExceededException, ResourcePolicyNotFoundException. Recorded behavior: a parameter holds at most one resource policy; a second PutResourcePolicy with NO PolicyId while a policy is attached is refused HTTP 400 ValidationException "Resource policy for parameter <ResourceArn> already exists" — fixed text around the resource ARN taken from the request — and the attached policy is unchanged; the identical no-PolicyId call succeeds once the first policy is deleted.
Emulator: the second call succeeds with HTTP 200 and returns a new PolicyId/PolicyHash.
Why: a caller can silently replace or duplicate a parameter's access policy without supplying the PolicyId that AWS requires for an update, which is exactly the lost-update that PolicyId/PolicyHash exist to prevent.
Fix: the PutResourcePolicy handler in the ssm service crate — reject a create when a policy is already attached and require PolicyId for an update.
Traces: ssm.put-resource-policy-second-policy-without-policy-id-rejected.

Cause 4 of 6: a PutResourcePolicy update carrying a stale PolicyHash is refused with the wrong error class.
AWS: recorded behavior for the same operation: an update that names an existing PolicyId with a PolicyHash that no longer matches is refused HTTP 400 with Error.Code ResourcePolicyNotFoundException (the modeled not-found error; httpResponseCode 404 in the model).
Emulator: the call is refused with Error.Code ResourcePolicyConflictException and the message "The hash provided in the call doesn't match the stored hash.".
Why: the two classes mean different things to a client — conflict invites a read-modify-retry loop, not-found means re-resolve the policy — so optimistic-concurrency code written against AWS takes the wrong branch.
Fix: the PutResourcePolicy handler's PolicyId/PolicyHash reconciliation in the ssm service crate.
Traces: ssm.put-resource-policy-update-with-stale-hash-rejected-not-found.

Cause 5 of 6: UpdateDocumentDefaultVersion's InvalidDocumentVersion message is emulator-authored.
AWS: Smithy com.amazonaws.ssm#UpdateDocumentDefaultVersion, required Name and DocumentVersion, modeled client error InvalidDocumentVersion (400). Recorded behavior: a version number the document does not have is refused with that code and the message "Document with name <Name> with version <DocumentVersion> does not exist." — fixed text with the document name and requested version taken from the request.
Emulator: same code, message "Document version: 7 is invalid, latest version is 1" — it names neither the document nor AWS's wording, and leaks the current latest version.
Why: message-matching clients break, and the refusal no longer identifies which document was addressed.
Fix: the UpdateDocumentDefaultVersion handler's version lookup error in the ssm service crate.
Traces: ssm.update-document-default-version-nonexistent-version-rejected.

Cause 6 of 6: UpdateOpsMetadata's map-key ValidationException renders the value and the pattern differently from AWS.
AWS: Smithy com.amazonaws.ssm#UpdateOpsMetadata, MetadataToUpdate targets MetadataMap whose key MetadataKey has @length 1-256 and @pattern "^(?!\\s*$).+$". Recorded behavior: a whitespace-only key is refused ValidationException with "1 validation error detected: Value '{   =MetadataValue(value=bad)}' at 'metadataToUpdate' failed to satisfy constraint: Map keys must satisfy constraint: [Member must have length less than or equal to 256, Member must have length greater than or equal to 1, Member must satisfy regular expression pattern: ^(?!\\s*$).+]" — note the rendered map value uses the lowercase member spelling value= and the pattern keeps its leading ^ anchor.
Emulator: the same framing but renders the map value as MetadataValue(Value=bad) (capitalised member name) and the pattern as (?!\\s*$).+ with the leading ^ anchor dropped.
Why: the whole refusal text is a requirement; a client matching it — or a user reading the pattern — gets a regex that is not the one the service enforces. This is the same render family as the resolved issue 20261103T173026Z-ssm-validationexception-text-misrenders-constraints-anchored-patterns-dropped.md, which does not cover this operation's map-key path.
Fix: the shape-validation message renderer used by the UpdateOpsMetadata handler in the ssm service crate — keep the leading anchor and emit modeled member names in their wire spelling when rendering a map value.
Traces: ssm.update-ops-metadata-invalid-argument-rejected.

## Evidence
Behavioral suite, service heading `### ssm`.

Causes 1 — ssm.put-parameter-typed-data-types-round-trip and ssm.put-parameter-rejects-malformed-typed-values, identical escape on the first typed case:
botocore.errorfactory.ValidationException: An error occurred (ValidationException) when calling the PutParameter operation: DataType 'aws:region' isn't supported; the supported data type values are text aws:ec2:image aws:ssm:integration
The checks drive the Standard-tier cases (aws:region, aws:account:id, boolean, integer, float, binary, hostname, email, ipv4-address, ipv6-address) and, for the negative check, the per-type value messages quoted in the description.

Cause 2 — ssm.put-parameter-advanced-only-data-types-rejected-on-standard-tier:
expected message 'The url data type is only supported for advanced parameters.', got "DataType 'url' isn't supported; the supported data type values are text aws:ec2:image aws:ssm:integration" (same for json and jsonc).

Cause 3 — ssm.put-resource-policy-second-policy-without-policy-id-rejected:
expected ValidationException 'Resource policy for parameter <parameter ARN> already exists';
actual: AssertionError: expected the call to be refused, but it succeeded: {'PolicyId': 'e519367d-a023-44f1-9829-e5eeb2f485e5', 'PolicyHash': '17af946d...', 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}

Cause 4 — ssm.put-resource-policy-update-with-stale-hash-rejected-not-found:
assert err['Code'] == 'ResourcePolicyNotFoundException' failed with
{'Message': "The hash provided in the call doesn't match the stored hash.", 'Code': 'ResourcePolicyConflictException'}

Cause 5 — ssm.update-document-default-version-nonexistent-version-rejected:
expected ('InvalidDocumentVersion', 'Document with name <doc> with version 7 does not exist.'), got ('InvalidDocumentVersion', 'Document version: 7 is invalid, latest version is 1')

Cause 6 — ssm.update-ops-metadata-invalid-argument-rejected:
expected "... Value '{   =MetadataValue(value=bad)}' at 'metadataToUpdate' ... Member must satisfy regular expression pattern: ^(?!\\s*$).+]"
got      "... Value '{   =MetadataValue(Value=bad)}' at 'metadataToUpdate' ... Member must satisfy regular expression pattern: (?!\\s*$).+]"
The same check's positive control (OpsMetadataNotFoundException "OpsMetadata object not found. Verify the Amazon Resource Name (ARN) and try again.") passes, so only the render diverges.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the behavioral suite.
2. Cause 1: ssm PutParameter Name=<new>, Value="us-east-1", Type="String", DataType="aws:region". Observe 400 ValidationException "DataType 'aws:region' isn't supported; ..."; AWS answers 200 and GetParameter reports DataType aws:region. Repeat for aws:account:id, boolean, integer, float, binary, hostname, email, ipv4-address, ipv6-address, then with a malformed value per type and compare the per-type messages.
3. Cause 2: the same call with Tier="Standard" and DataType="url" (then json, jsonc). Observe the unsupported-type message instead of "The url data type is only supported for advanced parameters.".
4. Cause 3: PutParameter an Advanced-tier parameter, PutResourcePolicy a policy on its ARN, then call PutResourcePolicy again with a different policy and no PolicyId. Observe HTTP 200 and a second PolicyId; AWS refuses ValidationException "Resource policy for parameter <arn> already exists" and GetResourcePolicies still lists only the first policy.
5. Cause 4: PutResourcePolicy with the correct PolicyId but a PolicyHash that is no longer current. Observe ResourcePolicyConflictException instead of ResourcePolicyNotFoundException.
6. Cause 5: CreateDocument a one-version document, then UpdateDocumentDefaultVersion with DocumentVersion="7". Observe 'Document version: 7 is invalid, latest version is 1' instead of 'Document with name <doc> with version 7 does not exist.'.
7. Cause 6: UpdateOpsMetadata with OpsMetadataArn=<any well-formed arn> and MetadataToUpdate={"   ": {"Value": "bad"}}. Compare the rendered map value and pattern with AWS's.
8. Re-replay the 7 checks listed in trace_ids.
