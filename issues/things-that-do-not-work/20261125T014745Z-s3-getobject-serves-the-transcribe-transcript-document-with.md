Issue-Source: trace-sweep-divergence
Issue-Trace: transcribe-polly-speech-transcribed-to-s3-transcript
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: transcribe-polly-speech-transcribed-to-s3-transcript
Issue-Shape-Id: com.amazonaws.transcribe#Transcribe
Issue-Key: s3|GetObject|2xx->2xx|$headers.content-type
Issue-Related: 20261112T121939Z-s3-serves-the-transcribe-transcript-document-with-an.md
Issue-Related: 20261124T020651Z-s3-transcript-document-served-by-getobject-carries-an-empty.md
Issue-Commit: 502803de4e187f938d0d7a2d9ca81419f7361d9f

# s3 GetObject serves the Transcribe transcript document with an empty transcript, no items/audio_segments and no JSON content-type or checksum header

> _Note: this issue was identified against the code base at commit `502803de4e187f938d0d7a2d9ca81419f7361d9f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetObject

## Description
1 cause.

Cause 1 of 1: GetObject on the transcript document that a COMPLETED transcription job reports in Transcript.TranscriptFileUri answers 200, but the object served holds an empty transcript string, none of the per-token structure, and no segment breakdown, and it is served without the JSON content type or a body checksum header. Fixture transcribe-polly-speech-transcribed-to-s3-transcript, step task#6.

AWS: Smithy (s3, GetObject) — GET /{Bucket}/{Key+}, success code 200, output Body (httpPayload, streaming blob) plus the object's stored metadata headers, including ContentType -> Content-Type and ChecksumCRC64NVME -> x-amz-checksum-crc64nvme (httpChecksum responseAlgorithms list CRC64NVME first); modelled errors NoSuchKey (404) and InvalidObjectState (403). S3's own contract is therefore that an existing key is answered 200 with exactly the stored bytes and the headers recorded for the object at write time. The SCHEMA of the transcript document itself is recorded behavior only — it is not modelled in the S3 model. The recording shows the document Amazon Transcribe writes to the job's output location: an envelope (jobName, accountId, status "COMPLETED") plus `results` holding (a) transcripts[0].transcript, the full spoken text of the processed media as one string, (b) items[], one entry per token in order with id, type ("pronunciation" or "punctuation"), alternatives[{confidence, content}] and, for pronunciations, start_time/end_time as second-valued strings, and (c) audio_segments[], the segment breakdown over those items; the object is served as a JSON document with a checksum header matching the bytes. The particular words, confidence values, times, bucket, key, job name and account id in the recording are this run's values and are NOT the requirement: the requirement is that the transcript text is the speech contained in the media the job processed, that items and audio_segments are present and consistent with that text, and that the object carries the JSON content type plus a checksum computed over the bytes actually served.

Emulator: status matches (200) but the body diverges — $.results.transcripts[0].transcript is '' where the spoken text is expected; $.results.items[0] .. $.results.items[9] are MISSING entirely (got None) rather than present-and-empty; $.results.audio_segments is missing; and the cluster's divergence key also names $headers.content-type and $headers.x-amz-checksum-crc64nvme, i.e. the stored object is served with neither the JSON content type nor the CRC64NVME checksum header. This is deliberate in the current design rather than an accident: the transcribe state module header (services/service-com-amazonaws-transcribe-transcribe/src/state.rs, lines ~41-60) states the output document is stored at the reported location "with an EMPTY transcript and no caption, because no engine read the audio" ([`transcript_document`], [`subtitle_document`], stored via [`store_job_outputs`]).

Why: a client running the canonical workflow — put the audio, StartTranscriptionJob, poll GetTranscriptionJob to COMPLETED, fetch Transcript.TranscriptFileUri — receives a successful 200 that yields none of the spoken words, so every downstream consumer (keyword extraction, captioning, search indexing, diarization) silently gets nothing instead of failing loudly, with no status or error code to key on. Worse than the empty string: a client that walks results.items or results.audio_segments (the normal way to read word timings, confidences and segments) indexes members that are ABSENT rather than empty and fails with a key/None error, where an AWS-shaped empty result still carries both arrays. A consumer that dispatches on Content-Type or verifies x-amz-checksum-crc64nvme against the body also mis-handles the response, because neither header is served.

Fix: not GetObject's read path, which correctly serves whatever bytes were stored; the WRITE side is the defect. The transcript-document builder on the transcribe side (the transcript_document / store_job_outputs path reached from the transcription-job lifecycle in services/service-com-amazonaws-transcribe-transcribe/src/state.rs) must render the words of the media the job pointed at into results.transcripts[0].transcript and emit results.items and results.audio_segments consistent with that text, and must store the object with the JSON content type and a checksum over the bytes it writes so S3's object serializer emits Content-Type and x-amz-checksum-crc64nvme. The same applies to the redacted-transcript and subtitle documents built alongside it (subtitle_document).

Related but NOT covering this cluster (the replay reports owned_by empty for it, so no open issue carries its replay key): open issue 20261124T020651Z-s3-transcript-document-served-by-getobject-carries-an-empty.md and open issue 20261112T121939Z-s3-serves-the-transcribe-transcript-document-with-an.md describe the same empty document on the same fixture/step but each carries only a PART of this cluster's key list (the former only $headers.x-amz-checksum-crc64nvme, the latter only the three body keys), so the divergence is still reported unowned; open behavioral issue 20261118T015953Z-transcribe-a-completed-transcription-job-serves-a-transcript-document.md records the same root cause from the behavioral corpus under service transcribe / StartTranscriptionJob.

Traces: transcribe-polly-speech-transcribed-to-s3-transcript#task#6.

## Evidence
transcribe-polly-speech-transcribed-to-s3-transcript#task#6, GetObject, service s3, expected status 200 and got 200, verdict mismatch; divergence key service=s3|GetObject|status=200->200|keys=$.results.audio_segments,$.results.items[0..9],$.results.transcripts[0].transcript,$headers.content-type,$headers.x-amz-checksum-crc64nvme.

Reported diffs:
- $.results.transcripts[0].transcript: value_mismatch — expected the spoken text of the media ('The quick brown fox jumps over the lazy dog.' in this recording), got ''.
- $.results.items[0] .. $.results.items[9]: missing (got None) — expected one entry per token, e.g. {'id': 0, 'type': 'pronunciation', 'alternatives': [{'confidence': '0.971', 'content': 'The'}], 'start_time': '0.04', 'end_time': '0.24'} through a trailing punctuation entry {'id': 9, 'type': 'punctuation', 'alternatives': [{'confidence': '0.0', 'content': '.'}]}.
- $.results.audio_segments: missing (got None) — expected the segment breakdown over those items.
- $headers.content-type and $headers.x-amz-checksum-crc64nvme: diverging — the object is served without the JSON content type and without the CRC64NVME checksum header.

Fixed vs variable: the fixed requirement is the document's shape (envelope jobName/accountId/status plus results.transcripts[0].transcript, results.items[] with id/type/alternatives[{confidence,content}]/start_time/end_time, results.audio_segments[]) and the two response headers. The specific words, confidence numbers, start/end times, bucket, key, job name and account id in the recording are per-run values from the request and the emulator's state and are not themselves requirements.

## Steps to Reproduce
1. Create a bucket and PutObject the audio into it.
2. Call StartTranscriptionJob with Media.MediaFileUri pointing at that object and an output location (OutputBucketName/OutputKey).
3. Poll GetTranscriptionJob until TranscriptionJobStatus is COMPLETED and read Transcript.TranscriptFileUri (transcripts/<job-name>.json).
4. GetObject that bucket/key.
5. Observed: 200 with {'jobName': ..., 'accountId': ..., 'status': 'COMPLETED', 'results': {'transcripts': [{'transcript': ''}]}} — the transcript string is empty and 'items' and 'audio_segments' are absent — and the response carries neither the JSON Content-Type nor x-amz-checksum-crc64nvme. Expected: 200 with the transcript of the submitted media plus the items and audio_segments arrays consistent with it, under the JSON content type and a checksum header matching the served bytes.
Replay: transcribe-polly-speech-transcribed-to-s3-transcript#task#6.
