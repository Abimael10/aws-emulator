Issue-Source: trace-sweep-divergence
Issue-Trace: transcribe-transcription-job-workflow
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: transcribe-transcription-job-workflow transcribe-call-analytics-job-workflow
Issue-Shape-Id: com.amazonaws.transcribe#Transcribe
Issue-Key: transcribe|StartTranscriptionJob|4xx->4xx|$.Message
Issue-Key: transcribe|StartCallAnalyticsJob|4xx->4xx|$.Message
Issue-Commit: b9ec11b758c0a88959ff4f7c1c663fc7da468908

# Transcribe start operations report the bucket-missing refusal where AWS reports the media URI points to no S3 object

> _Note: this issue was identified against the code base at commit `b9ec11b758c0a88959ff4f7c1c663fc7da468908`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartTranscriptionJob

## Description
Two net-new transcribe divergences, one root cause shared by two start operations: when the Amazon S3 location a start names for its INPUT MEDIA does not resolve because the BUCKET is absent, the emulator answers the bucket-missing sentence, while AWS answers the object sentence for an unresolvable media URI regardless of whether the bucket or the key is what is missing. Error code (BadRequestException) and HTTP status (400) match in both; only $.Message diverges. 2 causes.

Shared Smithy basis for both causes: StartTranscriptionJob (input Media required, Media.MediaFileUri; errors BadRequestException httpError 400, ConflictException 409, LimitExceededException 429, InternalFailureException 500) and StartCallAnalyticsJob (input Media required with MediaFileUri / RedactedMediaFileUri, OutputLocation @pattern ^(s3://|http(s*)://).+$; same four errors). BadRequestException doc: "Your request didn't pass one or more validation tests. ... See the exception message field for more information." Because that single code covers not-found, wrong-state and validation failures alike, the MESSAGE is the only discriminator a client has, and the per-class text is recorded behavior only.

Cause 1 of 2: StartTranscriptionJob pointed at media in a bucket that does not exist.
AWS: recorded behavior only — HTTP 400, __type BadRequestException, Message "The URI that you provided doesn't point to an S3 object. Make sure that the object exists and try your request again." The whole sentence pair is FIXED text; there are no variable parts (neither the bucket name, the key, nor the URI from the request is interpolated). AWS does not split the media-resolution failure into bucket-missing vs key-missing: the one object sentence covers a media URI that does not resolve.
Emulator: HTTP 400 BadRequestException with Message "The specified S3 bucket doesn't exist. Make sure the bucket exists and try your request again." — also entirely fixed text, but the sentence for a DIFFERENT failure class.
Why: a client (or an operator) keyed on AWS's text does not recognise the refusal, and is told the bucket is the problem where AWS tells it the media object is. Automation that branches missing-bucket vs missing-object on the message takes the wrong path, and the message is the only signal available because the code and status are identical for every 400 of this operation.
Fix: the media-resolution refusal of the transcribe handler — the shared absent-address decision the job starts read (crate::state media resolution returning MediaVerdict::Absent with the per-code sentence), which must yield the object sentence for every unresolvable media address. Caution for the fixer: the bucket sentence IS recorded for StartMedicalTranscriptionJob (open issue 20261029T200807Z-transcribe-refusal-wording-is-reworded-on-four-operations.md, its Cause 2), and that recording names a nonexistent OutputBucketName as well as absent media. Hypothesis (stated by neither the model nor the recordings): AWS grounds the bucket sentence in validation of the OUTPUT bucket the request names, not in resolution of the input media, so the bucket sentence belongs in a separate OutputBucketName existence check rather than in the media read. Simply deleting the bucket branch from the media path would regress that medical step, so both must be satisfied together.
Traces: transcribe-transcription-job-workflow#task#2

Cause 2 of 2: StartCallAnalyticsJob pointed at media in a bucket that does not exist — same wording defect, second operation.
AWS: recorded behavior only — HTTP 400, __type BadRequestException, Message "The URI that you provided doesn't point to an S3 object. Make sure that the object exists and try your request again." Fixed text: the whole sentence pair; variable parts: none.
Emulator: HTTP 400 BadRequestException with Message "The specified S3 bucket doesn't exist. Make sure the bucket exists and try your request again."
Why: as Cause 1, for the Call Analytics family: a start whose input audio cannot be resolved is misdiagnosed, so a caller is sent to create a bucket instead of to upload (or re-address) the media file.
Fix: same single seam — this operation refuses through the same shared absent-address decision (its handler resolves Media.MediaFileUri and Media.RedactedMediaFileUri through it), so fixing the shared sentence fixes both; this step must still be re-replayed on its own because it is a distinct recorded step.
Traces: transcribe-call-analytics-job-workflow#task#2

Not covered by any open issue: 20261029T200807Z-transcribe-refusal-wording-is-reworded-on-four-operations.md owns the StartMedicalTranscriptionJob message key (and four other message keys) but neither of the two steps above; 20261111T185526Z-transcribe-starttranscriptionjob-refuses-media-uploaded-to-s3-breaking.md owns the StartTranscriptionJob 2xx->4xx keys of a different fixture, not this error-path message.

## Evidence
Cause 1 (fixture transcribe-transcription-job-workflow, step task#2, StartTranscriptionJob, 400 -> 400): $.Message value_mismatch — expected "The URI that you provided doesn't point to an S3 object. Make sure that the object exists and try your request again.", got "The specified S3 bucket doesn't exist. Make sure the bucket exists and try your request again.". Both strings are entirely fixed text with no request- or state-derived parts; they denote different failures. HTTP status 400 and error code BadRequestException matched.
Cause 2 (fixture transcribe-call-analytics-job-workflow, step task#2, StartCallAnalyticsJob, 400 -> 400): $.Message value_mismatch — expected "The URI that you provided doesn't point to an S3 object. Make sure that the object exists and try your request again.", got "The specified S3 bucket doesn't exist. Make sure the bucket exists and try your request again.". Status and error code matched.
Emulator code path (both): services/service-com-amazonaws-transcribe-transcribe/src/state/jobs.rs lines 794-839 — URI_NAMES_NO_OBJECT, S3_BUCKET_DOES_NOT_EXIST and absent_address_message(code), which answers S3_BUCKET_DOES_NOT_EXIST whenever the in-process HeadObject carried the NoSuchBucket code; reached from services/service-com-amazonaws-transcribe-transcribe/src/ops/start_transcription_job.rs lines 493-503 and services/service-com-amazonaws-transcribe-transcribe/src/ops/start_call_analytics_job.rs lines 345-357. No OutputBucketName existence check exists anywhere in that crate, so the bucket sentence is reachable only from the media read.

## Steps to Reproduce
1. Do NOT create the bucket. Call StartTranscriptionJob(TranscriptionJobName=<a new valid name>, LanguageCode='en-US', Media={MediaFileUri:'s3://<bucket-that-was-never-created>/<key>'}) with no OutputBucketName, and read Message of the 400: AWS answers "The URI that you provided doesn't point to an S3 object. Make sure that the object exists and try your request again."; the emulator answers "The specified S3 bucket doesn't exist. Make sure the bucket exists and try your request again.".
2. Call StartCallAnalyticsJob(CallAnalyticsJobName=<a new valid name>, Media={MediaFileUri:'s3://<bucket-that-was-never-created>/<key>'}, DataAccessRoleArn=<a role arn>) and read Message of the 400: same expected vs actual divergence.
3. Regression check that must keep passing: StartMedicalTranscriptionJob with absent media AND a nonexistent OutputBucketName still has to answer the bucket-missing sentence (recorded in transcribe-medical-transcription-job-workflow#task#2).
Replay: transcribe-transcription-job-workflow#task#2 and transcribe-call-analytics-job-workflow#task#2.
