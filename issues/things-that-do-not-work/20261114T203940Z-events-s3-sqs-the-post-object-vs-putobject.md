Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__s3__sqs.a-post-object-upload-is-withheld-from-a-putobject-scoped-rule
Issue-Key: events__s3__sqs.a-post-object-upload-is-withheld-from-a-putobject-scoped-rule|_drain#36|Timeout|resp = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: 15ae58289b579bbcf6fa00301660f66a36f4498e

# events__s3__sqs: the POST-Object-vs-PutObject reason-scoped EventBridge routing scenario never reaches a verdict — the check makes no progress and is cut off at the 60 s replay cap

> _Note: this issue was identified against the code base at commit `15ae58289b579bbcf6fa00301660f66a36f4498e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PostObject

## Description
Cause 1 of 1: an S3 browser form upload (POST Object) produces no observable 'Object Created' event whose detail.reason discriminates it from a PutObject creation, and the scenario exercising that discrimination makes no progress at all — the replay harness kills it at its 60 s cap with no assertion ever reached.

AWS: POST Object is S3's browser-based HTML-form upload. It is a REST-only operation and is NOT in the Smithy model (recorded/documented behavior only — https://docs.aws.amazon.com/AmazonS3/latest/API/API_PostObject.html): an HTTP POST to the bucket root carrying a multipart/form-data body with the fields key, policy, x-amz-algorithm, x-amz-credential, x-amz-date, x-amz-signature and success_action_status, no Authorization header and no X-Amz-Target. When every policy condition holds and success_action_status=201 is present, S3 answers HTTP 201 and the object exists (HeadObject reports ContentLength equal to the posted bytes). When the bucket carries an EventBridge notification configuration (Smithy: PutBucketNotificationConfiguration.NotificationConfiguration.EventBridgeConfiguration, PUT /{Bucket}?notification), S3 emits exactly one event per creation to the default bus with source 'aws.s3', detail-type 'Object Created', and detail.reason naming the API that created the object — the documented literals are 'PutObject', 'POST Object', 'CopyObject' and 'CompleteMultipartUpload' (https://docs.aws.amazon.com/AmazonS3/latest/userguide/EventBridge.html). The reason is therefore the discriminator two EventBridge rules filter on: a rule whose pattern pins detail.reason to ['PutObject'] receives plain PutObject creations and must receive NOTHING for a form upload, and a rule pinned to ['POST Object'] receives the form upload and nothing else. detail.bucket.name is the addressed bucket.

Emulator: the check is terminated with 'Check timed out after 60.0s' having produced no assertion failure, i.e. it never completed a single beat of the scenario within the replay budget (the check's own worst-case budget is 900 s and its individual polls allow 180 s). Nothing in the run shows either leg of the reason discrimination being satisfied. Two emulator behaviours already recorded in the checkout are consistent with it never progressing: the open issue 20261028T105230Z-s3-8-net-new-root-causes-in-the.md records, as its Cause 1, that POST Object is not routed at all — a conforming multipart/form-data POST to a bucket root is answered HTTP 404 with the non-AWS JSON body {"__type":"UnknownService","message":"could not resolve a target service from the request (no SigV4 credential scope, X-Amz-Target, or Host header)"} instead of 201 — and the open issue 20261113T151530Z-events-s3-sqs-per-event-type-s3-queue.md records that per-event-type S3 notification routing does not settle. NEITHER issue carries this check's replay key and neither covers the EventBridge detail.reason discrimination this check is about, so this cause is filed separately and named against them.

Why: a client cannot use or regression-test the presigned-POST direct-upload pattern at all, and — the point of this scenario — it cannot rely on detail.reason to tell apart how an object was created. An EventBridge rule pinned to reason 'PutObject' is the standard way to ignore browser uploads (or to handle them separately); against the emulator such a rule either sees nothing at all or cannot be shown to be correctly withholding, so an isolation/routing regression in production event wiring ships unnoticed. A caller also gets a 404 UnknownService JSON body where AWS returns 201 with a <PostResponse> document, which no AWS SDK or browser form can interpret.

Fix: the request router's service attribution must recognise an S3 POST form upload (a multipart/form-data POST whose target is a bucket root, bearing x-amz-credential in the form rather than an Authorization header) and dispatch it to a POST Object handler in the s3 crate that verifies the form policy and signature and honours success_action_status; that handler's object-creation side effect must then flow through the same S3→EventBridge notification emitter used by PutObject, with detail.reason set to the literal 'POST Object' (and 'PutObject' for PutObject), so the events crate's rule-pattern matcher can discriminate on it. The emitter must publish exactly one event per creation so a rule pinned to the other reason stays empty.

Traces: events__s3__sqs.a-post-object-upload-is-withheld-from-a-putobject-scoped-rule.

## Evidence
Check: events__s3__sqs.a-post-object-upload-is-withheld-from-a-putobject-scoped-rule (verdict: mismatch).

Actual (emulator): 'Error: Check timed out after 60.0s' — the check produced no assertion result; it was cut off at the replay harness cap having completed none of its four beats. Its own declared worst case is _WORST_CASE_BUDGET_S = 900 and each of its polls allows timeout_s=180.

Expected (AWS), per beat of the scenario:
- Beat 1: put_object on a bucket with EventBridgeConfiguration set -> the rule whose pattern is {"source":["aws.s3"],"detail-type":["Object Created"],"detail":{"bucket":{"name":[<bucket>]},"reason":["PutObject"]}} receives the event, detail.reason == "PutObject"; the rule pinned to reason "POST Object" receives nothing.
- Beat 2: a SigV4-signed multipart/form-data POST Object to the bucket root with success_action_status=201 answers HTTP 201 (not 404); head_object then reports ContentLength equal to the posted payload length; the POST-Object-scoped rule receives an event with detail.reason == "POST Object" and detail.bucket.name == <bucket>.
- Beat 3: the PutObject-scoped queue stays empty over a hold of 3x the measured S3->EventBridge->SQS latency, and its ApproximateNumberOfMessages* total is 0.
- Beat 4: a second put_object still reaches the PutObject-scoped rule, and that queue carries exactly the new key.

Related already-open issues that do NOT carry this check and do not cover the detail.reason discrimination: 20261028T105230Z-s3-8-net-new-root-causes-in-the.md (Cause 1: POST Object answered HTTP 404 {"__type":"UnknownService",...} instead of 201) and 20261113T151530Z-events-s3-sqs-per-event-type-s3-queue.md (per-event-type S3 queue notification routing never settles).

## Steps to Reproduce
1. Replay the behavioral suite and re-run exactly the check id in trace_ids (events__s3__sqs.a-post-object-upload-is-withheld-from-a-putobject-scoped-rule).
2. Manually: create a bucket and call PutBucketNotificationConfiguration with NotificationConfiguration={"EventBridgeConfiguration": {}}; confirm GetBucketNotificationConfiguration reports EventBridgeConfiguration.
3. Create two SQS queues with a policy allowing events.amazonaws.com sqs:SendMessage, and two ENABLED EventBridge rules on the default bus with patterns {"source":["aws.s3"],"detail-type":["Object Created"],"detail":{"bucket":{"name":[<bucket>]},"reason":["PutObject"]}} and the same with reason ["POST Object"], each targeting one queue.
4. put_object into the bucket and poll the PutObject queue: the event must arrive with detail.reason == "PutObject" and the POST-Object queue must stay empty.
5. Build a SigV4-signed POST policy form (fields x-amz-algorithm, x-amz-credential, x-amz-date, success_action_status=201, policy, x-amz-signature, then key, then file) and POST it as multipart/form-data to the bucket root URL. Observe the status: AWS answers 201; the emulator does not route POST Object at all.
6. head_object the posted key (AWS: ContentLength equals the payload) and poll the POST-Object queue for an event with detail.reason == "POST Object".
7. Hold on the PutObject queue for 3x the measured delivery latency: it must stay empty and report queue depth 0; then put_object again and confirm the PutObject rule is still live and carries only the new key.
