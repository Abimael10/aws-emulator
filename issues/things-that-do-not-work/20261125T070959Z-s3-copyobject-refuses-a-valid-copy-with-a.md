Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3.s3-concurrent-copy-while-the-source-is-being-deleted-never-yields-a-partial-object
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3.s3-concurrent-copy-while-the-source-is-being-deleted-never-yields-a-partial-object|run#13|AssertionError|assert code == ("NoSuchKey", "The specified key does not exist."), (
Issue-Commit: 2c87622153d52c1735efdd41ae2a08ec1437d782

# s3 CopyObject refuses a valid copy with a non-AWS `ValidationException` naming an emulator-invented 64 MiB per-bucket storage ceiling

> _Note: this issue was identified against the code base at commit `2c87622153d52c1735efdd41ae2a08ec1437d782`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CopyObject

## Description
Cause 1 of 1: CopyObject fails a legal copy with an error code that is not in the S3 wire vocabulary, because the emulator enforces a per-bucket total-bytes cap that AWS does not have.

Related but NOT covering: open issue 20261111T165323Z-s3-emulator-invented-storage-ceilings-refuse-valid-writes.md records the same invented ceilings reached through PutObject/UploadPart/ListObjects(V2)/ListObjectVersions. It does not carry this check's replay key and does not cover the CopyObject path, where the refusal additionally arrives as a wire-invalid error code. It should be fixed together with this one.

AWS: S3 imposes no per-bucket or per-account limit on the number of objects or the total bytes stored in a bucket, and CopyObject of an existing object into the same bucket succeeds (HTTP 200 with a CopyObjectResult body carrying ETag and LastModified). Smithy (services/service-com-amazonaws-s3-amazons3): CopyObject POST/PUT /{Bucket}/{Key+}?x-id=CopyObject, success code 200, output CopyObjectOutput; the ONLY modeled error is ObjectNotInActiveTierError (client, HTTP 403). S3 is restXml and serialises errors as <Error><Code>...</Code><Message>...</Message></Error>; `ValidationException` is not an S3 error code at all — it belongs to JSON-protocol services. When the copy source has been concurrently deleted, the only legal failure is HTTP 404 with Code `NoSuchKey` and Message `The specified key does not exist.` (fixed text; no variable part).

Emulator: an 8-way concurrent CopyObject of one 10 MiB source object into fresh destination keys in the same bucket is refused with error code `ValidationException` and the message `copying "<dest-key>" would leave the bucket "<bucket>" holding <total> bytes of objects, past the 67108864 bytes this emulator keeps per bucket`. Everything in that message is emulator-authored: the fixed text is the emulator's own prose, and `<dest-key>`, `<bucket>` and `<total>` come from the request and from emulator state; the 67108864 (64 MiB) limit is invented. In the replayed check the destination key was `race-dest-1.bin`, the bucket a per-run `sim2real-behav-bkt-*` name and the total 73400957 bytes — those are one run's values, not part of the requirement. No such refusal exists in AWS: 9 x 10 MiB in one bucket is unremarkable.

Why: a client doing any multi-copy or fan-out workload (backup, batch copy, versioned re-write) is told its request is invalid once the bucket passes 64 MiB. Because the code is `ValidationException` rather than any modeled S3 error, SDK error handling keyed on S3 codes (NoSuchKey, InvalidRequest, EntityTooLarge) does not match it, and botocore surfaces it as an unknown client error, so retry/branch logic that would handle a real S3 refusal cannot. It also masks the behaviour under test: a copy racing a source delete must end either whole or as NoSuchKey, and the invented ceiling makes that distinction unobservable.

Fix: CopyObject's handler and the shared object-store state that accounts bucket size — remove the per-bucket byte ceiling (and the matching per-bucket object-count ceiling) so writes are bounded only by real resources; and remove `ValidationException` from the S3 error serializer so every S3 refusal is an <Error><Code> from the S3 vocabulary.

Traces: s3.s3-concurrent-copy-while-the-source-is-being-deleted-never-yields-a-partial-object (mismatch) — asserts the only legal failure of a copy racing a source delete is ('NoSuchKey', 'The specified key does not exist.').

## Evidence
Expected (AWS): each of the 8 concurrent CopyObject calls either returns HTTP 200 with the source's bytes and ETag at the destination, or fails HTTP 404 <Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message></Error> with no destination object created. No size-based refusal exists.

Actual (emulator): CopyObject of `race-dest-1.bin` failed with error code `ValidationException` and message:
  copying "race-dest-1.bin" would leave the bucket "sim2real-behav-bkt-219686703aec" holding 73400957 bytes of objects, past the 67108864 bytes this emulator keeps per bucket

Check assertion that fired (check_s3_s3_concurrent_copy_while_the_source_is_being_deleted_never_yields_a_partial_object.py:132):
  assert code == ("NoSuchKey", "The specified key does not exist.")
  AssertionError: race-dest-1.bin's copy failed with ('ValidationException', 'copying ... past the 67108864 bytes this emulator keeps per bucket'); the only legal failure when the source is concurrently deleted is NoSuchKey

Two distinct defects in one response: (a) a storage ceiling AWS does not have, (b) an error code that is not an S3 error code.

## Steps to Reproduce
1. CreateBucket <bucket> (LocationConstraint = region when not us-east-1).
2. PutObject <bucket>/race-source.bin with 10 MiB of bytes; HeadObject confirms ContentLength 10485760.
3. From 8 threads, concurrently CopyObject Bucket=<bucket>, Key=race-dest-<i>.bin, CopySource={Bucket: <bucket>, Key: race-source.bin} for i in 1..8, while another thread DeleteObject's race-source.bin.
4. Observe that once the bucket's accounted bytes pass 67108864, CopyObject answers error code `ValidationException` with the emulator-authored per-bucket-ceiling message instead of either succeeding (200) or failing NoSuchKey.
5. Equivalent minimal repro without the race: PutObject a 10 MiB object, then CopyObject it to 7 new keys in the same bucket — the 7th copy is refused.
