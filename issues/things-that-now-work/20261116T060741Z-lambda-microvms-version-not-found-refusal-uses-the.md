Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda-microvms.delete-microvm-image-version-with-multiple-versions
Issue-Shape-Id: com.amazonaws.lambdamicrovms#LambdaMicrovms
Issue-Key: lambda-microvms.delete-microvm-image-version-with-multiple-versions|run#23|AssertionError|assert error.get("Message") == (
Issue-Commit: ae7bc1f94299e50ef29fa361e521f7e3f1efe785

# lambda-microvms version-not-found refusal uses the wrong message form: "MicroVM image <arn> has no version <v>" instead of AWS's "MicroVM image version not found: <arn>, version: <v>"

> _Note: this issue was identified against the code base at commit `ae7bc1f94299e50ef29fa361e521f7e3f1efe785`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateMicrovmImageVersion

## Description
Cause 1 of 1: the shared "image version not found" message helper produces a message form AWS does not use, so every version-addressed refusal carries the wrong text.

AWS: UpdateMicrovmImageVersion is modeled with input members imageIdentifier (httpLabel, required, MicrovmImageIdentifier, @length 1..256) and imageVersion (httpLabel, required, NonBlankString, @length 1..2048, pattern ^[^\s]+$), bound to PATCH /2025-09-09/microvm-images/{imageIdentifier}/versions/{imageVersion}, and declares ResourceNotFoundException (HTTP 404) — "The specified resource does not exist." (Smithy: UpdateMicrovmImageVersion.errors ResourceNotFoundException httpError 404; ResourceNotFoundException.message required, resourceType/resourceId optional.) The model does not state the message text, so for the text this is recorded behavior only: addressing an imageVersion the image does not hold (here, a version that DeleteMicrovmImageVersion has already removed) is refused with HTTP 404, error code ResourceNotFoundException, and the message

    MicroVM image version not found: <imageArn>, version: <imageVersion>

Fixed text: "MicroVM image version not found: ", then ", version: ". Variable parts: <imageArn> is the resolved image's own ARN (the single identity of the record, whichever form — ARN or id — the request addressed it by) and <imageVersion> is the imageVersion value taken verbatim from the request. The recorded ARN and version string of any single run only fill that template; they are not part of it.

Emulator: the status (404) and the code (ResourceNotFoundException) are right, but the message is a different sentence entirely —

    MicroVM image <imageArn> has no version <imageVersion>

(replay: 'MicroVM image arn:aws:lambda:us-west-2:000000000000:microvm-image:sim2real-mvm-img-4fd5b95fe0b1 has no version 1.0', Code 'ResourceNotFoundException'). It is not a per-run-value difference: the fixed text differs ("MicroVM image " + " has no version " vs "MicroVM image version not found: " + ", version: "), the two variable parts are in the opposite grammatical roles, and the ", version: " separator is absent.

Why: a client that distinguishes a missing IMAGE from a missing VERSION by the refusal text — both are the same code (ResourceNotFoundException) and the same status (404), so the message is the only discriminator the service offers — mis-classifies the failure. Operators' log greps, retry predicates and test assertions keyed on AWS's sentence never match, and a caller walking a version list it raced against a delete cannot report which version is gone.

Fix: the shared message helper crate::state::image_version_not_found_message — not the handler, since the handler already routes the version miss to the modeled 404 correctly. Rewording it there fixes every version-addressed refusal at once (UpdateMicrovmImageVersion, and any other op that resolves a version through the same helper), so the fix must keep the two substituted values the resolved image's own ARN and the request's imageVersion rather than echoing the identifier the request used.

Related but NOT covering this: open issue 20261109T152104Z-lambda-microvms-2-net-new-root-causes-the.md concerns the resource-scoped AccessDeniedException message form, and 20261108T125129Z-lambda-microvms-image-version-and-build-read-paths.md concerns dropped members on read paths; neither carries this check or the ResourceNotFoundException wording.

Traces: lambda-microvms.delete-microvm-image-version-with-multiple-versions. The check first proves the happy path works — two versions exist, DeleteMicrovmImageVersion on the non-last version echoes it with state DELETING/DELETED, the version becomes unreachable, the surviving version and the image are untouched — and then fails only on the refusal text of the follow-up UpdateMicrovmImageVersion against the deleted version.

## Evidence
Check: lambda-microvms.delete-microvm-image-version-with-multiple-versions ([mismatch]).

Failing assertion (check line 82):
    assert error.get("Message") == (
        f"MicroVM image version not found: {image_arn}, version: {old_version}"
    ), error

AssertionError payload (the emulator's error):
    {'Message': 'MicroVM image arn:aws:lambda:us-west-2:000000000000:microvm-image:sim2real-mvm-img-4fd5b95fe0b1 has no version 1.0',
     'Code': 'ResourceNotFoundException'}

Expected (AWS, as the recording words it, template form):
    Code:    ResourceNotFoundException   (matches)
    Status:  404                         (matches)
    Message: "MicroVM image version not found: <imageArn>, version: <imageVersion>"

Actual (emulator, template form):
    Message: "MicroVM image <imageArn> has no version <imageVersion>"

The preceding assertion on error.get("Code") == "ResourceNotFoundException" passed, so only the message text diverges.

Source of the diverging text: services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/src/ops/update_microvm_image_version.rs lines 255-261 route the version miss to the modeled 404 via crate::state::image_version_not_found_message(&image.image_arn, &input.image_version); that helper renders the wrong sentence.

## Steps to Reproduce
1. Provision a MicroVM image and wait for image state CREATED and its first version state SUCCESSFUL.
2. Call UpdateMicrovmImage on it to produce a second, distinct imageVersion; wait for that version to reach SUCCESSFUL. ListMicrovmImageVersions now lists both versions, so the next step is not the last-version case.
3. Call DeleteMicrovmImageVersion(imageIdentifier=<imageArn>, imageVersion=<first version>) and wait until GetMicrovmImageVersion for that version answers ResourceNotFoundException or state DELETED.
4. Call UpdateMicrovmImageVersion(imageIdentifier=<imageArn>, imageVersion=<first version>, status="INACTIVE").
5. Observe the ClientError: Code is ResourceNotFoundException (HTTP 404) as AWS answers, but Message is "MicroVM image <imageArn> has no version <imageVersion>" instead of AWS's "MicroVM image version not found: <imageArn>, version: <imageVersion>".

Replay shortcut: re-run check lambda-microvms.delete-microvm-image-version-with-multiple-versions.
