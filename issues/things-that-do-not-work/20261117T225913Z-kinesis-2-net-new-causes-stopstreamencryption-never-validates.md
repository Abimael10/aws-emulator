Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kinesis.stop-stream-encryption-with-a-nonexistent-key-id-rejected kinesis.consumer-resource-tags-lifecycle-independent-of-stream-tags
Issue-Shape-Id: com.amazonaws.kinesis#Kinesis_20131202
Issue-Key: kinesis.stop-stream-encryption-with-a-nonexistent-key-id-rejected|_error#12|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: kinesis|ListTagsForResource|ResourceNotFoundException
Issue-Commit: 75900fccf35b95945ac4e89ca5619cc881e6a687

# kinesis: 2 net-new causes — StopStreamEncryption never validates KeyId (a nonexistent key disables encryption with 200), and ListTagsForResource/RegisterStreamConsumer.Tags do not resolve a stream-consumer ARN

> _Note: this issue was identified against the code base at commit `75900fccf35b95945ac4e89ca5619cc881e6a687`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StopStreamEncryption

## Description
Two net-new root causes in the kinesis service, both grounded in failing behavioral checks of this run. Neither is covered by an open issue (the nearest ones are named per cause below).

Cause 1 of 2: StopStreamEncryption accepts a KeyId that names no KMS key — and that does not even match the key the stream is encrypted under — and disables encryption with HTTP 200 instead of refusing it. Failing check: kinesis.stop-stream-encryption-with-a-nonexistent-key-id-rejected.

AWS: Smithy (com-amazonaws-kinesis-kinesis-20131202) models StopStreamEncryption.KeyId as @required, @length 1..2048, documented as "The GUID for the customer-managed Amazon Web Services KMS key to use for encryption. This value can be a globally unique identifier, a fully specified Amazon Resource Name (ARN) to either an alias or a key, or an alias name prefixed by \"alias/\"", and the operation models InvalidArgumentException (HTTP 400) — "A specified parameter exceeds its restrictions, is not supported, or can't be used. For more information, see the returned message." Recorded behavior: a request whose KeyId is a syntactically valid but nonexistent key ARN in the caller's own account and region (arn:<partition>:kms:<region>:<account>:key/<uuid>) is refused with HTTP 400, Error.Code InvalidArgumentException and Error.Message exactly "Invalid keyId." — that message is entirely fixed text, it interpolates nothing from the request. The refusal leaves the stream exactly as it was: DescribeStreamSummary still reports StreamStatus ACTIVE, EncryptionType KMS and the KeyId the stream is actually encrypted under. The sibling StartStreamEncryption already resolves its KeyId through KMS in this emulator (ops/start_stream_encryption.rs, kms_not_found()), so key resolution is reachable from this crate; StopStreamEncryption simply never asks.

Emulator: StopStreamEncryption ignores KeyId altogether. It validates only EncryptionType, the stream lookup, the stream status, that the stream is encrypted at all, and the 25-per-24h change window; it then calls stream.stop_encryption(now) without ever comparing or resolving KeyId. The replayed call with the absent key ARN answered HTTP 200 with the modelled empty body ({}), and encryption was switched off. Recorded replay detail: "AssertionError: expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, ...'content-length': '2'...}}".

Why: a client that passes the wrong key id — a stale key, a key from another account, a typo — gets a success where AWS refuses, and the stream silently stops being encrypted. Code paths that rely on the refusal to detect "this is not the key this stream uses" (key-rotation tooling, compliance checks that assert the stream stays KMS-encrypted unless the correct key is presented) take the 200 as confirmation and proceed, and the stream's encryption state diverges from what the caller believes. There is also no way for a caller to observe InvalidArgumentException/"Invalid keyId." from this operation at all.

Fix: the StopStreamEncryption handler's validation — the refusal set computed before the write must resolve input.KeyId (key id, key ARN, alias name or alias ARN, including alias/aws/kinesis) the same way StartStreamEncryption's KMS lookup does, and answer InvalidArgumentException with message "Invalid keyId." for a KeyId that resolves to nothing, before stop_encryption() mutates the stream. Nothing about the stream may change on that path.

Traces: kinesis.stop-stream-encryption-with-a-nonexistent-key-id-rejected. Related but NOT covering this cause: the fixed issue 20261027T095220Z-kinesis-6-net-new-root-causes-startstreamencryption-accepts.md fixed the mirror-image gap on StartStreamEncryption only, and the fixed 20261027T064010Z-kinesis-stopstreamencryption-answers-501-notimplemented-so-the-cannot.md covered only this operation's unencrypted-stream refusal. This is not a REOPENS: the call is not refused, it wrongly succeeds. Source: services/service-com-amazonaws-kinesis-kinesis-20131202/src/ops/stop_stream_encryption.rs lines 233-313 (no KeyId arm anywhere in stop_stream_encryption()); contrast services/service-com-amazonaws-kinesis-kinesis-20131202/src/ops/start_stream_encryption.rs lines 348-356.

