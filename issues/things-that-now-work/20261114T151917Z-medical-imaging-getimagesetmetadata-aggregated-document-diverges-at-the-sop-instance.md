Issue-Source: trace-sweep-divergence
Issue-Trace: medical-imaging-dicom-import-job-yields-searchable-image-set
Issue-Step: task#12
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: medical-imaging-dicom-import-job-yields-searchable-image-set
Issue-Shape-Id: com.amazonaws.medicalimaging#AHIGatewayService
Issue-Key: medical-imaging|GetImageSetMetadata|2xx->2xx|$.Study.Series.1.2.826.0.1.3680043.10.511.1.3.Instances.1.2.826.0.1.3680043.10.511.1.1.ChecksumType
Issue-Commit: f4df73eda0f97cfa6904b2b17c477411feb13853

# GetImageSetMetadata aggregated document diverges at the SOP-instance ChecksumType member

> _Note: this issue was identified against the code base at commit `f4df73eda0f97cfa6904b2b17c477411feb13853`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetImageSetMetadata

## Description
Cause 1 of 1: the aggregated DICOM metadata document that GetImageSetMetadata serves for an image set created by a completed DICOM import carries a per-SOP-instance `ChecksumType` member that does not agree with the document AWS HealthImaging serves at the same path.

AWS: recorded behavior only. The response member is `imageSetMetadataBlob`, a required streaming blob documented only as "The blob containing the aggregated metadata information for the image set" (Smithy: GetImageSetMetadataResponse.imageSetMetadataBlob, httpPayload, streaming blob; contentType/contentEncoding are HTTP headers) — the model says nothing about the JSON inside it, so the recorded document is the only authority for its shape. HTTP status is 200 on both sides and every other key of the document matched on replay, so the recorded AWS document and the emulator's agree on the whole Patient -> Study -> Series -> Instances hierarchy, the instance DICOM attributes and the ImageFrames entries (including `PixelDataChecksumFromBaseToFullResolution` with its Width/Height/Checksum entries, FrameSizeInBytes and the Min/MaxPixelValue pair). They disagree at exactly one leaf: `Study.Series.<SeriesInstanceUID>.Instances.<SOPInstanceUID>.ChecksumType`, i.e. a member sitting at the INSTANCE level beside `ImageFrames`. The recorded document does not carry the emulator's value there.

Emulator: it unconditionally adds an instance-level `ChecksumType` whenever any frame of that instance produced a checksum entry — services/service-com-amazonaws-medicalimaging-ahigatewayservice/src/ops/get_image_set_metadata.rs lines 336-343 insert `"ChecksumType": "CRC32"` into the instance object (the constant is PIXEL_DATA_CHECKSUM_TYPE = "CRC32" in src/state/dicom_instance.rs line 31, chosen because the emulator computes the frame checksum with crc32fast). The replay reports the leaf as `$.Study.Series.1.2.826.0.1.3680043.10.511.1.3.Instances.1.2.826.0.1.3680043.10.511.1.1.ChecksumType: value_mismatch`; the detail renders both sides as `None` because the comparator's dotted JSON path cannot address DICOM UID keys (which themselves contain dots), so the rendered values are not usable — the diverging leaf path is.

Why: a client that parses the aggregated metadata document to learn how the pixel-data checksums it reads were computed is given a member AWS does not place there, or a value AWS does not use. Code that keys off the presence of that member (verifying a frame's checksum, or round-tripping the document) behaves differently against the emulator than against AWS, and a client that re-serializes the document it read back into a comparison or a hash gets a mismatch for an extra member that was never in the service's output.

Fix: the metadata-document builder for this operation — the instance-level object assembled in src/ops/get_image_set_metadata.rs (the `checksummed` branch that writes ChecksumType beside ImageFrames). The document must carry at that path exactly what the recorded aggregated metadata document carries and nothing else; if the checksum algorithm is to be named at all it belongs where the recording places it, not at the instance level by the emulator's own choice. No status, header or error behavior is involved — contentType/contentEncoding and the 200 both matched.

Note on existing issues: the open issue "A COMPLETED DICOM import job creates no image sets..." (20261024T071454Z-medical-imaging-startdicomimportjob-wrong-behavior.md) concerns image sets not existing at all; the import in this fixture does yield a searchable image set and the metadata read answers 200, so that issue does not cover this document-content divergence. No other open medical-imaging issue carries this replay key.

## Evidence
Fixture medical-imaging-dicom-import-job-yields-searchable-image-set, step task#12, operation GetImageSetMetadata, verdict mismatch.
Expected status 200, actual status 200 — the divergence is in the response body (the gzipped aggregated metadata JSON).
Replay detail, verbatim: "$.Study.Series.1.2.826.0.1.3680043.10.511.1.3.Instances.1.2.826.0.1.3680043.10.511.1.1.ChecksumType: value_mismatch (expected None, got None)" — divergence_key "service=medical-imaging|GetImageSetMetadata|status=200->200|keys=$.Study.Series.1.2.826.0.1.3680043.10.511.1.3.Instances.1.2.826.0.1.3680043.10.511.1.1.ChecksumType". The two `None` renderings are the comparator failing to re-address a path whose key segments are dotted DICOM UIDs; the reported leaf is the finding. The UIDs above are this recording's identifiers and are not part of the requirement — the requirement is about the `ChecksumType` member at the instance level of every instance.
Emulator side: src/ops/get_image_set_metadata.rs:336-343 writes `instance["ChecksumType"] = "CRC32"` whenever the instance has at least one frame checksum; src/state/dicom_instance.rs:31 defines PIXEL_DATA_CHECKSUM_TYPE = "CRC32".
Model side: smithy_query(query="operation", operation="GetImageSetMetadata") — output is imageSetMetadataBlob (required, streaming blob, httpPayload) plus contentType/contentEncoding headers; the document's internal members are unmodelled, hence "recorded behavior only".
Every other step of the fixture passed (36/38 steps passed corpus-wide), so nothing else about this image set's metadata diverges.

## Steps to Reproduce
1. Replay fixture medical-imaging-dicom-import-job-yields-searchable-image-set against the emulator (replay_wire_corpus) and look at step task#12.
2. Equivalently, by hand: POST /datastore to create a data store and poll GetDatastore until ACTIVE; POST /startDICOMImportJob/datastore/{datastoreId} pointing at a DICOM P10 input prefix and poll GetDICOMImportJob until COMPLETED; SearchImageSets to obtain the image set the import produced.
3. POST /datastore/{datastoreId}/imageSet/{imageSetId}/getImageSetMetadata, gunzip the payload and parse the JSON.
4. Inspect Study.Series.<SeriesInstanceUID>.Instances.<SOPInstanceUID>: the emulator's instance object carries a "ChecksumType": "CRC32" member beside "ImageFrames"; the recorded AWS document for the same image set does not carry that value at that path.
