Issue-Source: trace-sweep-divergence
Issue-Trace: transcribe-polly-speech-transcribed-to-s3-transcript
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: transcribe-polly-speech-transcribed-to-s3-transcript
Issue-Shape-Id: com.amazonaws.transcribe#Transcribe
Issue-Key: s3|GetObject|2xx->2xx|$headers.x-amz-checksum-crc64nvme
Issue-Related: 20261112T121939Z-s3-serves-the-transcribe-transcript-document-with-an.md
Issue-Related: 20261118T102814Z-s3-getobject-serves-content-type-binary-octet-stream.md
Issue-Commit: 428a0a660ec8f21315f2c255010ca5b9d3af4eb5

# Transcript document served by GetObject carries an empty transcript and omits items/audio_segments and the JSON content-type and checksum headers

> _Note: this issue was identified against the code base at commit `428a0a660ec8f21315f2c255010ca5b9d3af4eb5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetObject

## Description
1 cause.

Cause 1 of 1: GetObject on the transcript document that a COMPLETED transcription job reports in Transcript.TranscriptFileUri answers 200, but the object it serves holds an empty transcript string and none of the per-token structure, and it is served without the JSON content type or a body checksum header. Fixture transcribe-polly-speech-transcribed-to-s3-transcript, step task#6.

AWS: Smithy (s3, GetObject): GET /{Bucket}/{Key+}, success code 200, output Body (httpPayload, streaming blob) plus the object's metadata headers — ContentType -> Content-Type, ChecksumCRC64NVME -> x-amz-checksum-crc64nvme (httpChecksum responseAlgorithms include CRC64NVME), errors NoSuchKey (404) and InvalidObjectState (403). S3's own contract is therefore that an existing key is answered 200 with exactly the stored bytes and the headers recorded for the object at write time. The transcript document's SCHEMA is recorded behavior only — it is not modelled in the S3 model. The recording shows the document Transcribe writes to the job's output location: an envelope (jobName, accountId, status "COMPLETED") plus `results` holding (a) transcripts[0].transcript, the full spoken text of the media as one string, (b) items[], one entry per token in order with id, type ("pronunciation" or "punctuation"), alternatives[{confidence, content}] and, for pronunciations, start_time/end_time as second-valued strings, and (c) audio_segments[], the segment breakdown over those items. The object is served as a JSON document with a checksum header matching the bytes. The transcript words, confidence values, times, bucket, key, job name and account id in the recording are this run's values and are not the requirement; the requirement is that the transcript text is the speech contained in the media the job processed and that items/audio_segments are present and consistent with that text, under the JSON content type and a checksum computed over the bytes actually served.

Emulator: 200 (status matches), but $.results.transcripts[0].transcript is '' where the spoken text is expected; $.results.items[0] .. $.results.items[9] are missing entirely (got None) rather than present-and-empty; $.results.audio_segments is missing; and the cluster's diverging-key list also names $headers.content-type and $headers.x-amz-checksum-crc64nvme, i.e. the stored object is served without the JSON content type and without the CRC64NVME checksum header. This is deliberate in the current design rather than an accident: the transcribe state module header (services/service-com-amazonaws-transcribe-transcribe/src/state.rs, lines ~41-58) states the output document is stored at the reported location with an EMPTY transcript and no caption, "because no engine read the audio".

Why: a client performing the canonical workflow — upload audio, StartTranscriptionJob, poll GetTranscriptionJob to COMPLETED, fetch Transcript.TranscriptFileUri — gets a successful 200 that yields none of the spoken words, so every downstream consumer (keyword extraction, captioning, search indexing, diarization) silently receives nothing instead of failing loudly, with no status or error code to key on. Worse than the empty string: a client that walks results.items or results.audio_segments (the normal way to read word timings, confidences or segments) indexes members that are ABSENT rather than empty and fails with a key/None error; an AWS-shaped empty result still carries both arrays. A consumer that dispatches on Content-Type or verifies x-amz-checksum-crc64nvme against the body also mis-handles the response, because neither header is served.

Fix: not GetObject's read path, which correctly serves whatever bytes were stored. The write side is the defect: the transcript-document builder on the transcribe side (the transcript_document / store_job_outputs path reached from the transcription-job lifecycle in services/service-com-amazonaws-transcribe-transcribe/src/state.rs) must render the words of the media the job pointed at into results.transcripts[0].transcript and emit results.items and results.audio_segments consistent with that text, and must store the object with the JSON content type and a checksum over the bytes it writes so S3's serializer emits Content-Type and x-amz-checksum-crc64nvme. The same applies to the redacted-transcript and subtitle documents built alongside it (subtitle_document).

Related but NOT covering this (the replay reports this cluster as owned by no open issue): open issue 20261112T121939Z-s3-serves-the-transcribe-transcript-document-with-an.md describes the same empty document on the same fixture/step but does not carry this cluster's replay key (its key list omits $headers.content-type and $headers.x-amz-checksum-crc64nvme), so the divergence is still reported unowned; open behavioral issue 20261118T015953Z-transcribe-a-completed-transcription-job-serves-a-transcript-document.md records the same root cause from the behavioral corpus under service transcribe / StartTranscriptionJob.

Traces: transcribe-polly-speech-transcribed-to-s3-transcript#task#6.

## Evidence
transcribe-polly-speech-transcribed-to-s3-transcript#task#6, GetObject, service s3, expected 200 and got 200, verdict mismatch.

Diverging keys reported by the replay: $.results.transcripts[0].transcript value_mismatch (expected the spoken text of the media — 'The quick brown fox jumps over the lazy dog.' in this recording — got ''); $.results.items[0] .. $.results.items[9] all missing (expected per-token entries such as {'id': 0, 'type': 'pronunciation', 'alternatives': [{'confidence': '0.971', 'content': 'The'}], 'start_time': '0.04', 'end_time': '0.24'} and a trailing punctuation entry {'id': 9, 'type': 'punctuation', 'alternatives': [{'confidence': '0.0', 'content': '.'}]}, got None); $.results.audio_segments missing (got None); plus $headers.content-type and $headers.x-amz-checksum-crc64nvme in the cluster's divergence key (service=s3|GetObject|status=200->200).

The particular words, confidence values, times, bucket/key, job name and account id are this run's values and are not the requirement; the requirement is a transcript of the media the job processed with the items and audio_segments arrays present and consistent with it, served with the JSON content type and a checksum header matching the served bytes.

## Steps to Reproduce
1. Create a bucket and put the audio object into it; call StartTranscriptionJob with Media.MediaFileUri pointing at it and an output location.
2. Poll GetTranscriptionJob until TranscriptionJobStatus is COMPLETED.
3. GetObject the bucket/key named by the job's Transcript.TranscriptFileUri (transcripts/<job-name>.json).
4. The emulator answers 200 with {'jobName': ..., 'accountId': ..., 'status': 'COMPLETED', 'results': {'transcripts': [{'transcript': ''}]}} — the transcript string is empty and 'items' and 'audio_segments' are absent — and the response carries neither the JSON Content-Type nor x-amz-checksum-crc64nvme, where AWS serves the transcript of the submitted media together with both arrays under the JSON content type and a matching checksum header.
Replay: transcribe-polly-speech-transcribed-to-s3-transcript#task#6.