Cause 2 of 2: ListTagsForResource (and UntagResource, and the Tags seeded by RegisterStreamConsumer) do not resolve a stream-CONSUMER ARN, so a consumer registered with tags has no readable tag set and every generic tag read on it is refused ResourceNotFoundException. Failing check: kinesis.consumer-resource-tags-lifecycle-independent-of-stream-tags.

AWS: Smithy models RegisterStreamConsumer.Tags (TagMap, @length 1..200) so a consumer can be tagged AT registration, and ListTagsForResource.ResourceARN is @length 1..2048 @pattern ^arn:aws.*:kinesis:.*:\d{12}:.*(stream|channel)/\S+$ — a consumer ARN of the form arn:aws:kinesis:<region>:<account>:stream/<stream-name>/consumer/<consumer-name>:<registration-epoch> satisfies that pattern, so it is in-contract input. Recorded behavior: after RegisterStreamConsumer(StreamARN=..., ConsumerName=..., Tags={...}) and the consumer reaching ConsumerStatus ACTIVE, ListTagsForResource(ResourceARN=<ConsumerARN>) answers HTTP 200 and lists exactly the tags registration carried; the parent stream's own tags are unaffected, and TagResource/UntagResource on that consumer ARN add and remove keys in that separate tag set. ResourceNotFoundException is reserved for an ARN this account and region hold no resource under.

Emulator: ListTagsForResource on the ConsumerARN that RegisterStreamConsumer just returned, for an ACTIVE consumer on an ACTIVE stream, answers HTTP 400 with Error.Code ResourceNotFoundException and message "Consumer <consumerName> with creation timestamp <epochSeconds> under stream: <streamName>, account <accountId> not found." — fixed text "Consumer ", " with creation timestamp ", " under stream: ", ", account ", " not found."; the variable parts are the consumer name, registration epoch and stream name parsed out of the request's ResourceARN plus the caller's account id. Replay detail: "botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the ListTagsForResource operation: Consumer <consumer> with creation timestamp <epoch> under stream: <stream>, account 000000000000 not found." The handler resolves the ARN through the single store::resource_tags_by_arn lookup, which spans only streams and channels, and maps a miss straight to the not-found arm — so a consumer ARN can never resolve, and the tags RegisterStreamConsumer accepted are unreachable.

Why: tags on an enhanced-fan-out consumer are write-only and then unreadable: cost-allocation and ownership tagging of consumers cannot be verified, and a caller that reads back what it just registered sees a 400 that is indistinguishable from "the consumer does not exist", so it either loops polling the consumer or aborts the whole register → tag → subscribe → deregister lifecycle at the first read. UntagResource is unusable on consumers for the same reason.

Fix: the shared ARN→tag-set resolution behind the resource-tag family in the kinesis service state (resource_tags_by_arn / resource_tags_by_arn_mut) needs a consumer arm resolving a consumer ARN to that consumer's own tag set, with the consumer record carrying a tag set that RegisterStreamConsumer.Tags seeds, kept separate from the parent stream's tags; only an ARN naming no existing stream, channel or consumer should reach the not-found arm. ListTagsForResource/UntagResource then address a consumer through that same lookup.

Traces: kinesis.consumer-resource-tags-lifecycle-independent-of-stream-tags. Related open issue that does NOT cover this one: 20261113T210341Z-kinesis-tagresource-refuses-a-just-registered-stream-consumer.md carries replay key kinesis|TagResource|ResourceNotFoundException and is grounded in kinesis.e2e-stream-consumer-fanout-lifecycle (the TagResource write path only); this check fails on the READ path (ListTagsForResource) and on RegisterStreamConsumer.Tags never being stored, and is not listed by it. Source: services/service-com-amazonaws-kinesis-kinesis-20131202/src/ops/list_tags_for_resource.rs lines 186-206.

