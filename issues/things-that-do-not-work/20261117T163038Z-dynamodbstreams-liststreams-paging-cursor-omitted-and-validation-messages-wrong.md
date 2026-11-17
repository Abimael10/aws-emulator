Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodbstreams-describe-stream-missing-streamarn-rejected
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodbstreams-describe-stream-missing-streamarn-rejected dynamodbstreams-get-records-malformed-iterator-over-limit dynamodbstreams-list-streams-limit-floor-rejected
Issue-Shape-Id: com.amazonaws.dynamodbstreams#DynamoDBStreams_20120810
Issue-Key: dynamodbstreams|DescribeStream|4xx->4xx|$.message
Issue-Key: dynamodbstreams|GetRecords|4xx->4xx|$.message
Issue-Key: dynamodbstreams|ListStreams|2xx->2xx|$.LastEvaluatedStreamArn
Issue-Commit: 81e21ca007588719de802b4dd67d83498ccb7823

# ListStreams paging cursor omitted and validation messages wrong

> _Note: this issue was identified against the code base at commit `81e21ca007588719de802b4dd67d83498ccb7823`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStream

## Description
The emulator's DynamoDB Streams front end composes its own request-rejection text instead of the service's, and drops the ListStreams pagination cursor when a page is truncated. Statuses and error codes match AWS on every step below; the divergence is in the error message text, in which parameter is validated first, and in one output member. Three causes, all must be closed.

Cause 1 of 3 - DescribeStream with a null StreamArn reports the generic framework null-check message instead of the service's message.
AWS: Smithy: DescribeStream.StreamArn targets StreamArn, @required, @length min 37 max 1024. For a request that omits the member entirely, the recorded response is HTTP 400 with the message 'Invalid StreamArn' - the whole message is fixed text for this error class, with no variable part interpolated from the request. (The validation-framework form, '1 validation error detected: Value null at '<member>' failed to satisfy constraint: Member must not be null', is what AWS emits for services whose front end runs generic constraint validation; DynamoDB Streams does not use it for this member - recorded behavior only.)
Emulator: HTTP 400, same error code, message "1 validation error detected: Value null at 'streamArn' failed to satisfy constraint: Member must not be null".
Why: clients and test suites that branch on, log, or assert the error message for a missing stream ARN see text AWS never produces for this operation; message-matching retry/diagnostic logic does not fire.
Fix: DescribeStream's input validation (the missing/absent-StreamArn rejection path) must raise the service-specific message rather than falling through to the generic required-member validator.
Traces: dynamodbstreams-describe-stream-missing-streamarn-rejected#task#1

Cause 2 of 3 - GetRecords validates Limit before ShardIterator, so a request that is wrong in both ways is rejected for the wrong parameter.
AWS: Smithy: GetRecords.ShardIterator targets ShardIterator, @required, @length min 1 max 2048; GetRecords.Limit targets PositiveIntegerObject @range min 1. For a request carrying a malformed/undecodable shard iterator together with a Limit above the service ceiling, the recorded response is HTTP 400 with message 'Invalid ShardIterator' - i.e. the iterator is validated first and its rejection message is fixed text with no variable part. The over-limit message AWS uses when the iterator is valid, 'One or more parameter values were invalid: Limit should be less than or equal to 1000', has fixed text with the ceiling (1000) as its only value-like part; it is not the message for this request.
Emulator: HTTP 400, same error code, message 'One or more parameter values were invalid: Limit should be less than or equal to 1000' - the Limit check runs first and masks the invalid iterator.
Why: a client whose stored iterator has gone bad is told its Limit is at fault. It retries with a smaller Limit forever instead of re-acquiring a shard iterator via GetShardIterator, so the consumer stalls on that shard.
Fix: GetRecords' input validation - order the checks so ShardIterator decoding/validity is rejected before the Limit bound.
Traces: dynamodbstreams-get-records-malformed-iterator-over-limit#task#6

Cause 3 of 3 - ListStreams omits LastEvaluatedStreamArn when the result set is truncated by Limit.
AWS: Smithy: ListStreamsOutput.LastEvaluatedStreamArn targets StreamArn (@length min 37 max 1024) - the stream ARN of the item where the operation stopped, inclusive of the previous result set, to be passed as ExclusiveStartStreamArn on the next call; it is absent only when the last page has been processed. In the recording, a ListStreams call whose page is cut short by Limit returns HTTP 200 with the member present, carrying the ARN of the last stream in the returned page (that ARN value is generated per environment and is not itself a requirement - the requirement is that the member is emitted whenever more streams remain).
Emulator: HTTP 200 with Streams populated but LastEvaluatedStreamArn absent.
Why: absence is AWS's end-of-results signal, so a client paging streams concludes it has enumerated every stream after the first truncated page. Streams beyond the first page are invisible and never consumed.
Fix: ListStreams' handler paging logic plus its output serializer - set the cursor from the last stream included in the page whenever the unreturned remainder is non-empty. Note the already-open issue 20261029T172902Z-dynamodbstreams-describestream-projection-omits-keyschema-type-and-never-emits.md reports the analogous omission of StreamDescription.LastEvaluatedShardId on DescribeStream; it does not cover ListStreams, which is a separate operation, output shape and paging path.
Traces: dynamodbstreams-list-streams-limit-floor-rejected#task#4

## Evidence
Replay of trace set simparity-wire-aws-v1 (14 fixtures, 97 steps, 83 passed, 13 mismatched, score 85.6%).

Cause 1 - fixture dynamodbstreams-describe-stream-missing-streamarn-rejected step task#1, DescribeStream, expected status 400 / actual 400: $.message value_mismatch - expected 'Invalid StreamArn', got "1 validation error detected: Value null at 'streamArn' failed to satisfy constraint: Member must not be null". Divergence key service=dynamodbstreams|DescribeStream|status=400->400|keys=$.message.

Cause 2 - fixture dynamodbstreams-get-records-malformed-iterator-over-limit step task#6, GetRecords, expected status 400 / actual 400: $.message value_mismatch - expected 'Invalid ShardIterator', got 'One or more parameter values were invalid: Limit should be less than or equal to 1000'. Divergence key service=dynamodbstreams|GetRecords|status=400->400|keys=$.message.

Cause 3 - fixture dynamodbstreams-list-streams-limit-floor-rejected step task#4, ListStreams, expected status 200 / actual 200: $.LastEvaluatedStreamArn missing - expected a stream ARN of the form arn:aws:dynamodb:<region>:<account>:table/<table>/stream/<label>, got None. Divergence key service=dynamodbstreams|ListStreams|status=200->200|keys=$.LastEvaluatedStreamArn.

Smithy (services/service-com-amazonaws-dynamodbstreams-dynamodbstreams-20120810): DescribeStream.StreamArn @required @length 37..1024; GetRecords.ShardIterator @required @length 1..2048, GetRecords.Limit @range min 1; ListStreamsOutput.LastEvaluatedStreamArn -> StreamArn @length 37..1024.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Cause 1: replay dynamodbstreams-describe-stream-missing-streamarn-rejected step task#1 - call DescribeStream with no StreamArn member and compare the 400 body's message against 'Invalid StreamArn'.
4. Cause 2: replay dynamodbstreams-get-records-malformed-iterator-over-limit step task#6 - call GetRecords with a malformed ShardIterator AND a Limit above 1000; the 400 message must be 'Invalid ShardIterator', not the Limit message.
5. Cause 3: replay dynamodbstreams-list-streams-limit-floor-rejected step task#4 - with more streams present than the requested Limit, call ListStreams and check the 200 body for LastEvaluatedStreamArn; it is absent.
