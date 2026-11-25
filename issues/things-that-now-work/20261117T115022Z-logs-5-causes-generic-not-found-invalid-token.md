Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs.delete-data-protection-policy-nonexistent-log-group-fails logs.describe-field-indexes-nonexistent-log-group-fails logs.describe-log-streams-nexttoken-from-other-group-rejected logs.start-live-tail-log-group-count-boundary-rejected logs.start-query-without-log-group-selector-rejected
Issue-Shape-Id: com.amazonaws.cloudwatchlogs#Logs_20140328
Issue-Key: logs.delete-data-protection-policy-nonexistent-log-group-fails|run#14|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: logs.describe-field-indexes-nonexistent-log-group-fails|run#11|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: cloudwatch-logs|DescribeLogStreams|InvalidParameterException
Issue-Key: logs.start-live-tail-log-group-count-boundary-rejected|run#17|AssertionError|assert message == expected, f"expected {expected!r}, got {message!r}"
Issue-Key: logs.start-query-without-log-group-selector-rejected|run#14|AssertionError|assert code == "MalformedQueryException", f"expected MalformedQueryException, got {code!r}: {message!r}"
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# logs: 5 causes — generic not-found/invalid-token refusal text on four operations and StartQuery's missing-log-group-selector refused with the wrong error class

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartQuery

## Description
Five net-new refusal divergences in CloudWatch Logs. Each operation's error CLASS/code is otherwise right; what diverges is the message AWS words per operation (and, in cause 5, the class itself).

Cause 1 of 5: DeleteDataProtectionPolicy's not-found message.
AWS: Smithy logs DeleteDataProtectionPolicy models ResourceNotFoundException (client, HTTP 400); for a log group that was never created the message is "Log group '<logGroupName>' for owner '<accountId>' cannot be found" — fixed text around two values taken from the request and the caller's account.
Emulator: ResourceNotFoundException with the generic sentence 'The specified log group does not exist.'
Fix: the DeleteDataProtectionPolicy handler's not-found refusal message.
Failing check: logs.delete-data-protection-policy-nonexistent-log-group-fails

Cause 2 of 5: DescribeFieldIndexes' not-found message.
AWS: Smithy logs DescribeFieldIndexes models ResourceNotFoundException; for a logGroupIdentifier that does not exist the message is the fixed sentence 'Log group cannot be found.' (no variable parts).
Emulator: 'The specified log group does not exist.'
Fix: the DescribeFieldIndexes handler's not-found refusal message.
Failing check: logs.describe-field-indexes-nonexistent-log-group-fails

Cause 3 of 5: DescribeLogStreams' invalid-pagination-token message.
AWS: Smithy logs DescribeLogStreams models InvalidParameterException and is @paginated on nextToken; a token minted by a call on another log group is refused InvalidParameterException with the fixed sentence 'Pagination token in request is invalid. Only token from previous list call or null is valid.'
Emulator: InvalidParameterException 'The specified nextToken is invalid.'
Fix: the DescribeLogStreams handler's pagination-token validation message.
Failing check: logs.describe-log-streams-nexttoken-from-other-group-rejected

Cause 4 of 5: StartLiveTail's constraint violation omits the offending value.
AWS: Smithy logs StartLiveTail constrains logGroupIdentifiers to length 1..10, and the framework refusal names the value that failed: "1 validation error detected: Value '[<the submitted list, comma-separated>]' at 'logGroupIdentifiers' failed to satisfy constraint: Member must have length less than or equal to 10" — the fixed text is the standard validation envelope, and the quoted value is the list from the request.
Emulator: the same envelope with the value elided: "1 validation error detected: Value at 'logGroupIdentifiers' failed to satisfy constraint: Member must have length less than or equal to 10".
Fix: the input-constraint validation serializer for logs (include the rejected value in the validation message).
Failing check: logs.start-live-tail-log-group-count-boundary-rejected

Cause 5 of 5: StartQuery without a log-group selector is refused with the wrong error class.
AWS: Smithy logs StartQuery binds both MalformedQueryException and InvalidParameterException; a StartQuery that names no log group in any form (logGroupName, logGroupNames, logGroupIdentifiers, or a SOURCE command in queryString) is refused MalformedQueryException — the query itself is malformed, not a parameter out of range.
Emulator: InvalidParameterException with the message 'Exactly one of logGroupName, logGroupNames, logGroupIdentifiers, or a SOURCE command in the query string, must specify the log groups to query.'
Why (all five): these messages and classes are the API's only account of WHY a call was refused. Clients branch on the exception class (cause 5 changes which typed exception is raised, so a MalformedQueryException handler never fires), surface the message to operators, and in the live-tail case parse the rejected value out of the validation text. A message that differs from AWS's wording is a divergence even when the code matches.
Fix: the StartQuery handler's query validation (raise the malformed-query error class).
Failing check: logs.start-query-without-log-group-selector-rejected

## Evidence
Cause 1: [mismatch] logs.delete-data-protection-policy-nonexistent-log-group-fails — expected message "Log group '<name>' for owner '<account>' cannot be found", got 'The specified log group does not exist.'
Cause 2: [mismatch] logs.describe-field-indexes-nonexistent-log-group-fails — expected 'Log group cannot be found.', got 'The specified log group does not exist.'
Cause 3: [mismatch] logs.describe-log-streams-nexttoken-from-other-group-rejected — expected 'Pagination token in request is invalid. Only token from previous list call or null is valid.', got 'The specified nextToken is invalid.' (code InvalidParameterException in both).
Cause 4: [mismatch] logs.start-live-tail-log-group-count-boundary-rejected — expected "1 validation error detected: Value '[<11 log-group ARNs from the request>]' at 'logGroupIdentifiers' failed to satisfy constraint: Member must have length less than or equal to 10", got "1 validation error detected: Value at 'logGroupIdentifiers' failed to satisfy constraint: Member must have length less than or equal to 10".
Cause 5: [mismatch] logs.start-query-without-log-group-selector-rejected — expected MalformedQueryException, got 'InvalidParameterException': 'Exactly one of logGroupName, logGroupNames, logGroupIdentifiers, or a SOURCE command in the query string, must specify the log groups to query.'

## Steps to Reproduce
1. DeleteDataProtectionPolicy(logGroupIdentifier=<never-created name>) and read the ResourceNotFoundException message.
2. DescribeFieldIndexes(logGroupIdentifiers=[<never-created name>]) and read the message.
3. Create two log groups with streams, take a nextToken from DescribeLogStreams on the first, and replay it against the second.
4. StartLiveTail with 11 logGroupIdentifiers and read the validation message.
5. StartQuery with a queryString that has no SOURCE command and none of logGroupName/logGroupNames/logGroupIdentifiers, and read the error code.