## Evidence
Cause 1 — StopStreamEncryption, nonexistent KeyId:
Expected (AWS): CreateStream + StartStreamEncryption(EncryptionType=KMS, KeyId=alias/aws/kinesis) leaves the stream ACTIVE with EncryptionType=KMS and KeyId echoed; StopStreamEncryption(StreamName=<s>, EncryptionType=KMS, KeyId=arn:aws:kms:<region>:<account>:key/00000000-0000-0000-0000-000000000000) answers HTTP 400, Error.Code=InvalidArgumentException, Error.Message="Invalid keyId." (fixed text, no interpolation), and DescribeStreamSummary still reports StreamStatus=ACTIVE, EncryptionType=KMS, KeyId=alias/aws/kinesis. Only StopStreamEncryption with the key the stream is actually encrypted under succeeds and moves it to EncryptionType=NONE with no KeyId.
Actual (emulator): that same StopStreamEncryption answers HTTP 200 with content-length 2 (the empty JSON body) and disables encryption. Replay detail: "AssertionError: expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/x-amz-json-1.1', 'content-length': '2', ...}}}". KeyId is never read by stop_stream_encryption() (ops/stop_stream_encryption.rs lines 233-313).

Cause 2 — consumer tag reads:
Expected (AWS): RegisterStreamConsumer(StreamARN=<stream-arn>, ConsumerName=<c>, Tags={"team":"ingest","env":"canary","project":"fanout"}) -> Consumer.ConsumerARN; after ConsumerStatus=ACTIVE, ListTagsForResource(ResourceARN=<ConsumerARN>) -> HTTP 200 listing exactly those three keys, while ListTagsForResource(ResourceARN=<stream-arn>) still lists only the stream's own tags; TagResource then adds a key to the consumer's set and UntagResource removes one, neither touching the stream's tags.
Actual (emulator): ListTagsForResource(ResourceARN=<ConsumerARN>) -> HTTP 400 ResourceNotFoundException "Consumer <consumerName> with creation timestamp <epochSeconds> under stream: <streamName>, account <accountId> not found." Replay detail: "botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the ListTagsForResource operation: Consumer <consumer> with creation timestamp <epoch> under stream: <stream>, account 000000000000 not found." (consumer name, epoch, stream name and account id are per-run values filling that template, not requirements). The consumer's registration tags are therefore never observable.

## Steps to Reproduce
Cause 1 (kinesis.stop-stream-encryption-with-a-nonexistent-key-id-rejected):
1. kinesis.CreateStream(StreamName=<s>, ShardCount=1, StreamModeDetails={StreamMode: PROVISIONED}); poll DescribeStreamSummary until StreamStatus==ACTIVE; note StreamARN.
2. kinesis.StartStreamEncryption(StreamName=<s>, EncryptionType="KMS", KeyId="alias/aws/kinesis"); poll until StreamStatus==ACTIVE and EncryptionType=="KMS".
3. kinesis.StopStreamEncryption(StreamName=<s>, EncryptionType="KMS", KeyId="arn:aws:kms:<region>:<account>:key/00000000-0000-0000-0000-000000000000").
   -> emulator: HTTP 200, empty body, encryption disabled.
   -> AWS: HTTP 400 InvalidArgumentException "Invalid keyId.", stream unchanged (ACTIVE / KMS / alias/aws/kinesis).
4. kinesis.DescribeStreamSummary(StreamName=<s>) to confirm the stream kept EncryptionType==KMS on AWS.

Cause 2 (kinesis.consumer-resource-tags-lifecycle-independent-of-stream-tags):
1. kinesis.CreateStream(StreamName=<s>, ShardCount=1); wait ACTIVE; arn = DescribeStreamSummary(<s>).StreamDescriptionSummary.StreamARN.
2. kinesis.TagResource(ResourceARN=arn, Tags={"owner":"data-platform","env":"staging"}); ListTagsForResource(ResourceARN=arn) lists those two.
3. consumer_arn = kinesis.RegisterStreamConsumer(StreamARN=arn, ConsumerName=<c>, Tags={"team":"ingest","env":"canary","project":"fanout"}).Consumer.ConsumerARN; poll DescribeStreamConsumer(ConsumerARN=consumer_arn) until ConsumerStatus==ACTIVE.
4. kinesis.ListTagsForResource(ResourceARN=consumer_arn).
   -> emulator: HTTP 400 ResourceNotFoundException "Consumer <c> with creation timestamp <epoch> under stream: <s>, account <acct> not found."
   -> AWS: HTTP 200 listing the three registration tags.
5. kinesis.TagResource(ResourceARN=consumer_arn, Tags={"stage":"rollout"}) then UntagResource(ResourceARN=consumer_arn, TagKeys=["env"]) and re-read; AWS reflects each change on the consumer only, leaving the stream's two tags untouched.
Replay: run the behavioral checks kinesis.stop-stream-encryption-with-a-nonexistent-key-id-rejected and kinesis.consumer-resource-tags-lifecycle-independent-of-stream-tags.
