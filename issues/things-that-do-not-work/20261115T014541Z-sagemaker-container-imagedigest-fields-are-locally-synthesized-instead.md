Issue-Source: trace-sweep-divergence
Issue-Trace: sagemaker-algorithm-duplicate-and-missing-errors
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sagemaker-algorithm-duplicate-and-missing-errors sagemaker-algorithm-lifecycle sagemaker-model-package-create-and-verify sagemaker-model-package-metric-numeric-precision-preserved
Issue-Shape-Id: com.amazonaws.sagemaker#SageMaker
Issue-Key: sagemaker|DescribeAlgorithm|2xx->2xx|$.TrainingSpecification.TrainingImageDigest
Issue-Key: sagemaker|DescribeModelPackage|2xx->2xx|$.InferenceSpecification.Containers[*].ImageDigest
Issue-Commit: 2c9e097b66ada646a313dc09bbc30cfe7dbc4d7b

# sagemaker: container ImageDigest fields are locally synthesized instead of carrying the registered digest

> _Note: this issue was identified against the code base at commit `2c9e097b66ada646a313dc09bbc30cfe7dbc4d7b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAlgorithm

## Description
Two net-new causes, one shared fault: wherever a SageMaker resource carries a Docker image digest, the emulator returns a digest it derives locally rather than the digest the resource was registered with, so every read-back of that resource reports a `sha256:` value AWS would never return. Both causes are read paths on resources created earlier in the same fixture, and the diverging value is byte-identical across unrelated fixtures (`sha256:a9a77d475defd1d437e0fe8ffb39a703f0c3c7f5a27a504eba82f2a5926b166a`), i.e. it is a deterministic local derivation, not a per-run value.

Cause 1 of 2: DescribeAlgorithm returns the wrong TrainingSpecification.TrainingImageDigest (fixture sagemaker-algorithm-duplicate-and-missing-errors, step task#4; same cluster also sagemaker-algorithm-lifecycle#task#3).
AWS: Smithy: TrainingSpecification.TrainingImageDigest -> ImageDigest, @length max 72, @pattern ^[Ss][Hh][Aa]256:[0-9a-fA-F]{64}$, documented as "An MD5 hash of the training algorithm that identifies the Docker image used for training." It is an input member of CreateAlgorithm's TrainingSpecification, so the value a DescribeAlgorithm read returns is the digest the algorithm was registered with — AWS does not recompute or substitute it. Status 200, no error. Recorded behavior: the create registered the digest and DescribeAlgorithm echoed it back unchanged, identical in both affected fixtures.
Emulator: answers 200 with a different, locally derived digest at $.TrainingSpecification.TrainingImageDigest — same shape (sha256: + 64 hex), wrong value, stable across fixtures.
Why: a client that registered an algorithm against a pinned training image and then reads it back to verify or to re-pin a deployment sees a digest that does not match the one it supplied. Digest comparison is the whole point of the field, so the compare fails and the client concludes the image drifted; anything that forwards the digest on to a training job pins a non-existent image.
Fix: the CreateAlgorithm handler's state (it must persist the request's TrainingSpecification.TrainingImageDigest verbatim) and the DescribeAlgorithm serializer (it must emit the persisted value rather than call a digest helper). Hypothesis: the digest is being computed from the image URI at read time, which is why it is identical for every resource pointing at the same image.
Traces: sagemaker-algorithm-duplicate-and-missing-errors#task#4, sagemaker-algorithm-lifecycle#task#3.

Cause 2 of 2: DescribeModelPackage returns the wrong InferenceSpecification.Containers[0].ImageDigest (fixture sagemaker-model-package-create-and-verify, step task#3; same cluster also sagemaker-model-package-metric-numeric-precision-preserved#task#4).
AWS: Smithy: ModelPackageContainerDefinition.ImageDigest -> ImageDigest, @length max 72, @pattern ^[Ss][Hh][Aa]256:[0-9a-fA-F]{64}$, documented as "An MD5 hash of the training algorithm that identifies the Docker image used for training." It is likewise an input member of CreateModelPackage's InferenceSpecification containers, so DescribeModelPackage returns the digest the package was registered with. Status 200, no error. Recorded behavior: the digest supplied at create is read back unchanged.
Emulator: answers 200 with the same locally derived digest instead of the registered one at $.InferenceSpecification.Containers[0].ImageDigest.
Why: model-package consumers use ImageDigest to assert that an approved package still references the exact inference image that was reviewed. A substituted digest makes that assertion fail for every package, and a deployment pipeline that pulls the container by digest resolves nothing.
Fix: the CreateModelPackage handler's state (persist each InferenceSpecification container's ImageDigest as given) and the DescribeModelPackage serializer (emit the persisted value); the same digest helper is shared with the DescribeAlgorithm path above, so both causes are one correction plus two read paths to stop recomputing.
Traces: sagemaker-model-package-create-and-verify#task#3, sagemaker-model-package-metric-numeric-precision-preserved#task#4.

Neither cause is covered by an existing open issue: the sagemaker Describe-response issues already open (20261102T105242Z-sagemaker-describe-responses-omit-members-present-in-the.md, 20261109T123745Z-sagemaker-describetrainingjob-and-describeendpoint-omit-response-members-the.md) are about response members the emulator OMITS, not about a present member carrying a wrong value, and they name neither of these operations.

## Evidence
Cause 1 — DescribeAlgorithm, expected status 200, actual status 200:
  $.TrainingSpecification.TrainingImageDigest: value_mismatch
    expected 'sha256:8ebddbc1bfbd56c897023d25d19aefde6606d876ddc5abe295d0628f3144aa9e'
    actual   'sha256:a9a77d475defd1d437e0fe8ffb39a703f0c3c7f5a27a504eba82f2a5926b166a'
  divergence_key service=sagemaker|DescribeAlgorithm|status=200->200|keys=$.TrainingSpecification.TrainingImageDigest
  affected: sagemaker-algorithm-duplicate-and-missing-errors#task#4, sagemaker-algorithm-lifecycle#task#3

Cause 2 — DescribeModelPackage, expected status 200, actual status 200:
  $.InferenceSpecification.Containers[0].ImageDigest: value_mismatch
    expected 'sha256:8ebddbc1bfbd56c897023d25d19aefde6606d876ddc5abe295d0628f3144aa9e'
    actual   'sha256:a9a77d475defd1d437e0fe8ffb39a703f0c3c7f5a27a504eba82f2a5926b166a'
  divergence_key service=sagemaker|DescribeModelPackage|status=200->200|keys=$.InferenceSpecification.Containers[0].ImageDigest
  affected: sagemaker-model-package-create-and-verify#task#3, sagemaker-model-package-metric-numeric-precision-preserved#task#4

The expected digest is the value the preceding create registered; the actual digest is identical in all four steps across four unrelated fixtures, so it is a deterministic local derivation and not a per-run artifact. The literal hex strings above are this corpus's values and fill the template; the requirement is only that the digest read back equals the digest registered, matching ^[Ss][Hh][Aa]256:[0-9a-fA-F]{64}$.

## Steps to Reproduce
1. Replay fixture sagemaker-algorithm-duplicate-and-missing-errors against the emulator and stop at step task#4: CreateAlgorithm registers an algorithm whose TrainingSpecification carries a TrainingImageDigest, then DescribeAlgorithm reads it back. Compare $.TrainingSpecification.TrainingImageDigest against the digest the create supplied — the emulator returns a different sha256 value. sagemaker-algorithm-lifecycle#task#3 reproduces the same step.
2. Replay fixture sagemaker-model-package-create-and-verify and stop at step task#3: CreateModelPackage registers an InferenceSpecification container with an ImageDigest, then DescribeModelPackage reads it back. Compare $.InferenceSpecification.Containers[0].ImageDigest against the create's value — again a different sha256. sagemaker-model-package-metric-numeric-precision-preserved#task#4 reproduces it.
3. Note that the digest the emulator returns is the same string in both fixtures even though the registered digests belong to different resources, confirming it is computed rather than stored.
