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
Issue-Commit: 127e90fa938572074a2c08d828ebb8de7f0d4317

# sagemaker: registration read-backs report a reference-derived ImageDigest instead of the referenced image's digest

> _Note: this issue was identified against the code base at commit `127e90fa938572074a2c08d828ebb8de7f0d4317`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAlgorithm

## Description
Two net-new wire divergences, both in SageMaker's registration read-backs, share ONE root cause: the digest this emulator stores for a container-image reference is a SHA-256 over the reference TEXT (crate::state::image_digest::derived_image_digest), not the content digest of the image the reference names. Both resources therefore replay a well-formed but wrong sha256: value, and both report the SAME wrong value because both fixtures name the same image path. The shared slice documents the intended order itself (pin the digest in the path, else ask the Amazon ECR registry mounted in the same engine, else fall back to the emulator's own derived identifier); in practice the resolution reaches the fallback for a reference the registry can answer for, so the fallback becomes the normal answer.

Cause 1 of 2: DescribeAlgorithm reports the wrong $.TrainingSpecification.TrainingImageDigest (fixture sagemaker-algorithm-duplicate-and-missing-errors, step task#4; also sagemaker-algorithm-lifecycle#task#3).
  AWS: Smithy: TrainingSpecification.TrainingImageDigest targets ImageDigest, @length 0..72, @pattern ^[Ss][Hh][Aa]256:[0-9a-fA-F]{64}$, documented as "An MD5 hash of the training algorithm that identifies the Docker image used for training", sitting beside TrainingSpecification.TrainingImage, "The Amazon ECR registry path of the Docker image that contains the training algorithm". The digest is a property of the IMAGE the path names, so DescribeAlgorithm replays either the digest the CreateAlgorithm request carried or, when the request carried none, the digest the registry holding that image reports for it — the same value that registry's own image reads report. Status 200, no error involved.
  Emulator: answers 200 and returns a digest that is neither: at the asserted step the recorded digest for the algorithm's training image differs from the one served, and the served value is the SHA-256 of the image-reference string.
  Why: a client that pins or verifies algorithm provenance by comparing TrainingImageDigest against the digest it pushed to (or read from) Amazon ECR sees a mismatch for an image it just registered, so promotion/approval gates that compare the two digests reject a correct registration, and a digest recorded from this read-back can never be used to re-address the image.
  Fix: the digest-resolution step on the write path for CreateAlgorithm — crate::state::image_digest::resolved_image_digest / materialise_training_specification: when the request supplies no digest, the registry lookup must answer for the reference before the derived fallback is used; the fallback must remain the last resort, not the usual outcome.
  Hypothesis: registry_image_digest returns None for the reference spelling these fixtures use (tag form and/or the announced-registry host rewrite), so resolution falls straight through to derived_image_digest.

Cause 2 of 2: DescribeModelPackage reports the wrong $.InferenceSpecification.Containers[0].ImageDigest (fixture sagemaker-model-package-create-and-verify, step task#3; also sagemaker-model-package-metric-numeric-precision-preserved#task#4).
  AWS: Smithy: ModelPackageContainerDefinition.ImageDigest targets ImageDigest, @length 0..72, @pattern ^[Ss][Hh][Aa]256:[0-9a-fA-F]{64}$, beside ModelPackageContainerDefinition.Image, "The Amazon Elastic Container Registry (Amazon ECR) path where inference code is stored" (registry/repository[:tag] and registry/repository[@digest] both supported). Same rule: the member reports the referenced image's own digest, so DescribeModelPackage replays the digest supplied at CreateModelPackage or the registry's digest for that image. Status 200.
  Emulator: answers 200 with the reference-derived digest for the inference container instead of the referenced image's digest — the same wrong value as Cause 1, since the same image path is referenced.
  Why: model-package approval and deployment flows compare a package's container ImageDigest with the image in ECR before approving or deploying; an unrelated digest makes every such comparison fail, and a digest that addresses no image cannot be used to deploy the pinned container.
  Fix: the same resolution step on the CreateModelPackage write path — materialise_inference_specification / materialise_additional_inference_specifications via resolved_image_digest; one correction in that slice should close both causes, but each must be re-replayed.

No open issue covers this: the SageMaker digest/read-back issues already filed cover DescribeDomain's AppNetworkAccess, training-job/endpoint timestamp and metric-definition losses, and ECR's ImageNotFoundException placeholder digest — none of them covers the ImageDigest a registration read-back reports.

Traces: sagemaker-algorithm-duplicate-and-missing-errors#task#4, sagemaker-algorithm-lifecycle#task#3, sagemaker-model-package-create-and-verify#task#3, sagemaker-model-package-metric-numeric-precision-preserved#task#4. Code: services/service-com-amazonaws-sagemaker-sagemaker/src/state/image_digest.rs lines 60-63 (derived_image_digest), 137-176 (registry_image_digest / resolved_image_digest), 233-291 (materialise_container / materialise_inference_specification / materialise_training_specification); services/service-com-amazonaws-sagemaker-sagemaker/src/ops/create_algorithm.rs lines 182-191.

## Evidence
Cause 1 — DescribeAlgorithm, expected 200 / actual 200, body diverges:
  $.TrainingSpecification.TrainingImageDigest: value_mismatch (expected 'sha256:8ebddbc1bfbd56c897023d25d19aefde6606d876ddc5abe295d0628f3144aa9e', got 'sha256:a9a77d475defd1d437e0fe8ffb39a703f0c3c7f5a27a504eba82f2a5926b166a')
  divergence_key: service=sagemaker|DescribeAlgorithm|status=200->200|keys=$.TrainingSpecification.TrainingImageDigest
  affected: sagemaker-algorithm-duplicate-and-missing-errors#task#4, sagemaker-algorithm-lifecycle#task#3

Cause 2 — DescribeModelPackage, expected 200 / actual 200, body diverges:
  $.InferenceSpecification.Containers[0].ImageDigest: value_mismatch (expected 'sha256:8ebddbc1bfbd56c897023d25d19aefde6606d876ddc5abe295d0628f3144aa9e', got 'sha256:a9a77d475defd1d437e0fe8ffb39a703f0c3c7f5a27a504eba82f2a5926b166a')
  divergence_key: service=sagemaker|DescribeModelPackage|status=200->200|keys=$.InferenceSpecification.Containers[0].ImageDigest
  affected: sagemaker-model-package-create-and-verify#task#3, sagemaker-model-package-metric-numeric-precision-preserved#task#4

The two literal hex values above are this run's example values, not requirements. The requirement is the RULE: the member carries the digest the request supplied, else the content digest the registry holding the referenced image reports for it (the same value that registry's own image reads return) — never a value computed from the reference string. Both responses are HTTP 200 with no error code, so only the field value diverges; the shape and the @pattern are satisfied by the wrong value, which is why no validation catches it.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus), trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture sagemaker-algorithm-duplicate-and-missing-errors to step task#4 — CreateAlgorithm registers an algorithm whose TrainingSpecification.TrainingImage names a container image held in the engine's Amazon ECR registry, then DescribeAlgorithm reads it back. Compare $.TrainingSpecification.TrainingImageDigest with the digest that registry reports for the same image (ECR DescribeImages / BatchGetImage on the referenced repository and tag): the emulator's value is the SHA-256 of the reference string instead. Same at sagemaker-algorithm-lifecycle#task#3.
3. Cause 2: replay fixture sagemaker-model-package-create-and-verify to step task#3 — CreateModelPackage registers a package whose InferenceSpecification.Containers[0].Image names that image, then DescribeModelPackage reads it back. $.InferenceSpecification.Containers[0].ImageDigest shows the same reference-derived value rather than the image's digest. Same at sagemaker-model-package-metric-numeric-precision-preserved#task#4.
4. Both steps return HTTP 200; the divergence is only the digest value, so assert on it directly.
